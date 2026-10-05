use std::{env, path::PathBuf, process::Command};

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../zig");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    let status = Command::new("zig")
        // `-Dcpu=baseline`: without it Zig compiles for the build machine's own CPU and the library
        // dies with an illegal instruction on an older one.
        .args(["build", "-Doptimize=ReleaseSafe", "-Dcpu=baseline", "--prefix"])
        .arg(&out)
        .current_dir(&root)
        .status()
        .expect("zig not found in PATH");
    assert!(status.success(), "zig build failed");

    println!("cargo:rustc-link-search=native={}/lib", out.display());
    println!("cargo:rustc-link-lib=static=ustanzig");
    println!("cargo:rerun-if-changed=../../zig/src");
    println!("cargo:rerun-if-changed=../../zig/build.zig");
}
