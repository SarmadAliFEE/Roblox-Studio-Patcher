use std::cmp::Ordering;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::ask_yn;

const REPO: &str = "uwufuzzywiiiaisddd/Roblox-Studio-Patcher";

#[cfg(target_os = "windows")]
const ASSET_NAME: &str = "Roblox-Studio-Patcher-windows.exe";
#[cfg(not(target_os = "windows"))]
const ASSET_NAME: &str = "Roblox-Studio-Patcher-mac-silicon";

struct Release {
    version: String,
    url: String,
}

enum Ident {
    Num(u64),
    Text(String),
}

impl Ident {
    fn parse(part: &str) -> Ident {
        part.parse::<u64>().map(Ident::Num).unwrap_or_else(|_| Ident::Text(part.to_owned()))
    }
}

fn parse_version(tag: &str) -> ([u32; 3], Vec<Ident>) {
    let stripped = tag.trim_start_matches('v');
    let (core, pre) = match stripped.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (stripped, None),
    };
    let mut fields = core.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let core = [fields.next().unwrap_or(0), fields.next().unwrap_or(0), fields.next().unwrap_or(0)];
    let pre = pre.map(|p| p.split('.').map(Ident::parse).collect()).unwrap_or_default();
    (core, pre)
}

fn cmp_ident(a: &Ident, b: &Ident) -> Ordering {
    match (a, b) {
        (Ident::Num(x), Ident::Num(y)) => x.cmp(y),
        (Ident::Num(_), Ident::Text(_)) => Ordering::Less,
        (Ident::Text(_), Ident::Num(_)) => Ordering::Greater,
        (Ident::Text(x), Ident::Text(y)) => x.cmp(y),
    }
}

fn cmp_versions(a: &str, b: &str) -> Ordering {
    let (core_a, pre_a) = parse_version(a);
    let (core_b, pre_b) = parse_version(b);
    match core_a.cmp(&core_b) {
        Ordering::Equal => {}
        other => return other,
    }
    match (pre_a.is_empty(), pre_b.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            for (x, y) in pre_a.iter().zip(pre_b.iter()) {
                match cmp_ident(x, y) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
            pre_a.len().cmp(&pre_b.len())
        }
    }
}

fn is_newer(candidate: &str, current: &str) -> bool {
    cmp_versions(candidate, current) == Ordering::Greater
}

fn asset_url(release: &Value) -> Option<String> {
    release["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"] == ASSET_NAME)
        .and_then(|a| a["browser_download_url"].as_str())
        .map(str::to_owned)
}

fn latest_releases() -> Result<(Option<Release>, Option<Release>)> {
    let out = Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "5", "--max-time", "15", "-H", "Accept: application/vnd.github+json"])
        .arg(format!("https://api.github.com/repos/{REPO}/releases?per_page=30"))
        .output()
        .context("curl not on PATH?")?;
    if !out.status.success() {
        bail!("github didn't respond: {}", String::from_utf8_lossy(&out.stderr));
    }

    let doc: Value = serde_json::from_slice(&out.stdout)?;
    let releases = doc.as_array().context("unexpected releases response")?;
    let mut stable: Option<Release> = None;
    let mut nightly: Option<Release> = None;
    for release in releases {
        if release["draft"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(version) = release["tag_name"].as_str() else { continue };
        let Some(url) = asset_url(release) else { continue };
        let slot = if release["prerelease"].as_bool().unwrap_or(false) {
            &mut nightly
        } else {
            &mut stable
        };
        if slot.is_none() {
            *slot = Some(Release { version: version.to_owned(), url });
        }
    }
    Ok((stable, nightly))
}

fn install(url: &str) -> Result<()> {
    let tmp: PathBuf = env::temp_dir().join(ASSET_NAME);
    let ok = Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "10", "--max-time", "120", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()?
        .success();
    if !ok {
        bail!("download failed");
    }

    #[cfg(not(target_os = "windows"))]
    Command::new("chmod").arg("+x").arg(&tmp).status()?;

    self_replace::self_replace(&tmp)?;
    let _ = fs::remove_file(&tmp);
    Ok(())
}

