use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    if env::var_os("CARGO_FEATURE_SDK_FIXTURE").is_none() {
        return;
    }

    println!("cargo:rerun-if-changed=tests/fixtures/sdk-component/Cargo.toml");
    println!("cargo:rerun-if-changed=tests/fixtures/sdk-component/Cargo.lock");
    println!("cargo:rerun-if-changed=tests/fixtures/sdk-component/src");
    println!("cargo:rerun-if-changed=../../core/ext-api/wit");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let fixture_dir = manifest_dir.join("tests/fixtures/sdk-component");
    let fixture_manifest = fixture_dir.join("Cargo.toml");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let fixture_target_dir = out_dir.join("sdk-fixture-target");
    let fixture_wasm = fixture_target_dir.join("wasm32-wasip2/release/sdk_component_fixture.wasm");
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args([
            "build",
            "--locked",
            "--manifest-path",
            fixture_manifest.to_str().expect("fixture manifest path"),
            "--target",
            "wasm32-wasip2",
            "--release",
        ])
        .env("CARGO_TARGET_DIR", &fixture_target_dir)
        .status()
        .expect("spawn SDK fixture build");
    assert!(
        status.success(),
        "SDK fixture build failed with status {status}"
    );

    let out_path = out_dir.join("sdk_component_fixture.wasm");
    fs::copy(&fixture_wasm, &out_path).unwrap_or_else(|error| {
        panic!(
            "copy SDK fixture from {} to {}: {error}",
            fixture_wasm.display(),
            out_path.display()
        )
    });
    println!(
        "cargo:rustc-env=SHILPO_SDK_FIXTURE_WASM={}",
        out_path.display()
    );
}
