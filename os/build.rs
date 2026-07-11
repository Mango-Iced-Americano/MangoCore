//! Build script for the os kernel crate.
//!
//! Declares Cargo fingerprint dependencies so that `cargo build`
//! automatically recompiles when `MANGO_CMDLINE` changes, or when
//! the embedded initramfs cpio is rebuilt.
//!
//! Without this, `option_env!("MANGO_CMDLINE")` in `bootargs.rs`
//! is invisible to Cargo's change detection — the old value stays
//! baked into the binary.

fn main() {
    // Re-run if the build script itself changes
    println!("cargo:rerun-if-changed=build.rs");

    // Re-run if the embedded initramfs cpio changes (referenced by
    // initramfs-rv.S / initramfs-la.S via .incbin)
    println!("cargo:rerun-if-changed=../fs-img-dir/initramfs-rv.cpio");
    println!("cargo:rerun-if-changed=../fs-img-dir/initramfs-la.cpio");

    // Re-run if MANGO_CMDLINE changes between builds
    println!("cargo:rerun-if-env-changed=MANGO_CMDLINE");

    // Forward the command line to rustc so option_env! picks it up
    let cmdline = std::env::var("MANGO_CMDLINE")
        .unwrap_or_else(|_| String::from("mango.mode=normal"));

    // Safety: no newlines allowed in bootargs (would break parsing)
    assert!(
        !cmdline.contains('\n') && !cmdline.contains('\r'),
        "MANGO_CMDLINE must be a single-line string"
    );

    println!("cargo:rustc-env=MANGO_CMDLINE={cmdline}");
}
