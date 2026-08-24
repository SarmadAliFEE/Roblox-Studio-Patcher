use std::env;
use std::path::PathBuf;
use std::process::Command;

fn build_hook_dll(out_dir: &PathBuf, source: &str, dll_name: &str, extra_libs: &[&str]) {
    println!("cargo:rerun-if-changed={source}");

    let dll_path: PathBuf = out_dir.join(dll_name);
    let ok: bool = Command::new("x86_64-w64-mingw32-g++")
        .args([
            "-shared", "-std=c++17", "-static", "-static-libgcc", "-static-libstdc++",
            "-ffunction-sections", "-fdata-sections", "-Wl,--gc-sections", "-s", "-o",
        ])
        .arg(&dll_path)
        .arg(source)
        .args(extra_libs)
        .status()
        .expect("couldn't run x86_64-w64-mingw32-g++ - install mingw-w64 to build the windows hook dlls")
        .success();

    if !ok {
        panic!("x86_64-w64-mingw32-g++ failed to compile {source}");
    }
}

fn build_studio_hook(out_dir: &PathBuf, target: &str, target_os: &str) {
    println!("cargo:rerun-if-changed=crates/studio-hook/src");
    println!("cargo:rerun-if-changed=crates/studio-hook/build.rs");
    println!("cargo:rerun-if-changed=crates/studio-hook/Cargo.toml");

    let hook_target_dir: PathBuf = out_dir.join("studio-hook-build");
    let cargo: String = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let ok: bool = Command::new(cargo)
        .args([
            "build", "--release",
            "--manifest-path", "crates/studio-hook/Cargo.toml",
            "--target", target,
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
    let src: PathBuf = hook_target_dir.join(target).join("release").join(built);
    std::fs::copy(&src, out_dir.join(embedded))
        .unwrap_or_else(|e| panic!("couldn't stage studio-hook payload from {}: {e}", src.display()));
}

fn main() {
    let out_dir: PathBuf = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target: String = env::var("TARGET").unwrap();
    let target_os: String = env::var("CARGO_CFG_TARGET_OS").unwrap();

    build_studio_hook(&out_dir, &target, &target_os);

    if target_os != "windows" {
        println!("cargo:rerun-if-changed=hooks/editor_background/editor_background_windows.cpp");
        println!("cargo:rerun-if-changed=hooks/window_transparency/window_transparency_windows.cpp");
        return;
    }

    build_hook_dll(
        &out_dir,
        "hooks/editor_background/editor_background_windows.cpp",
        "editor_background_windows.dll",
        &["-ldbghelp", "-lgdi32"],
    );
    build_hook_dll(
        &out_dir,
        "hooks/window_transparency/window_transparency_windows.cpp",
        "window_transparency_windows.dll",
        &["-luser32"],
    );
}
