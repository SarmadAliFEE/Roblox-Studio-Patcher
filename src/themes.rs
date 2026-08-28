use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Result};
use serde_json::Value;

use crate::binary::{backup, find_matches, is_pe, kill_running_studio, resign, PatByte};
use crate::palette::ensure_palette_defaults;
use crate::Args;

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub const THEMES_DIR: &str = r"C:\Users\Public\rbxthemeset";
#[cfg(target_os = "macos")]
pub const THEMES_DIR: &str = "/Users/Shared/rbx-theme-set"; // gotta be exactly 27 bytes

#[cfg(target_os = "linux")]
static HOST_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Points the config directory at the wineprefix the target studio runs in. `THEMES_DIR`
#[cfg(target_os = "linux")]
pub fn adopt_prefix_of(target: &Path) -> Option<PathBuf> {
    let prefix: PathBuf = target.ancestors().find_map(|ancestor: &Path| {
        let prefixes: PathBuf = ancestor.join("prefixes");
        if !prefixes.is_dir() {
            return None;
        }
        let studio: PathBuf = prefixes.join("studio");
        if studio.is_dir() {
            return Some(studio);
        }
        fs::read_dir(&prefixes).ok()?.flatten().next().map(|entry| entry.path())
    })?;
    let dir: PathBuf = prefix.join("drive_c/users/Public/rbxthemeset");
    let _ = HOST_DIR.set(dir.clone());
    Some(dir)
}

#[cfg(not(target_os = "linux"))]
pub fn adopt_prefix_of(_target: &Path) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "linux")]
pub fn host_dir() -> PathBuf {
    HOST_DIR.get().cloned().unwrap_or_else(|| PathBuf::from("/var/tmp/rbxthemeset"))
}

#[cfg(not(target_os = "linux"))]
pub fn host_dir() -> PathBuf {
    PathBuf::from(THEMES_DIR)
}

/// The path baked into the binary, which studio itself resolves.
fn embedded_json_path(name: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("{THEMES_DIR}/{name}")
    } else {
        format!("{THEMES_DIR}\\{name}")
    }
}

pub fn dark_json_path() -> PathBuf {
    host_dir().join("FoundationDarkTheme.json")
}

pub fn light_json_path() -> PathBuf {
    host_dir().join("FoundationLightTheme.json")
}

pub fn editor_background_json_path() -> PathBuf {
    host_dir().join("EditorBackground.json")
}

pub fn window_transparency_json_path() -> PathBuf {
    host_dir().join("WindowTransparency.json")
}

const EDITOR_BACKGROUND_DEFAULTS: &str = concat!(
    "{\n",
    "    \"enabled\": true,\n",
    "    \"image\": \"\",\n",
    "    \"opacity\": 0.1\n",
    "}\n",
);

#[cfg(target_os = "windows")]
const WINDOW_TRANSPARENCY_DEFAULTS: &str = concat!(
    "{\n",
    "    \"enabled\": true,\n",
    "    \"opacity\": 1.0,\n",
    "    \"step\": 0.05,\n",
    "    \"minOpacity\": 0.2,\n",
    "    \"increaseHotkey\": \"alt+=\",\n",
    "    \"decreaseHotkey\": \"alt+-\"\n",
    "}\n",
);
#[cfg(not(target_os = "windows"))]
const WINDOW_TRANSPARENCY_DEFAULTS: &str = concat!(
    "{\n",
    "    \"enabled\": true,\n",
    "    \"opacity\": 1.0,\n",
    "    \"step\": 0.05,\n",
    "    \"minOpacity\": 0.2,\n",
    "    \"increaseHotkey\": \"ctrl+=\",\n",
    "    \"decreaseHotkey\": \"ctrl+-\"\n",
    "}\n",
);

pub fn ensure_theme_jsons() -> Result<()> {
    fs::create_dir_all(host_dir())?;
    for name in ["FoundationDarkTheme.json", "FoundationLightTheme.json"] {
        let dest: PathBuf = host_dir().join(name);
        if dest.exists() {
            continue;
        }
        let url: String = format!(
            "https://raw.githubusercontent.com/MaximumADHD/Roblox-Client-Tracker/roblox/QtResources/Platform/Base/QtUI/themes/{name}"
        );
        let ok: bool = Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&dest)
            .arg(&url)
            .status()?
            .success();
        if !ok {
            bail!("couldn't grab {name}, drop your own copy in {}", host_dir().display());
        }
    }

    let editor_bg: PathBuf = editor_background_json_path();
    if !editor_bg.exists() {
        fs::write(&editor_bg, EDITOR_BACKGROUND_DEFAULTS)?;
        println!(
            "created {} - set \"image\" to an absolute path to enable a custom script editor background",
            editor_bg.display()
        );
    }

    let window_transparency: PathBuf = window_transparency_json_path();
    if !window_transparency.exists() {
        fs::write(&window_transparency, WINDOW_TRANSPARENCY_DEFAULTS)?;
        println!(
            "created {} - hotkeys adjust studio's window opacity",
            window_transparency.display()
        );
    }
    Ok(())
}

const THEME_NAMES: [&str; 2] = ["FoundationDarkTheme.json", "FoundationLightTheme.json"];

fn theme_url(name: &str) -> String {
    format!(
        "https://raw.githubusercontent.com/MaximumADHD/Roblox-Client-Tracker/roblox/QtResources/Platform/Base/QtUI/themes/{name}"
    )
}

