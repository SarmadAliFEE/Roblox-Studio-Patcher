use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=hooks/editor_background_windows.cpp");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir: PathBuf = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dll_path: PathBuf = out_dir.join("editor_background_windows.dll");

    let ok: bool = Command::new("x86_64-w64-mingw32-g++")
        .args(["-shared", "-std=c++17", "-static", "-static-libgcc", "-static-libstdc++", "-o"])
        .arg(&dll_path)
        .arg("hooks/editor_background_windows.cpp")
        .args(["-ldbghelp", "-lgdi32"])
        .status()
        .expect("couldn't run x86_64-w64-mingw32-g++ - install mingw-w64 to build the windows hook dll")
        .success();

    if !ok {
        panic!("x86_64-w64-mingw32-g++ failed to compile hooks/editor_background_windows.cpp");
    }
}
