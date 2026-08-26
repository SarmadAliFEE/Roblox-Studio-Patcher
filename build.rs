use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cpp_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("cpp") {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn build_luau_compiler() {
    let luau = PathBuf::from("crates/studio-hook/vendor/luau");
    println!("cargo:rerun-if-changed={}", luau.display());

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .opt_level_str("s")
        .warnings(false)
        .define("LUACODE_API", "extern \"C\"")
        .include(luau.join("Ast/include"))
        .include(luau.join("Compiler/include"))
        .include(luau.join("Common/include"))
        .include(luau.join("Bytecode/include"));

    for dir in ["Ast/src", "Compiler/src", "Common/src"] {
        for file in cpp_sources(&luau.join(dir)) {
            build.file(file);
        }
    }
    build.file(luau.join("Bytecode/src/BytecodeBuilder.cpp"));

    build.compile("luau_compiler");
}

fn main() {
    build_luau_compiler();

    println!("cargo:rerun-if-changed=crates/studio-hook/src");
    println!("cargo:rerun-if-changed=crates/studio-hook/build.rs");
    println!("cargo:rerun-if-changed=crates/studio-hook/Cargo.toml");

    let out_dir: PathBuf = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target: String = env::var("TARGET").unwrap();
    let target_os: String = env::var("CARGO_CFG_TARGET_OS").unwrap();

    let hook_target_dir: PathBuf = out_dir.join("studio-hook-build");
    let cargo: String = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let ok: bool = Command::new(cargo)
        .args([
            "build", "--release",
            "--manifest-path", "crates/studio-hook/Cargo.toml",
            "--target", &target,
            "--target-dir",
        ])
        .arg(&hook_target_dir)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("couldn't run cargo to build the studio-hook payload")
        .success();
    if !ok {
        panic!("failed to build the studio-hook cdylib payload");
    }

    let (built, embedded): (&str, &str) = if target_os == "windows" {
        ("studio_hook.dll", "studio_hook_payload.dll")
    } else {
        ("libstudio_hook.dylib", "libstudio_hook_payload.dylib")
    };
    let src: PathBuf = hook_target_dir.join(&target).join("release").join(built);
    std::fs::copy(&src, out_dir.join(embedded))
        .unwrap_or_else(|e| panic!("couldn't stage studio-hook payload from {}: {e}", src.display()));
}
