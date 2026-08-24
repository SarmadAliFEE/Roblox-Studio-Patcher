use std::path::{Path, PathBuf};

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

fn main() {
    let luau = PathBuf::from("vendor/luau");
    println!("cargo:rerun-if-changed={}", luau.display());

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .opt_level(2)
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
