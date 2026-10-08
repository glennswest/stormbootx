//! Link flags every build of the UEFI binaries needs (#53).
//!
//! lld-link writes the wall-clock time into the PE header's `TimeDateStamp`,
//! so two links of the same commit, seconds apart, gave two digests.
//! `/Brepro` writes a hash of the output there instead. Here rather than in
//! RUSTFLAGS, so a plain `cargo build` (the sc-build command) gets it too.
//! The path remapping, which depends on where the build runs, is
//! `scripts/cargo-repro.sh`'s.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("uefi") {
        println!("cargo:rustc-link-arg=/Brepro");
    }
}
