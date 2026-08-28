use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=crates/studio-hook/src");
    println!("cargo:rerun-if-changed=crates/studio-hook/Cargo.toml");
    println!("cargo:rerun-if-changed=crates/luau-compile/src");
    println!("cargo:rerun-if-changed=crates/luau-compile/build.rs");

    let out_dir: PathBuf = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target: String = env::var("TARGET").unwrap();
    let target_os: String = env::var("CARGO_CFG_TARGET_OS").unwrap();

    let payload_target: String = if target_os == "linux" {
        "x86_64-pc-windows-gnu".to_string()
    } else {
        target.clone()
    };

    let hook_target_dir: PathBuf = out_dir.join("studio-hook-build");
    let cargo: String = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let ok: bool = Command::new(cargo)
        .args([
            "build", "--release",
            "--manifest-path", "crates/studio-hook/Cargo.toml",
            "--target", &payload_target,
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

    let (built, embedded): (&str, &str) = if target_os == "windows" || target_os == "linux" {
        ("studio_hook.dll", "studio_hook_payload.dll")
    } else {
        ("libstudio_hook.dylib", "libstudio_hook_payload.dylib")
    };
    let src: PathBuf = hook_target_dir.join(&payload_target).join("release").join(built);
    std::fs::copy(&src, out_dir.join(embedded))
        .unwrap_or_else(|e| panic!("couldn't stage studio-hook payload from {}: {e}", src.display()));
}