fn fetch_upstream(name: &str) -> Option<Value> {
    let out = Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "5", "--max-time", "20"])
        .arg(theme_url(name))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn merge_missing(local: &mut Value, upstream: &Value) -> usize {
    let (Some(local_obj), Some(upstream_obj)) = (local.as_object_mut(), upstream.as_object())
    else {
        return 0;
    };
    let mut added: usize = 0;
    for (key, upstream_value) in upstream_obj {
        match local_obj.get_mut(key) {
            Some(local_value) if local_value.is_object() && upstream_value.is_object() => {
                added += merge_missing(local_value, upstream_value);
            }
            Some(_) => {}
            None => {
                local_obj.insert(key.clone(), upstream_value.clone());
                added += 1;
            }
        }
    }
    added
}

pub fn sync_theme_jsons() {
    for name in THEME_NAMES {
        let dest: PathBuf = host_dir().join(name);
        let Ok(raw) = fs::read_to_string(&dest) else {
            continue;
        };
        let Ok(mut local) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(upstream) = fetch_upstream(name) else {
            continue;
        };
        let added: usize = merge_missing(&mut local, &upstream);
        if added == 0 {
            continue;
        }
        let Ok(text) = serde_json::to_string_pretty(&local) else {
            continue;
        };
        if fs::write(&dest, format!("{text}\n")).is_ok() {
            let plural: &str = if added == 1 { "entry" } else { "entries" };
            println!(
                "    {}",
                crate::term::dim(&format!("synced {added} new {plural} into {name}"))
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::merge_missing;
    use serde_json::json;

    #[test]
    fn adds_only_missing_keys_and_never_touches_existing_ones() {
        let mut local = json!({
            "Name": "Dark",
            "Presets": { "RobloxDefault": { "Default": "#EDITED" } },
            "RbxmPalette": { "mine": 1 }
        });
        let upstream = json!({
            "Name": "Dark",
            "Presets": {
                "RobloxDefault": { "Default": "#UPSTREAM", "NewKey": "#123456" },
                "NewPreset": { "A": "#000000" }
            },
            "TopLevelNew": true
        });

        let added = merge_missing(&mut local, &upstream);

        assert_eq!(added, 3, "NewKey, NewPreset, TopLevelNew");
        assert_eq!(local["Presets"]["RobloxDefault"]["Default"], "#EDITED");
        assert_eq!(local["Presets"]["RobloxDefault"]["NewKey"], "#123456");
        assert_eq!(local["Presets"]["NewPreset"]["A"], "#000000");
        assert_eq!(local["TopLevelNew"], true);
        assert_eq!(local["RbxmPalette"]["mine"], 1);
    }
}

pub fn run_themes(macho_path: &Path, args: &Args) -> Result<()> {
    let dark_new: String = embedded_json_path("FoundationDarkTheme.json");
    let light_new: String = embedded_json_path("FoundationLightTheme.json");
    let swaps: [(&str, &str); 2] = [
        (
            ":/Platform/Base/QtUI/themes/FoundationDarkTheme.json",
            dark_new.as_str(),
        ),
        (
            ":/Platform/Base/QtUI/themes/FoundationLightTheme.json",
            light_new.as_str(),
        ),
    ];
    for (old, new) in swaps {
        if old.len() != new.len() {
            bail!(
                "bug: {old:?} is {} bytes, {new:?} is {} bytes",
                old.len(),
                new.len()
            );
        }
    }

    let mut data: Vec<u8> = fs::read(macho_path)?;
    let mut sites: Vec<(usize, &str)> = vec![];
    for (old, new) in swaps {
        let pattern: Vec<PatByte> = old.bytes().map(PatByte::Exact).collect();
        for off in find_matches(&data, &pattern) {
            sites.push((off, new));
        }
    }
    if sites.is_empty() {
        bail!("no embedded theme paths found - wrong build, already patched, or qt stopped doing it this way");
    }
    println!("{} theme path(s) found", sites.len());

    if args.dry_run {
        println!("dry run");
        return Ok(());
    }
    kill_running_studio(macho_path, args)?;
    if !args.no_backup {
        backup(macho_path)?;
    }
    for (off, new) in &sites {
        data[*off..*off + new.len()].copy_from_slice(new.as_bytes());
    }
    fs::write(macho_path, &data)?;
    println!("redirected {} theme path(s) to {THEMES_DIR}", sites.len());

    ensure_theme_jsons()?;
    sync_theme_jsons();
    ensure_palette_defaults(&dark_json_path())?;
    println!("edit the jsons in {} then relaunch studio", host_dir().display());

    #[cfg(not(target_os = "windows"))]
    {
        let domain: &str = "com.roblox.RobloxStudio";
        let key: &str = "Themes.CurrentTheme";
        let current: String = Command::new("defaults")
            .args(["read", domain, key])
            .output()
            .map(|o: std::process::Output| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if current != "Dark" && current != "Light" {
            Command::new("defaults")
                .args(["write", domain, key, "-string", "Dark"])
                .status()?;
            println!("{domain} {key} was {current:?}, doesn't match a real theme name - reset to \"Dark\" so studio doesn't crash looking it up");
        }
    }

    if !args.no_resign && !is_pe(&data) {
        resign(macho_path)?;
    }
    Ok(())
}
