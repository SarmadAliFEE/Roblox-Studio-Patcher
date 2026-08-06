use std::env;
use std::fs;
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

fn newer(a: &str, b: &str) -> bool {
    let nums = |v: &str| -> Vec<u32> { v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    nums(a) > nums(b)
}

// (version tag, download url for this platform's asset)
fn latest_release() -> Result<(String, String)> {
    let out = Command::new("curl")
        .args(["-fsSL", "-H", "Accept: application/vnd.github+json"])
        .arg(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .output()
        .context("curl not on PATH?")?;
    if !out.status.success() {
        bail!("github didn't respond: {}", String::from_utf8_lossy(&out.stderr));
    }

    let doc: Value = serde_json::from_slice(&out.stdout)?;
    let version = doc["tag_name"].as_str().context("no tag_name in response")?.to_string();
    let url = doc["assets"]
        .as_array()
        .context("no assets in response")?
        .iter()
        .find(|a| a["name"] == ASSET_NAME)
        .and_then(|a| a["browser_download_url"].as_str())
        .with_context(|| format!("release has no {ASSET_NAME} build"))?
        .to_string();

    Ok((version, url))
}

fn install(url: &str) -> Result<()> {
    let tmp: PathBuf = env::temp_dir().join(ASSET_NAME);
    let ok = Command::new("curl").args(["-fsSL", "-o"]).arg(&tmp).arg(url).status()?.success();
    if !ok {
        bail!("download failed");
    }

    #[cfg(not(target_os = "windows"))]
    Command::new("chmod").arg("+x").arg(&tmp).status()?;

    self_replace::self_replace(&tmp)?;
    let _ = fs::remove_file(&tmp);
    Ok(())
}

pub fn check_and_prompt() {
    let current = env!("CARGO_PKG_VERSION");
    let (version, url) = match latest_release() {
        Ok(r) => r,
        Err(e) => return println!("update check failed ({e}), skipping"),
    };

    if !newer(&version, current) {
        return println!("already on the latest version ({current})");
    }
    if !ask_yn(&format!("update available: {current} -> {version} - install it?")) {
        return;
    }

    match install(&url) {
        Ok(()) => {
            println!("updated to {version} - run the tool again");
            std::process::exit(0);
        }
        Err(e) => println!("update failed ({e}), sticking with {current}"),
    }
}
