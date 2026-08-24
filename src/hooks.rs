use std::path::Path;

use anyhow::Result;

use crate::Args;

#[cfg(not(target_os = "windows"))]
const STUDIO_HOOK_PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libstudio_hook_payload.dylib"));
#[cfg(target_os = "windows")]
const STUDIO_HOOK_PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/studio_hook_payload.dll"));

#[cfg(not(target_os = "windows"))]
fn install_payload(macho_path: &Path, args: &Args) -> Result<()> {
    use crate::{binary, inject, themes};

    std::fs::create_dir_all(themes::THEMES_DIR)?;
    let dylib_path: std::path::PathBuf = Path::new(themes::THEMES_DIR).join("studio_hook.dylib");
    std::fs::write(&dylib_path, STUDIO_HOOK_PAYLOAD)?;

    binary::kill_running_studio(macho_path, args)?;
    if !args.no_backup {
        binary::backup(macho_path)?;
    }
    inject::inject_dylib(macho_path, &dylib_path.to_string_lossy())?;
    if !args.no_resign {
        binary::resign(macho_path)?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn install_payload(exe_path: &Path, args: &Args) -> Result<()> {
    use anyhow::Context;

    use crate::{binary, inject};

    let exe_dir: &Path = exe_path.parent().context("exe path has no parent directory")?;
    let dll_path: std::path::PathBuf = exe_dir.join("studio_hook.dll");
    std::fs::write(&dll_path, STUDIO_HOOK_PAYLOAD)?;

    binary::kill_running_studio(exe_path, args)?;
    if !args.no_backup {
        binary::backup(exe_path)?;
    }
    inject::inject_dylib(exe_path, &dll_path.to_string_lossy())?;
    Ok(())
}

/// Injects the payload; discord presence self-activates.
pub fn install_studio_hook(target: &Path, args: &Args) -> Result<()> {
    install_payload(target, args)?;
    println!("discord rich presence hook installed");
    Ok(())
}

/// Injects the payload and writes the editor-background config to fill in.
pub fn install_editor_background(target: &Path, args: &Args) -> Result<()> {
    crate::themes::ensure_theme_jsons()?;
    install_payload(target, args)?;
    println!(
        "hook installed - set \"image\" in {} to a background image",
        crate::themes::editor_background_json_path().display()
    );
    Ok(())
}

/// Injects the payload and writes the window-transparency config.
pub fn install_window_transparency(target: &Path, args: &Args) -> Result<()> {
    crate::themes::ensure_theme_jsons()?;
    install_payload(target, args)?;
    println!(
        "hook installed - hotkeys in {} adjust window opacity",
        crate::themes::window_transparency_json_path().display()
    );
    Ok(())
}
