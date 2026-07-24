//! Build script for the os kernel crate.
//!
//! Declares Cargo fingerprint dependencies so that `cargo build`
//! automatically recompiles when `MANGO_CMDLINE` or `MANGO_CORE_NUM`
//! changes, or when the embedded initramfs cpio is rebuilt.
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
    println!("cargo:rerun-if-changed=../fs-img-dir/initramfs-regression-rv.cpio");
    println!("cargo:rerun-if-changed=../fs-img-dir/initramfs-la.cpio");

    // Re-run if MANGO_CMDLINE changes between builds
    println!("cargo:rerun-if-env-changed=MANGO_CMDLINE");

    // CORE_NUM is a build-time topology contract. Tracking it here prevents
    // Cargo from reusing a kernel compiled for a different QEMU CPU count.
    println!("cargo:rerun-if-env-changed=MANGO_CORE_NUM");
    let core_num = std::env::var("MANGO_CORE_NUM").unwrap_or_else(|_| String::from("1"));
    assert!(
        matches!(core_num.as_str(), "1" | "2" | "4" | "8"),
        "MANGO_CORE_NUM must be one of 1, 2, 4, or 8; got {:?}",
        core_num
    );
    println!("cargo:rustc-env=MANGO_CORE_NUM={core_num}");

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
