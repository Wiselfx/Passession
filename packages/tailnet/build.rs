use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=PASEO_TAILSCALE_STATIC_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("ios") {
        return;
    }
    let dir = PathBuf::from(
        std::env::var_os("PASEO_TAILSCALE_STATIC_DIR")
            .expect("iOS requires PASEO_TAILSCALE_STATIC_DIR from cargo xtask build-gui"),
    );
    let archive = dir.join("libpaseo_tailscale.a");
    assert!(archive.is_file(), "missing {}", archive.display());
    println!("cargo:rerun-if-changed={}", archive.display());
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=paseo_tailscale");
    for framework in [
        "CoreFoundation",
        "Security",
        "SystemConfiguration",
        "Foundation",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    println!("cargo:rustc-link-lib=resolv");
}
