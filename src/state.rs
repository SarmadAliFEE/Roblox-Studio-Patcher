use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::{binary, inject, term, themes, Args};

#[cfg(target_os = "windows")]
const PAYLOAD_NAME: &str = "studio_hook.dll";
#[cfg(not(target_os = "windows"))]
const PAYLOAD_NAME: &str = "studio_hook.dylib";

const BACKUP_PREFIX: &str = "bak-";

/// On Windows the payload sits beside the executable; elsewhere it lives in the
/// shared themes directory.
fn payload_path(target: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        target.parent().unwrap_or(target).join(PAYLOAD_NAME)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = target;
        Path::new(themes::THEMES_DIR).join(PAYLOAD_NAME)
    }
}

/// Backups written beside `target` by [`binary::backup`], newest first.
fn backups(target: &Path) -> Vec<PathBuf> {
    let Some(dir) = target.parent() else { return Vec::new() };
    let Some(stem) = target.file_stem().and_then(|s| s.to_str()) else { return Vec::new() };
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };

    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
            let Some(rest) = name.strip_prefix(stem) else { return false };
            rest.strip_prefix('.').is_some_and(|ext| ext.starts_with(BACKUP_PREFIX))
        })
        .collect();

    found.sort_by_key(|path| backup_stamp(path));
    found.reverse();
    found
}

/// Unix seconds encoded in a `.bak-<ts>` name, or 0 when it cannot be parsed.
fn backup_stamp(path: &Path) -> u64 {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(|e| e.strip_prefix(BACKUP_PREFIX))
        .and_then(|ts| ts.parse().ok())
        .unwrap_or(0)
}

