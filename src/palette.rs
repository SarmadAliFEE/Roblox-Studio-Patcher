use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde_json::{json, Value};

use crate::binary::backup;
use crate::rbxm;
use crate::themes;
use crate::Args;

const DARK_TOKENS_TEMPLATE: &str = include_str!("../assets/dark_tokens.lua");

// (json key the user actually edits, internal luau token name, stock hex)
// darkest first - Explorer_Background is the actual panel background
const PALETTE_MAP: [(&str, &str, &str); 3] = [
    ("Explorer_Background", "Gray_1200", "#121215"),
    ("Explorer_Surface", "Gray_1100", "#191A1F"),
    ("Explorer_Border", "Gray_1000", "#202227"),
];

fn hex_to_rgb(hex: &str) -> Result<(u8, u8, u8)> {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        bail!("bad hex color {hex:?}, need #RRGGBB");
    }
    let r: u8 = u8::from_str_radix(&h[0..2], 16)?;
    let g: u8 = u8::from_str_radix(&h[2..4], 16)?;
    let b: u8 = u8::from_str_radix(&h[4..6], 16)?;
    Ok((r, g, b))
}

// backfills whatever RbxmPalette entries are missing (whole section, or just
// individual keys someone deleted) so the json always has something valid to
// read, whether this is a first run or the section's just gone stale
pub fn ensure_palette_defaults(json_path: &Path) -> Result<()> {
    let raw: String = fs::read_to_string(json_path)?;
    let mut doc: Value = serde_json::from_str(&raw)?;
    let root: &mut serde_json::Map<String, Value> = doc.as_object_mut().context("theme json root isn't an object")?;
    let palette: &mut Value = root
        .entry("RbxmPalette")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let palette_obj: &mut serde_json::Map<String, Value> = palette.as_object_mut().context("RbxmPalette isn't an object")?;

    let mut changed: bool = false;
    for (json_key, _, hex) in PALETTE_MAP {
        if !palette_obj.contains_key(json_key) {
            palette_obj.insert(json_key.to_string(), json!(hex));
            changed = true;
        }
    }

    if changed {
        fs::write(json_path, serde_json::to_string_pretty(&doc)?)?;
    }
    Ok(())
}

// returns luau token name -> rgb, translated from the human-friendly json keys
fn read_palette(json_path: &Path) -> Result<HashMap<String, (u8, u8, u8)>> {
    let raw: String = fs::read_to_string(json_path)?;
    let doc: Value = serde_json::from_str(&raw)?;
    let palette: &Value = doc
        .get("RbxmPalette")
        .context("no RbxmPalette section in theme json")?;
    let obj: &serde_json::Map<String, Value> = palette.as_object().context("RbxmPalette isn't an object")?;

    let mut out: HashMap<String, (u8, u8, u8)> = HashMap::new();
    for (json_key, lua_token, _) in PALETTE_MAP {
        let Some(value) = obj.get(json_key) else { continue };
        let hex: &str = value.as_str().with_context(|| format!("{json_key} isn't a string"))?;
        out.insert(lua_token.to_string(), hex_to_rgb(hex)?);
    }
    Ok(out)
}

fn build_source(colors: &HashMap<String, (u8, u8, u8)>) -> String {
    let mut source: String = DARK_TOKENS_TEMPLATE.to_string();
    for (name, (r, g, b)) in colors {
        let pattern: String = format!(
            r"({name}\s*=\s*\{{\s*Color3\s*=\s*Color3\.fromRGB\()\s*\d+\s*,\s*\d+\s*,\s*\d+\s*(\))"
        );
        let re: Regex = Regex::new(&pattern).unwrap();
        let replacement: String = format!("${{1}}{r}, {g}, {b}${{2}}");
        source = re.replace(&source, replacement.as_str()).into_owned();
    }
    source
}

fn compile_lua(source: &str) -> Result<Vec<u8>> {
    mlua::chunk::Compiler::new()
        .compile(source)
        .map_err(|e: mlua::prelude::LuaError| anyhow::anyhow!("luau compile failed: {e}"))
}

pub fn plugins_dir(target: &Path) -> Result<PathBuf> {
    let candidates: Vec<PathBuf> = if target.extension().and_then(|e| e.to_str()) == Some("app") {
        vec![target.join("Contents/Resources/BuiltInStandalonePlugins/Optimized_Embedded_Signature")]
    } else {
        let version_dir: &Path = target.parent().context("binary has no parent dir")?;
        vec![
            version_dir.join("BuiltInStandalonePlugins/Optimized_Embedded_Signature"),
            version_dir.join("BuiltInStandalonePlugins"),
        ]
    };
    candidates
        .into_iter()
        .find(|p: &PathBuf| p.exists())
        .context("couldn't find BuiltInStandalonePlugins next to studio, pass --rbxm-dir")
}

pub fn run_rbxm_palette(target: &Path, args: &Args) -> Result<()> {
    let dir: PathBuf = match &args.rbxm_dir {
        Some(p) => PathBuf::from(p),
        None => plugins_dir(target)?,
    };
    let explorer_path: PathBuf = dir.join("ExplorerPlugin.rbxm");
    if !explorer_path.exists() {
        bail!("no ExplorerPlugin.rbxm at {}", explorer_path.display());
    }

    if args.dry_run {
        println!("dry run - would patch {}", explorer_path.display());
        return Ok(());
    }

    themes::ensure_theme_jsons()?;
    let dark_json: PathBuf = themes::dark_json_path();
    ensure_palette_defaults(&dark_json)?;
    let colors: HashMap<String, (u8, u8, u8)> = read_palette(&dark_json)?;

    let markers: Vec<&str> = PALETTE_MAP.iter().map(|(_, lua_token, _)| *lua_token).collect();
    let source: String = build_source(&colors);
    let bytecode: Vec<u8> = compile_lua(&source)?;

    let data: Vec<u8> = fs::read(&explorer_path)?;
    let patched: Vec<u8> = rbxm::patch_module_by_name(&data, "ModuleScript", "Dark", "Source", &markers, &bytecode)?;

    if !args.no_backup {
        backup(&explorer_path)?;
    }
    fs::write(&explorer_path, patched)?;
    println!("patched explorer palette from {}", dark_json.display());
    Ok(())
}
