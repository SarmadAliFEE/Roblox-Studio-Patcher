mod binary;
mod hooks;
mod inject;
mod palette;
mod rbxm;
mod state;
mod term;
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

    /// Write every error and crash to a local log file.
    #[arg(long)]
    crash_logging: bool,

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

    /// Report what is installed for the target studio, then exit.
    #[arg(long)]
    status: bool,

    /// Roll the target studio back to its newest backup, then exit.
    #[arg(long)]
    restore: bool,
}

/// Prompts `q [y/N]` on stdout and reads a yes/no answer from stdin.
pub fn ask_yn(q: &str) -> bool {
    print!("  {} {} ", term::bold(q), term::dim("[y/N]"));
    read_yn()
}

fn ask_feature(title: &str, detail: &str) -> bool {
    term::step(title);
    term::detail(detail);
    print!("    {} ", term::dim("[y/N]"));
    read_yn()
}

fn read_yn() -> bool {
    let _ = io::stdout().flush();
    let mut line: String = String::new();
    io::stdin().read_line(&mut line).ok();
    matches!(line.trim(), "y" | "Y" | "yes")
}

fn select_target() -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = binary::discover_candidates()?;
    if candidates.len() == 1 {
        return Ok(candidates.remove(0));
    }

    term::step("multiple roblox studio installs found");
    for (index, candidate) in candidates.iter().enumerate() {
        let tag = if index == 0 { term::dim(" (default)") } else { String::new() };
        println!(
            "    {} {}{}",
            term::cyan(&format!("[{}]", index + 1)),
            candidate.display(),
            tag
        );
    }
    let choice = ask_index(candidates.len());
    Ok(candidates.remove(choice))
}

fn ask_index(count: usize) -> usize {
    loop {
        print!(
            "    {} ",
            term::dim(&format!("which build? [1-{count}, enter for 1]"))
        );
        let _ = io::stdout().flush();
        let mut line: String = String::new();
        io::stdin().read_line(&mut line).ok();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return 0;
        }
        if let Ok(n) = trimmed.parse::<usize>() {
            if (1..=count).contains(&n) {
                return n - 1;
            }
        }
        term::warn("enter a number from the list");
    }
}

fn ask_choice(options: &[&str]) -> usize {
    for (index, option) in options.iter().enumerate() {
        println!("    {} {}", term::cyan(&format!("[{}]", index + 1)), option);
    }
    loop {
        print!(
            "    {} ",
            term::dim(&format!("choose [1-{}, enter for 1]", options.len()))
        );
        let _ = io::stdout().flush();
        let mut line: String = String::new();
        io::stdin().read_line(&mut line).ok();
        let trimmed: &str = line.trim();
        if trimmed.is_empty() {
            return 0;
        }
        if let Ok(n) = trimmed.parse::<usize>() {
            if (1..=options.len()).contains(&n) {
                return n - 1;
            }
        }
        term::warn("enter a number from the list");
    }
}

fn run_auto(target: &std::path::Path, macho_path: &std::path::Path, args: &Args) -> Result<()> {
    update::check_and_prompt();

    if let Ok(installed) = state::inspect(macho_path) {
        if installed.injected {
            term::step("this studio is already set up");
            term::detail(&installed.summary());
            if !installed.is_healthy() {
                term::warn("the payload is missing - studio may fail to launch until you re-apply");
            }

            match ask_choice(&[
                "set up features again (re-applies everything you pick)",
                "show what's installed",
                "put studio back the way it was",
                "quit",
            ]) {
                1 => {
                    state::run_status(macho_path)?;
                    println!();
                    return Ok(());
                }
                2 => {
                    state::run_restore(macho_path, args)?;
                    println!();
                    return Ok(());
                }
                3 => {
                    println!();
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    let mut globals_args: Args = args.clone();
    globals_args.globals = vec!["auto".to_string()];
    term::step("patching plugin permissions");
    term::detail("lets studio load unsigned plugins");
    match binary::run_globals(macho_path, &globals_args) {
        Ok(_) => term::ok("permissions patched"),
        Err(e) => term::warn(&format!("skipped ({e}) - probably already patched")),
    }

    if ask_feature(
        "enable custom theme support?",
        "patches the binary to load theme jsons off disk",
    ) {
        match themes::run_themes(macho_path, args) {
            Ok(_) => term::ok("theme support enabled"),
            Err(e) => term::warn(&format!("skipped ({e}) - probably already patched")),
        }
    }

    if ask_feature(
        "also apply RbxmPalette colors?",
        "recolors plugin bytecode that ignores the qt theme",
    ) {
        match palette::run_rbxm_palette(target, args) {
            Ok(_) => term::ok("rbxm palette applied"),
            Err(e) => term::warn(&format!("rbxm palette patch failed ({e})")),
        }
    }

    if ask_feature(
        "enable native hooks?",
        "adds a custom image behind the script editor",
    ) {
        match hooks::install_editor_background(macho_path, args) {
            Ok(_) => term::ok("editor background hook installed"),
            Err(e) => term::warn(&format!("hook install failed ({e})")),
        }
    }

    if ask_feature(
        "enable window transparency hotkeys?",
        "hotkeys to fade studio's whole window in and out",
    ) {
        match hooks::install_window_transparency(macho_path, args) {
            Ok(_) => term::ok("window transparency hook installed"),
            Err(e) => term::warn(&format!("hook install failed ({e})")),
        }
    }

    if ask_feature(
        "enable discord rich presence?",
        "shows the place and script you're editing",
    ) {
        match hooks::install_studio_hook(macho_path, args) {
            Ok(_) => term::ok("discord rich presence installed"),
            Err(e) => term::warn(&format!("hook install failed ({e})")),
        }
    }

    if ask_feature(
        "enable crash logging?",
        "writes every error and crash to a local log file",
    ) {
        match hooks::install_logger(macho_path, args) {
            Ok(_) => term::ok("crash logging installed"),
            Err(e) => term::warn(&format!("hook install failed ({e})")),
        }
    }

    println!();
    term::ok(&term::bold("all done"));
    println!();
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{} {e:#}", term::red("error:"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Args = Args::parse();

    if args.update {
        update::check_and_prompt();
        return Ok(());
    }

    term::banner();

    let target: PathBuf = match args.binary.clone() {
        Some(path) => path,
        None => select_target()?,
    };
    let macho_path: PathBuf = binary::resolve_macho(&target)?;
    println!("{} {}", term::dim("target"), term::cyan(&macho_path.display().to_string()));

    if args.status {
        state::run_status(&macho_path)?;
        return Ok(());
    }
    if args.restore {
        state::run_restore(&macho_path, &args)?;
        return Ok(());
    }

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
    if args.crash_logging {
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
