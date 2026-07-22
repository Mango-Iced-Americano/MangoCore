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

    let cpio = required_path("MANGO_INITRAMFS_CPIO");
    let user_output_root = required_path("MANGO_USER_OUTPUT_ROOT");
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

fn generate_preload_assembly() {
    let initramfs_enabled = env::var_os("CARGO_FEATURE_INITRAMFS").is_some();
    let preload_enabled = env::var_os("CARGO_FEATURE_PRELOAD_PAYLOADS").is_some();
    let legacy_arch_enabled = env::var_os("CARGO_FEATURE_RISCV").is_some()
        || env::var_os("CARGO_FEATURE_LOONGARCH64").is_some();
    if !preload_enabled && (initramfs_enabled || !legacy_arch_enabled) {
        return;
    }

    let user_output_root = required_path("MANGO_USER_OUTPUT_ROOT");
    let user_output_mode = env::var("MANGO_USER_OUTPUT_MODE")
        .unwrap_or_else(|_| panic!("MANGO_USER_OUTPUT_MODE is required for preload payloads"));
    let (target, tool_arch, compat_library) = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("riscv64") => (
            "riscv64gc-unknown-none-elf",
            "riscv64",
            "ltp_proto_compat-rv.so",
        ),
        Ok("loongarch64") => (
            "loongarch64-unknown-linux-gnu",
            "loongarch64",
            "ltp_proto_compat-la.so",
        ),
        Ok(arch) => panic!("preload payloads do not support target architecture: {}", arch),
        Err(_) => panic!("CARGO_CFG_TARGET_ARCH is required for preload payloads"),
    };
    let user_bin = user_output_root.join(target).join(user_output_mode);
    let repo_root = required_path("CARGO_MANIFEST_DIR")
        .parent()
        .unwrap_or_else(|| panic!("os manifest directory has no repository parent"))
        .to_path_buf();
    let initproc = required_file(user_bin.join("initproc"), "preload initproc");
    let fs_test = required_file(user_bin.join("fs_test"), "preload fs_test");
    let ltprunner = required_file(user_bin.join("ltprunner"), "preload ltprunner");
    let bash = required_file(
        repo_root.join("user/tools").join(tool_arch).join("bin/bash"),
        "preload bash",
    );
    let busybox = required_file(
        repo_root
            .join("user/tools")
            .join(tool_arch)
            .join("bin/busybox"),
        "preload busybox",
    );
    let os_config = required_file(repo_root.join("os_test.conf"), "preload os_test.conf");
    let ltp_compat = required_file(
        repo_root
            .join("user/tools")
            .join(tool_arch)
            .join("lib")
            .join(compat_library),
        "preload LTP compatibility library",
    );
    let source = format!(
        ".section .data\n.global sinitproc\n.global einitproc\n.align 12\nsinitproc:\n.incbin \"{}\"\neinitproc:\n.align 12\n\n.section .data\n.global sbash\n.global ebash\n.align 12\nsbash:\n.incbin \"{}\"\nebash:\n.align 12\n\n.section .data\n.global sbusybox\n.global ebusybox\n.align 12\nsbusybox:\n.incbin \"{}\"\nebusybox:\n.align 12\n\n.section .data\n.global sosconfig\n.global eosconfig\n.align 12\nsosconfig:\n.incbin \"{}\"\neosconfig:\n.align 12\n\n.section .data\n.global sfstest\n.global efstest\n.align 12\nsfstest:\n.incbin \"{}\"\nefstest:\n.align 12\n\n.section .data\n.global sltpcompat\n.global eltpcompat\n.align 12\nsltpcompat:\n.incbin \"{}\"\neltpcompat:\n.align 12\n\n.section .data\n.global sltprunner\n.global eltprunner\n.align 12\nsltprunner:\n.incbin \"{}\"\neltprunner:\n.align 12\n",
        assembly_path(&initproc),
        assembly_path(&bash),
        assembly_path(&busybox),
        assembly_path(&os_config),
        assembly_path(&fs_test),
        assembly_path(&ltp_compat),
        assembly_path(&ltprunner),
    );
    write_assembly("preload_app.S", source);
}

fn main() {
    // Re-run if the build script itself changes
    println!("cargo:rerun-if-changed=build.rs");

    generate_initramfs_assembly();
    generate_preload_assembly();

    // Re-run if MANGO_CMDLINE changes between builds
    println!("cargo:rerun-if-env-changed=MANGO_CMDLINE");

    // Forward the command line to rustc so option_env! picks it up
    let cmdline = std::env::var("MANGO_CMDLINE")
        .unwrap_or_else(|_| String::from("mango.mode=normal"));

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
