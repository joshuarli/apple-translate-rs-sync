use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from("./generated");
    let bridges = vec!["src/ffi.rs"];

    for path in &bridges {
        println!("cargo:rerun-if-changed={}", path);
    }
    println!("cargo:rerun-if-changed=src/TranslationWrapper.swift");

    // Step 1: Generate Swift/C glue code from the bridge module.
    // write_all_concatenated creates: {out_dir}/{crate_name}/{crate_name}.swift + .h
    // plus {out_dir}/SwiftBridgeCore.swift + SwiftBridgeCore.h (runtime support)
    let _ = std::fs::create_dir_all(&out_dir);
    let crate_name = env!("CARGO_PKG_NAME");
    swift_bridge_build::parse_bridges(bridges).write_all_concatenated(&out_dir, crate_name);

    // Step 2: Compile all Swift sources into a static library
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let is_release = profile == "release";

    let gen_dir = out_dir.join(crate_name);
    let generated_swift = gen_dir.join(format!("{}.swift", crate_name));
    let swift_bridge_core_header = out_dir.join("SwiftBridgeCore.h");
    let swift_bridge_core_swift = out_dir.join("SwiftBridgeCore.swift");

    let lib_name = "apple_translate_rs_sync_swift";
    let lib_path = out_dir.join(format!("lib{}.a", lib_name));

    let mut cmd = Command::new("swiftc");
    cmd.arg("-emit-library")
        .arg("-static")
        .arg("-o")
        .arg(&lib_path)
        .arg("-module-name")
        .arg("MacOSTranslateSwift")
        .arg("-import-objc-header")
        .arg(&swift_bridge_core_header)
        .arg("src/TranslationWrapper.swift")
        .arg(&swift_bridge_core_swift)
        .arg(&generated_swift);

    if is_release {
        cmd.arg("-O");
    }

    let output = cmd.output().expect("Failed to run swiftc");
    if !output.status.success() {
        panic!(
            "swiftc failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Step 3: Link the static library into the Rust build
    println!("cargo:rustc-link-lib=static={}", lib_name);
    println!("cargo:rustc-link-search=native={}", out_dir.display());

    // macOS: add rpath for Swift runtime dylibs in the dyld shared cache.
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
