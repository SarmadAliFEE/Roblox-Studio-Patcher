mod binary;
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
    #[arg(long)]
    binary: Option<PathBuf>,

    #[arg(long)]
    signature: Option<String>,

    #[arg(long)]
    patch: Option<String>,

    #[arg(long, default_value_t = 0)]
    occurrence: usize,

    // hex addrs, comma sep. every adrp+ldrb reading one of these becomes mov wD,#1
    #[arg(long, value_delimiter = ',')]
    globals: Vec<String>,

    #[arg(long)]
    themes: bool,

    #[arg(long)]
    rbxm_palette: bool,

    #[arg(long)]
    rbxm_dir: Option<String>,

    #[arg(long)]
    update: bool,

    #[arg(long)]
    discover: bool,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    no_backup: bool,

    #[arg(long)]
    no_resign: bool,
}

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
    if args.rbxm_palette {
        palette::run_rbxm_palette(&target, &args)?;
        did_something = true;
    }
    if !did_something {
        run_auto(&target, &macho_path, &args)?;
    }
    Ok(())
}
