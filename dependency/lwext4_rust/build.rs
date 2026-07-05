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
    let lib_name = format!("lwext4-{}", std::env::var("CARGO_CFG_TARGET_ARCH").unwrap());

    println!("cargo:rustc-link-lib=static={}", lib_name);
    println!("cargo:rustc-link-search=native={}", lib_dir);
    println!("cargo:rerun-if-env-changed=LWEXT4_LIB_DIR");
    println!("cargo:rerun-if-changed=build.rs");
}
