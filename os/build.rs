//! Build script for the os kernel crate.
//!
//! Declares Cargo fingerprint dependencies so that `cargo build`
//! automatically recompiles when `MANGO_CMDLINE` changes, or when
//! the embedded initramfs cpio is rebuilt.
//!
//! Without this, `option_env!("MANGO_CMDLINE")` in `bootargs.rs`
//! is invisible to Cargo's change detection — the old value stays
//! baked into the binary.

use std::{env, fs, path::PathBuf};

fn required_path(name: &str) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{} is required when initramfs is enabled", name))
}

fn assembly_path(path: &std::path::Path) -> String {
    let value = path.to_string_lossy();
    assert!(
        !value.contains('\n') && !value.contains('\r'),
        "initramfs artifact path must not contain a newline"
    );
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn required_file(path: PathBuf, label: &str) -> PathBuf {
    if !path.is_file() {
        panic!("{label} does not exist: {}", path.display());
    }
    println!("cargo:rerun-if-changed={}", path.display());
    path
}

fn write_assembly(filename: &str, source: String) {
    let generated = required_path("OUT_DIR").join(filename);
    fs::write(&generated, source).unwrap_or_else(|error| {
        panic!(
            "failed to generate assembly {}: {error}",
            generated.display()
        )
    });
}

fn generate_initramfs_assembly() {
    println!("cargo:rerun-if-env-changed=MANGO_INITRAMFS_CPIO");
    println!("cargo:rerun-if-env-changed=MANGO_USER_OUTPUT_ROOT");
    println!("cargo:rerun-if-env-changed=MANGO_USER_OUTPUT_MODE");

    if env::var_os("CARGO_FEATURE_INITRAMFS").is_none() {
        return;
    }

    // When running outside the Make build (e.g. cargo clippy, cargo test),
    // the initramfs env vars are not set.  Use an empty cpio so the build
    // succeeds — static analysis doesn't need a real initramfs.
    let use_dummy = env::var_os("MANGO_INITRAMFS_CPIO").is_none();
    let cpio: PathBuf = if use_dummy {
        let dummy = required_path("OUT_DIR").join("__dummy_initramfs.cpio");
        if !dummy.is_file() {
            std::fs::write(&dummy, "dummy").unwrap();
        }
        dummy
    } else {
        required_path("MANGO_INITRAMFS_CPIO")
    };
    let user_output_root: PathBuf = if use_dummy {
        required_path("OUT_DIR")
    } else {
        required_path("MANGO_USER_OUTPUT_ROOT")
    };
    if !user_output_root.is_dir() {
        panic!(
            "declared user output root does not exist: {}",
            user_output_root.display()
        );
    }

    println!("cargo:rerun-if-changed={}", user_output_root.display());

    let cpio = required_file(cpio, "initramfs CPIO artifact");
    let source = format!(
        ".section .data\n.global sinitramfs\n.global einitramfs\n.align 12\nsinitramfs:\n.incbin \"{}\"\neinitramfs:\n.align 12\n",
        assembly_path(&cpio),
    );
    write_assembly("initramfs.S", source);
}

fn main() {
    // Re-run if the build script itself changes
    println!("cargo:rerun-if-changed=build.rs");

    generate_initramfs_assembly();

    // Re-run if MANGO_CMDLINE changes between builds
    println!("cargo:rerun-if-env-changed=MANGO_CMDLINE");

    // Forward the command line to rustc so option_env! picks it up
    let cmdline =
        std::env::var("MANGO_CMDLINE").unwrap_or_else(|_| String::from("mango.mode=normal"));

    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("riscv64") {
        let linker_script = match (
            std::env::var_os("CARGO_FEATURE_BOARD_RVQEMU").is_some(),
            std::env::var_os("CARGO_FEATURE_BOARD_VF2").is_some(),
        ) {
            (true, false) => {
                println!("cargo:rerun-if-changed=src/hal/arch/riscv/linker-rvqemu.ld");
                "src/hal/arch/riscv/linker-rvqemu.ld"
            }
            (false, true) => {
                println!("cargo:rerun-if-changed=src/hal/arch/riscv/linker-vf2.ld");
                "src/hal/arch/riscv/linker-vf2.ld"
            }
            (false, false) => panic!("RV64 build requires exactly one board feature"),
            (true, true) => panic!("RV64 build requires exactly one board feature"),
        };

        println!("cargo:rustc-link-arg=-T{linker_script}");
    }

    // Safety: no newlines allowed in bootargs (would break parsing)
    assert!(
        !cmdline.contains('\n') && !cmdline.contains('\r'),
        "MANGO_CMDLINE must be a single-line string"
    );

    println!("cargo:rustc-env=MANGO_CMDLINE={cmdline}");
}
