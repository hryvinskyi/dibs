//! Names the target triple this binary is built for, so a self-update fetches that platform's
//! release archive, and rebuilds when the trusted release key changes.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=DIBS_TARGET={target}");
    println!("cargo:rerun-if-changed=../../keys/release.pub");
    println!("cargo:rerun-if-changed=build.rs");
}
