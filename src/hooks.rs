use std::path::Path;

use anyhow::Result;

use crate::Args;

#[cfg(not(target_os = "windows"))]
const EDITOR_BACKGROUND_SOURCE: &str = include_str!("../hooks/editor_background.mm");

/// Compiles and injects the script-editor-background hook.
#[cfg(not(target_os = "windows"))]
pub fn install_editor_background(macho_path: &Path, args: &Args) -> Result<()> {
    use anyhow::{bail, Context};
    use std::process::Command;

    use crate::{binary, inject, themes};

    let src_path: std::path::PathBuf = std::env::temp_dir().join("studio_patcher_editor_background.mm");
    std::fs::write(&src_path, EDITOR_BACKGROUND_SOURCE)?;

    std::fs::create_dir_all(themes::THEMES_DIR)?;
    let dylib_path: std::path::PathBuf = Path::new(themes::THEMES_DIR).join("editor_background.dylib");

    let ok: bool = Command::new("clang++")
        .args(["-dynamiclib", "-std=c++17", "-fobjc-arc", "-arch", "arm64", "-framework", "Foundation", "-o"])
        .arg(&dylib_path)
        .arg(&src_path)
        .status()
        .context("couldn't run clang++ - install xcode command line tools (xcode-select --install)")?
        .success();
    if !ok {
        bail!("clang++ failed to compile the hook");
    }

    themes::ensure_theme_jsons()?;
    if !args.no_backup {
        binary::backup(macho_path)?;
    }
    inject::inject_dylib(macho_path, &dylib_path.to_string_lossy())?;
    if !args.no_resign {
        binary::resign(macho_path)?;
    }
    println!("hook installed - edit {} to configure it", themes::editor_background_json_path().display());
    Ok(())
}

#[cfg(target_os = "windows")]
const EDITOR_BACKGROUND_DLL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/editor_background_windows.dll"));

#[cfg(target_os = "windows")]
pub fn install_editor_background(exe_path: &Path, args: &Args) -> Result<()> {
    use anyhow::Context;

    use crate::{binary, inject, themes};

    let exe_dir: &Path = exe_path.parent().context("exe path has no parent directory")?;
    let dll_path: std::path::PathBuf = exe_dir.join("editor_background_windows.dll");
    std::fs::write(&dll_path, EDITOR_BACKGROUND_DLL)?;

    themes::ensure_theme_jsons()?;
    if !args.no_backup {
        binary::backup(exe_path)?;
    }
    inject::inject_dylib(exe_path, &dll_path.to_string_lossy())?;
    println!("hook installed - edit {} to configure it", themes::editor_background_json_path().display());
    Ok(())
}
