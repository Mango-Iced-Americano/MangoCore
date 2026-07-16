fn main() {
    // Gracefully skip linking when LWEXT4_LIB_DIR is not set (e.g. cargo check).
    // The Makefile sets this env var before `cargo build`.
    let lib_dir = match std::env::var("LWEXT4_LIB_DIR") {
        Ok(dir) => dir,
        Err(_) => {
            println!("cargo:warning=LWEXT4_LIB_DIR not set, skipping native link (cargo check ok)");
            return;
        }
    };
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let lib_name = format!("lwext4-{}", target_arch);

    println!("cargo:rustc-link-lib=static:+whole-archive={}", lib_name);
    println!("cargo:rustc-link-search=native={}", lib_dir);
    println!("cargo:rerun-if-env-changed=LWEXT4_LIB_DIR");
    println!("cargo:rerun-if-changed=build.rs");

    // ── Invalidation: C sources ────────────────────────────────
    let c_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("c");
    let lwext4_src = c_root.join("lwext4").join("src");
    let lwext4_inc = c_root.join("lwext4").join("include");

    // Source files
    if let Ok(entries) = std::fs::read_dir(&lwext4_src) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map_or(false, |e| e == "c") {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
    }
    // Header files (top-level)
    for inc_dir in &[lwext4_inc.clone(), lwext4_inc.join("misc")] {
        if let Ok(entries) = std::fs::read_dir(inc_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().map_or(false, |e| e == "h") {
                    println!("cargo:rerun-if-changed={}", p.display());
                }
            }
        }
    }

    // ── Invalidation: cmake / project inputs ──────────────────
    for cmake in &[
        c_root.join("lwext4").join("CMakeLists.txt"),
        c_root.join("lwext4").join("src").join("CMakeLists.txt"),
        c_root.join("elf-linux-gnu.cmake"),
        c_root.join("ulibc.c"),
    ] {
        println!("cargo:rerun-if-changed={}", cmake.display());
    }

    // ── Invalidation: archive file itself ──────────────────────
    // When `make` rebuilds the .a, Cargo re-runs this script.  We embed the
    // archive mtime as a rustc-env so the build-script fingerprint changes
    // and Cargo re-links.
    let archive_path = std::path::Path::new(&lib_dir)
        .join(format!("lib{}.a", lib_name));
    println!("cargo:rerun-if-changed={}", archive_path.display());

    if let Ok(meta) = std::fs::metadata(&archive_path) {
        if let Ok(mtime) = meta.modified() {
            let ts = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            println!("cargo:rustc-env=LWEXT4_ARCHIVE_TS={}", ts);
        }
    }
}
