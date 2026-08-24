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

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        println!("cargo:rerun-if-changed=hooks/editor_background/editor_background_windows.cpp");
        println!("cargo:rerun-if-changed=hooks/window_transparency/window_transparency_windows.cpp");
        return;
    }

    let out_dir: PathBuf = PathBuf::from(env::var("OUT_DIR").unwrap());
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