fn pick_channel(current: &str, stable: Release, nightly: Release) -> Option<Release> {
    crate::term::step("update available");
    println!(
        "    {} stable   {current} -> {}{}",
        crate::term::cyan("[1]"),
        stable.version,
        crate::term::dim(" (default)")
    );
    println!(
        "    {} nightly  {current} -> {}",
        crate::term::cyan("[2]"),
        nightly.version
    );
    loop {
        print!(
            "    {} ",
            crate::term::dim("install which? [1-2, enter for 1, n to skip]")
        );
        let _ = io::stdout().flush();
        let mut line = String::new();
        io::stdin().read_line(&mut line).ok();
        match line.trim() {
            "" | "1" => return Some(stable),
            "2" => return Some(nightly),
            "n" | "N" => return None,
            _ => crate::term::warn("enter 1, 2, or n"),
        }
    }
}

pub fn check_and_prompt() {
    let current = env!("CARGO_PKG_VERSION");
    let spinner = crate::term::Spinner::start("checking for updates");
    let fetched = latest_releases();
    spinner.finish();
    let (stable, nightly) = match fetched {
        Ok(pair) => pair,
        Err(e) => return crate::term::warn(&format!("update check failed ({e}), skipping")),
    };

    let stable_update = stable.filter(|r| is_newer(&r.version, current));
    let nightly_update = nightly.filter(|r| is_newer(&r.version, current));

    if let Some(pre) = &nightly_update {
        crate::term::warn(&format!(
            "nightly pre-release {} available - unstable, opt in below",
            pre.version
        ));
    }

    let chosen = match (stable_update, nightly_update) {
        (None, None) => return crate::term::ok(&format!("up to date ({current})")),
        (Some(stable), None) => ask_yn(&format!(
            "update available: {current} -> {} - install it?",
            stable.version
        ))
        .then_some(stable),
        (None, Some(nightly)) => ask_yn(&format!(
            "install nightly pre-release {current} -> {}?",
            nightly.version
        ))
        .then_some(nightly),
        (Some(stable), Some(nightly)) => pick_channel(current, stable, nightly),
    };

    let Some(release) = chosen else { return };
    match install(&release.url) {
        Ok(()) => {
            crate::term::ok(&format!("updated to {} - run the tool again", release.version));
            std::process::exit(0);
        }
        Err(e) => crate::term::warn(&format!("update failed ({e}), sticking with {current}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{cmp_versions, is_newer};
    use std::cmp::Ordering;

    #[test]
    fn a_prerelease_sorts_below_its_final_release() {
        assert_eq!(cmp_versions("0.7.0-rc.1", "0.7.0"), Ordering::Less);
        assert_eq!(cmp_versions("0.7.0", "0.7.0-rc.1"), Ordering::Greater);
    }

    #[test]
    fn prereleases_compare_by_identifier() {
        assert_eq!(cmp_versions("0.7.0-rc.2", "0.7.0-rc.1"), Ordering::Greater);
        assert_eq!(cmp_versions("0.7.0-rc.10", "0.7.0-rc.2"), Ordering::Greater);
    }

    #[test]
    fn core_version_dominates_and_v_prefix_is_ignored() {
        assert_eq!(cmp_versions("0.7.1", "0.7.0"), Ordering::Greater);
        assert_eq!(cmp_versions("v0.7.0", "0.7.0"), Ordering::Equal);
        assert_eq!(cmp_versions("0.6.1", "0.7.0-rc.1"), Ordering::Less);
    }

    #[test]
    fn is_newer_matches_expected_channel_transitions() {
        assert!(is_newer("0.7.0-rc.1", "0.6.1"));
        assert!(is_newer("0.7.0", "0.7.0-rc.1"));
        assert!(!is_newer("0.6.1", "0.7.0-rc.1"));
        assert!(!is_newer("0.7.0-rc.1", "0.7.0-rc.1"));
    }
}