fn age(stamp: u64) -> String {
    let now: u64 = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let seconds: u64 = now.saturating_sub(stamp);
    match seconds {
        0 => "just now".into(),
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

fn hook_log_path() -> PathBuf {
    if cfg!(target_os = "windows") {
        std::env::temp_dir().join("studio_patcher_hook.txt")
    } else {
        PathBuf::from("/tmp/studio_patcher_hook.txt")
    }
}

/// Latest `resolve: … unavailable` / `install failed` lines from the hook log.
fn hook_problems() -> Vec<String> {
    let Ok(text) = fs::read_to_string(hook_log_path()) else { return Vec::new() };
    let mut problems: Vec<String> = text
        .lines()
        .filter(|line| line.contains("unavailable") || line.contains("install failed"))
        .map(str::to_owned)
        .collect();
    problems.dedup();
    problems.reverse();
    problems.truncate(4);
    problems
}

fn feature_line(label: &str, path: &Path) {
    let Ok(text) = fs::read_to_string(path) else {
        term::detail(&format!("{label}: {}", term::dim("not configured")));
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        term::detail(&format!("{label}: {}", term::red("invalid json")));
        term::detail(&format!("  {}", term::dim(&path.display().to_string())));
        return;
    };
    let enabled: bool = json.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(true);
    let mut note: String = if enabled { term::green("enabled") } else { term::dim("disabled") };

    if let Some(image) = json.get("image").and_then(serde_json::Value::as_str) {
        if image.is_empty() {
            note.push_str(&format!(" {}", term::dim("(no image set)")));
        } else if Path::new(image).exists() {
            note.push_str(&format!(" {}", term::dim(image)));
        } else {
            note.push_str(&format!(" {} {}", term::red("missing image"), term::dim(image)));
        }
    }
    term::detail(&format!("{label}: {note}"));
}

/// Other Studio installs that still load `payload`.
///
/// The macOS payload lives in a directory shared by every install, so it must
/// outlive a restore while any other binary still imports it.
fn still_referenced(restored: &Path, payload: &Path) -> Vec<PathBuf> {
    let Ok(installs) = binary::discover_candidates() else { return Vec::new() };
    let library: String = payload.to_string_lossy().into_owned();

    installs
        .into_iter()
        .filter_map(|install: PathBuf| binary::resolve_macho(&install).ok())
        .filter(|install: &PathBuf| install != restored)
        .filter(|install: &PathBuf| inject::is_injected(install, &library).unwrap_or(false))
        .collect()
}

/// What the patcher has installed for one Studio binary.
pub struct Installed {
    pub payload: PathBuf,
    pub injected: bool,
    pub payload_size: Option<u64>,
    pub backups: Vec<PathBuf>,
}

impl Installed {
    /// True when the binary loads the payload and the payload is on disk.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.injected && self.payload_size.is_some()
    }

    /// One line naming what is set up, for the interactive flow.
    #[must_use]
    pub fn summary(&self) -> String {
        let hook: &str = if self.injected { "hook injected" } else { "hook not injected" };
        let payload: &str =
            if self.payload_size.is_some() { "payload present" } else { "payload missing" };
        let backups: String = match self.backups.len() {
            0 => "no backup".into(),
            1 => "1 backup".into(),
            n => format!("{n} backups"),
        };
        format!("{hook} \u{b7} {payload} \u{b7} {backups}")
    }
}

/// Reads the current install state for `target`.
///
/// # Errors
/// Returns an error if `target` cannot be read or is not a supported binary.
///
/// # Examples
/// ```ignore
/// let installed = inspect(&target)?;
/// ```
pub fn inspect(target: &Path) -> Result<Installed> {
    let payload: PathBuf = payload_path(target);
    let injected: bool = inject::is_injected(target, &payload.to_string_lossy())?;
    Ok(Installed {
        payload_size: fs::metadata(&payload).ok().map(|meta: fs::Metadata| meta.len()),
        payload,
        injected,
        backups: backups(target),
    })
}

/// Prints what is installed for `target`, its backups, and any hook problems.
///
/// # Errors
/// Returns an error if `target` cannot be read or is not a supported binary.
///
/// # Examples
/// ```ignore
/// run_status(&target)?;
/// ```
pub fn run_status(target: &Path) -> Result<()> {
    let installed: Installed = inspect(target)?;

    term::step("studio install");
    term::detail(&target.display().to_string());

    term::step("patch");
    if installed.injected {
        term::detail(&format!("hook: {}", term::green("injected")));
    } else {
        term::detail(&format!("hook: {}", term::yellow("not injected")));
    }

    match installed.payload_size {
        Some(size) => term::detail(&format!(
            "payload: {} {}",
            term::green("present"),
            term::dim(&format!("{} ({} KB)", installed.payload.display(), size / 1024))
        )),
        None => term::detail(&format!(
            "payload: {} {}",
            term::yellow("missing"),
            term::dim(&installed.payload.display().to_string())
        )),
    }

    let saved: Vec<PathBuf> = installed.backups;
    term::step("backups");
    if saved.is_empty() {
        term::detail(&term::dim("none - --restore has nothing to roll back to"));
    } else {
        for path in saved.iter().take(3) {
            term::detail(&format!(
                "{} {}",
                path.display(),
                term::dim(&age(backup_stamp(path)))
            ));
        }
        if saved.len() > 3 {
            term::detail(&term::dim(&format!("and {} older", saved.len() - 3)));
        }
    }

    term::step("features");
    feature_line("editor background", &themes::editor_background_json_path());
    feature_line("window transparency", &themes::window_transparency_json_path());

    let problems: Vec<String> = hook_problems();
    if !problems.is_empty() {
        term::step("hook reported problems");
        for problem in &problems {
            term::detail(&term::yellow(problem));
        }
        term::detail(&term::dim(&hook_log_path().display().to_string()));
    }

    Ok(())
}


/// # Examples
/// ```ignore
/// run_restore(&target, &args)?;
/// ```
pub fn run_restore(target: &Path, args: &Args) -> Result<()> {
    let saved: Vec<PathBuf> = backups(target);
    let Some(newest) = saved.first() else {
        bail!("no backup found next to {}, nothing to restore", target.display());
    };

    term::step("restore");
    term::detail(&format!("target: {}", target.display()));
    term::detail(&format!(
        "from:   {} {}",
        newest.display(),
        term::dim(&age(backup_stamp(newest)))
    ));

    if args.dry_run {
        term::warn("dry run, nothing written");
        return Ok(());
    }

    if !crate::ask_yn("restore this backup over the patched binary?") {
        term::warn("cancelled");
        return Ok(());
    }

    binary::kill_running_studio(target, args)?;
    fs::copy(newest, target).with_context(|| format!("restoring {}", target.display()))?;
    term::ok("binary restored");

    let payload: PathBuf = payload_path(target);
    if payload.exists() {
        let others: Vec<PathBuf> = still_referenced(target, &payload);
        if others.is_empty() {
            match fs::remove_file(&payload) {
                Ok(()) => term::ok(&format!("removed {}", payload.display())),
                Err(err) => term::warn(&format!("could not remove {}: {err}", payload.display())),
            }
        } else {
            term::detail(&format!(
                "kept {} {}",
                payload.display(),
                term::dim(&format!("still used by {} other install(s)", others.len()))
            ));
            for other in &others {
                term::detail(&format!("  {}", term::dim(&other.display().to_string())));
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    if !args.no_resign {
        binary::resign(target)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_stamps_parse_and_order_newest_first() {
        assert_eq!(backup_stamp(Path::new("/x/RobloxStudio.bak-1700000000")), 1_700_000_000);
        assert_eq!(backup_stamp(Path::new("/x/RobloxStudio.bak-nope")), 0);
        assert_eq!(backup_stamp(Path::new("/x/RobloxStudio")), 0);
    }

    #[test]
    fn ages_render_in_the_largest_useful_unit() {
        let now: u64 = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(age(now).contains("just now") || age(now).contains('s'));
        assert!(age(now - 120).ends_with("m ago"));
        assert!(age(now - 7200).ends_with("h ago"));
        assert!(age(now - 200_000).ends_with("d ago"));
    }

    #[test]
    fn a_restored_binary_never_counts_itself_as_a_referencing_install() {
        let dir = std::env::temp_dir().join(format!("rsp-ref-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("RobloxStudio");
        fs::write(&target, b"not-a-macho").unwrap();

        assert!(still_referenced(&target, &dir.join("studio_hook.dylib"))
            .iter()
            .all(|other| other != &target));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn backups_are_found_by_stem_and_sorted() {
        let dir = std::env::temp_dir().join(format!("rsp-state-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("RobloxStudio");
        fs::write(&target, b"x").unwrap();
        fs::write(dir.join("RobloxStudio.bak-100"), b"a").unwrap();
        fs::write(dir.join("RobloxStudio.bak-300"), b"b").unwrap();
        fs::write(dir.join("Unrelated.bak-200"), b"c").unwrap();

        let found = backups(&target);
        assert_eq!(found.len(), 2);
        assert_eq!(backup_stamp(&found[0]), 300);
        assert_eq!(backup_stamp(&found[1]), 100);

        let _ = fs::remove_dir_all(&dir);
    }
}
