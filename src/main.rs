mod binary;
mod hooks;
mod inject;
mod palette;
mod rbxm;
mod themes;
mod update;

use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "studio-patcher", version)]
pub struct Args {
    /// Path to the studio install/binary; auto-discovered if omitted.
    #[arg(long)]
    binary: Option<PathBuf>,

    /// Raw signature bytes to search for, paired with --patch.
    #[arg(long)]
    signature: Option<String>,

    /// Replacement bytes to splice in at the --signature match.
    #[arg(long)]
    patch: Option<String>,

    /// Which match of --signature to patch, 0-indexed.
    #[arg(long, default_value_t = 0)]
    occurrence: usize,

    /// Hex addrs, comma separated, or "auto" - each becomes mov wD,#1.
    #[arg(long, value_delimiter = ',')]
    globals: Vec<String>,

    /// Redirect studio's theme jsons onto disk for editing.
    #[arg(long)]
    themes: bool,

    /// Patch Explorer/Ribbon/FindReplaceAll plugin colors from RbxmPalette.
    #[arg(long)]
    rbxm_palette: bool,

    /// Reapply --rbxm-palette on every save of the theme json.
    #[arg(long)]
    watch: bool,

    /// Override where the plugin rbxm's live, if auto-detection fails.
    #[arg(long)]
    rbxm_dir: Option<String>,

    /// Check for and install a newer release, then exit.
    #[arg(long)]
    update: bool,

    /// Load a dylib (mac) or dll (windows) into the target binary.
    #[arg(long)]
    inject: Option<String>,

    /// Install the discord rich presence hook.
    #[arg(long)]
    discord: bool,

    /// Report every error and crash to a discord webhook.
    #[arg(long)]
    webhook_logging: bool,

    /// Print candidate globals/permission-check sites without patching.
    #[arg(long)]
    discover: bool,

    /// Show what would change without writing anything.
    #[arg(long)]
    dry_run: bool,

    /// Skip making a .bak copy before patching.
    #[arg(long)]
    no_backup: bool,

    /// Skip re-signing after patching (mac only).
    #[arg(long)]
    no_resign: bool,

    /// Skip force-killing running studio processes before patching.
    #[arg(long)]
    no_kill_studio: bool,
}

/// Prompts `q [y/N]` on stdout and reads a yes/no answer from stdin.
pub fn ask_yn(q: &str) -> bool {
    print!("{q} [y/N] ");
    let _ = io::stdout().flush();
    let mut line: String = String::new();
    io::stdin().read_line(&mut line).ok();
    matches!(line.trim(), "y" | "Y" | "yes")
}

fn run_auto(target: &std::path::Path, macho_path: &std::path::Path, args: &Args) -> Result<()> {
    update::check_and_prompt();

    let mut globals_args: Args = args.clone();
    globals_args.globals = vec!["auto".to_string()];
    if let Err(e) = binary::run_globals(macho_path, &globals_args) {
        println!("permission patch failed ({e}) - probably already patched");
    }

    println!("custom themes work by patching the binary to load theme jsons off disk");
    if ask_yn("enable custom theme support?") {
        if let Err(e) = themes::run_themes(macho_path, args) {
            println!("theme patch failed ({e}) - probably already patched");
        }
    }

    println!("certain plugins have their own colors baked into plugin bytecode, separate from the qt theme");
    if ask_yn("also apply the RbxmPalette colors from the same theme json?") {
        if let Err(e) = palette::run_rbxm_palette(target, args) {
            println!("rbxm palette patch failed ({e})");
        }
    }

    println!("hooks add optional native behavior (currently: a custom script editor background image)");
    if ask_yn("enable hooks?") {
        if let Err(e) = hooks::install_editor_background(macho_path, args) {
            println!("hook install failed ({e})");
        }
    }

    println!("another hook: hotkeys to fade studio's whole window in/out");
    if ask_yn("enable window transparency hotkeys?") {
        if let Err(e) = hooks::install_window_transparency(macho_path, args) {
            println!("hook install failed ({e})");
        }
    }

    println!("another hook: discord rich presence showing the place and script you're editing");
    if ask_yn("enable discord rich presence?") {
        if let Err(e) = hooks::install_studio_hook(macho_path, args) {
            println!("hook install failed ({e})");
        }
    }

    println!("optional: report every error and crash to a discord webhook (for debugging)");
    if ask_yn("enable webhook logging?") {
        if let Err(e) = hooks::install_logger(macho_path, args) {
            println!("hook install failed ({e})");
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Args = Args::parse();

    if args.update {
        update::check_and_prompt();
        return Ok(());
    }

    let target: PathBuf = args
        .binary
        .clone()
        .map(Ok)
        .unwrap_or_else(binary::discover_binary)?;
    let macho_path: PathBuf = binary::resolve_macho(&target)?;
    println!("target: {}", macho_path.display());

    let mut did_something: bool = false;
    if args.discover {
        binary::run_discover(&macho_path)?;
        did_something = true;
    }
    if !args.globals.is_empty() {
        binary::run_globals(&macho_path, &args)?;
        did_something = true;
    }
    if args.signature.is_some() && args.patch.is_some() {
        binary::run_sig(&macho_path, &args)?;
        did_something = true;
    }
    if args.themes {
        themes::run_themes(&macho_path, &args)?;
        did_something = true;
    }
    if let Some(dylib_path) = &args.inject {
        binary::kill_running_studio(&macho_path, &args)?;
        if !args.no_backup {
            binary::backup(&macho_path)?;
        }
        inject::inject_dylib(&macho_path, dylib_path)?;
        let is_pe: bool = {
            let mut head: [u8; 128] = [0; 128];
            let n: usize = std::io::Read::read(&mut std::fs::File::open(&macho_path)?, &mut head)?;
            binary::is_pe(&head[..n])
        };
        if !args.no_resign && !is_pe {
            binary::resign(&macho_path)?;
        }
        did_something = true;
    }
    if args.discord {
        hooks::install_studio_hook(&macho_path, &args)?;
        did_something = true;
    }
    if args.webhook_logging {
        hooks::install_logger(&macho_path, &args)?;
        did_something = true;
    }
    if args.watch {
        palette::run_watch(&target, &args)?;
        did_something = true;
    } else if args.rbxm_palette {
        palette::run_rbxm_palette(&target, &args)?;
        did_something = true;
    }
    if !did_something {
        run_auto(&target, &macho_path, &args)?;
    }
    Ok(())
}
