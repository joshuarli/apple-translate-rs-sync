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
    println!("cargo:rerun-if-changed=src/EngineHelper.m");
    println!("cargo:rerun-if-changed=src/translation-worker.m");

    // Step 1: Generate Swift/C glue code from the bridge module.
    let _ = std::fs::create_dir_all(&out_dir);
    let crate_name = env!("CARGO_PKG_NAME");
    swift_bridge_build::parse_bridges(bridges).write_all_concatenated(&out_dir, crate_name);

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let is_release = profile == "release";

    let gen_dir = out_dir.join(crate_name);
    let generated_swift = gen_dir.join(format!("{}.swift", crate_name));
    let swift_bridge_core_header = out_dir.join("SwiftBridgeCore.h");
    let swift_bridge_core_swift = out_dir.join("SwiftBridgeCore.swift");

    let lib_name = "apple_translate_rs_sync_swift";
    let lib_path = out_dir.join(format!("lib{}.a", lib_name));

    // Step 2: Compile ObjC helper to object file.
    let engine_o = out_dir.join("EngineHelper.o");
    let mut cc = Command::new("clang");
    cc.arg("-c")
        .arg("src/EngineHelper.m")
        .arg("-o")
        .arg(&engine_o)
        .arg("-fobjc-arc");
    if is_release {
        cc.arg("-O2");
    }
    let output = cc.output().expect("Failed to run clang for EngineHelper.m");
    if !output.status.success() {
        panic!(
            "clang failed for EngineHelper.m:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Step 3: Compile Swift sources + link with EngineHelper.o into static lib.
    // swiftc -emit-library -static accepts both .swift and .o inputs.
    let mut sc = Command::new("swiftc");
    sc.arg("-emit-library")
        .arg("-static")
        .arg("-o")
        .arg(&lib_path)
        .arg("-module-name")
        .arg("MacOSTranslateSwift")
        .arg("-import-objc-header")
        .arg(&swift_bridge_core_header)
        .arg("src/TranslationWrapper.swift")
        .arg(&swift_bridge_core_swift)
        .arg(&generated_swift)
        .arg(&engine_o); // Link in the ObjC helper
    if is_release {
        sc.arg("-O");
    }
    let output = sc.output().expect("Failed to run swiftc");
    if !output.status.success() {
        panic!(
            "swiftc failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Step 4: Compile the standalone translation worker binary.
    let worker_path = out_dir.join("translation-worker");
    let mut worker_cc = Command::new("clang");
    worker_cc
        .arg("-framework")
        .arg("Foundation")
        .arg("-fobjc-arc")
        .arg("-o")
        .arg(&worker_path)
        .arg("src/translation-worker.m");
    if is_release {
        worker_cc.arg("-O2");
    }
    let output = worker_cc
        .output()
        .expect("Failed to compile translation-worker");
    if !output.status.success() {
        panic!(
            "translation-worker compilation failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Step 5: Link into Rust build.
    println!("cargo:rustc-link-lib=static={}", lib_name);
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
