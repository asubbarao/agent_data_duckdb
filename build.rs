use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CC");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
    {
        return;
    }

    let compiler = env::var("CC").unwrap_or_else(|_| "x86_64-w64-mingw32-gcc".to_string());
    let output = Command::new(&compiler)
        .arg("-print-libgcc-file-name")
        .output()
        .unwrap_or_else(|error| panic!("cannot run MinGW compiler '{compiler}': {error}"));
    if !output.status.success() {
        panic!("MinGW compiler '{compiler}' could not locate libgcc.a");
    }

    let libgcc = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !Path::new(&libgcc).is_file() {
        panic!("MinGW compiler '{compiler}' reported missing libgcc archive: {libgcc}");
    }

    let out_dir = env::var("OUT_DIR").expect("Cargo supplies OUT_DIR");
    let alias = Path::new(&out_dir).join("libgcc_eh.a");
    fs::copy(&libgcc, &alias).unwrap_or_else(|error| {
        panic!(
            "cannot provide MinGW libgcc_eh alias from '{}': {error}",
            libgcc
        )
    });
    println!("cargo:rustc-link-search=native={out_dir}");
}
