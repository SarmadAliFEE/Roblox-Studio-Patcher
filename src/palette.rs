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

enum Lookup {
    ByName,
    ByPath(&'static str),
}

struct Target {
    rbxm_file: &'static str,
    module_name: &'static str,
    lookup: Lookup,
    colors: &'static [(&'static str, &'static str, &'static str)],
}

const TARGETS: &[Target] = &[
    Target {
        rbxm_file: "ExplorerPlugin.rbxm",
        module_name: "Dark",
        lookup: Lookup::ByName,
        colors: &[
            ("Explorer_Background", "Gray_1200", "#121215"),
            ("Explorer_Surface", "Gray_1100", "#191A1F"),
            ("Explorer_Border", "Gray_1000", "#202227"),
        ],
    },
    Target {
        rbxm_file: "Ribbon.rbxm",
        module_name: "Dark",
        lookup: Lookup::ByPath("RbxDesignFoundations-31ab8d40-2.0.163"),
        colors: &[
            ("Ribbon_Background", "Gray_1200", "#121215"),
            ("Ribbon_Surface", "Gray_1100", "#191A1F"),
            ("Ribbon_Border", "Gray_1000", "#202227"),
        ],
    },
    Target {
        rbxm_file: "FindReplaceAll.rbxm",
        module_name: "Dark",
        lookup: Lookup::ByName,
        colors: &[
            ("FindReplace_Background", "Gray_1200", "#121215"),
            ("FindReplace_Surface", "Gray_1100", "#191A1F"),
            ("FindReplace_Border", "Gray_1000", "#202227"),
        ],
    },
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

pub fn ensure_palette_defaults(json_path: &Path) -> Result<()> {
    let raw: String = fs::read_to_string(json_path)?;
    let mut doc: Value = serde_json::from_str(&raw)?;
    let root: &mut serde_json::Map<String, Value> = doc.as_object_mut().context("theme json root isn't an object")?;
    let palette: &mut Value = root
        .entry("RbxmPalette")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let palette_obj: &mut serde_json::Map<String, Value> = palette.as_object_mut().context("RbxmPalette isn't an object")?;

    let mut changed: bool = false;
    for t in TARGETS {
        for (json_key, _, hex) in t.colors {
            if !palette_obj.contains_key(*json_key) {
                palette_obj.insert(json_key.to_string(), json!(hex));
                changed = true;
            }
        }
    }

    if changed {
        fs::write(json_path, serde_json::to_string_pretty(&doc)?)?;
    }
    Ok(())
}

fn json_hex(obj: &serde_json::Map<String, Value>, json_key: &str) -> Result<String> {
    let value: &Value = obj.get(json_key).with_context(|| format!("no {json_key:?} in RbxmPalette"))?;
    Ok(value.as_str().with_context(|| format!("{json_key} isn't a string"))?.to_string())
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

fn patch_target(dir: &Path, obj: &serde_json::Map<String, Value>, t: &Target, args: &Args) -> Result<()> {
    let rbxm_path: PathBuf = dir.join(t.rbxm_file);
    if !rbxm_path.exists() {
        bail!("no {} at {}", t.rbxm_file, rbxm_path.display());
    }
    if args.dry_run {
        println!("dry run - would patch {}", rbxm_path.display());
        return Ok(());
    }

    let mut colors: HashMap<String, (u8, u8, u8)> = HashMap::new();
    for (json_key, lua_field, _) in t.colors {
        colors.insert(lua_field.to_string(), hex_to_rgb(&json_hex(obj, json_key)?)?);
    }
    let markers: Vec<&str> = t.colors.iter().map(|(_, lua_field, _)| *lua_field).collect();
    let source: String = build_source(&colors);
    let bytecode: Vec<u8> = compile_lua(&source)?;

    let data: Vec<u8> = fs::read(&rbxm_path)?;
    let patched: Vec<u8> = match t.lookup {
        Lookup::ByName => rbxm::patch_module_by_name(&data, "ModuleScript", t.module_name, "Source", &markers, &bytecode)?,
        Lookup::ByPath(ancestor) => {
            rbxm::patch_module_by_path(&data, "ModuleScript", t.module_name, ancestor, "Source", &markers, &bytecode)?
        }
    };

    if !args.no_backup {
        backup(&rbxm_path)?;
    }
    fs::write(&rbxm_path, patched)?;
    println!("patched {}", t.rbxm_file);
    Ok(())
}

pub fn run_rbxm_palette(target: &Path, args: &Args) -> Result<()> {
    let dir: PathBuf = match &args.rbxm_dir {
        Some(p) => PathBuf::from(p),
        None => plugins_dir(target)?,
    };

    themes::ensure_theme_jsons()?;
    let dark_json: PathBuf = themes::dark_json_path();
    ensure_palette_defaults(&dark_json)?;

    let raw: String = fs::read_to_string(&dark_json)?;
    let doc: Value = serde_json::from_str(&raw)?;
    let obj: &serde_json::Map<String, Value> = doc
        .get("RbxmPalette")
        .and_then(Value::as_object)
        .context("no RbxmPalette section in theme json")?;

    let mut any_ok: bool = false;
    for t in TARGETS {
        match patch_target(&dir, obj, t, args) {
            Ok(()) => any_ok = true,
            Err(e) => println!("{} skipped ({e})", t.rbxm_file),
        }
    }

    if any_ok && !args.dry_run {
        println!("colors pulled from {}", dark_json.display());
    }
    Ok(())
}
