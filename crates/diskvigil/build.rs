//! Embeds Windows version information (product name, version, copyright) into the executable.
//! Code signing (SignPath) requires consistent product metadata across builds.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env!("CARGO_PKG_VERSION");
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "DiskVigil")
        .set(
            "FileDescription",
            "DiskVigil - HDD, SSD and NVMe health monitor",
        )
        .set("InternalName", "diskvigil")
        .set("OriginalFilename", "diskvigil.exe")
        .set(
            "LegalCopyright",
            "Copyright (c) 2026 dmcinturf. MIT License.",
        )
        .set("ProductVersion", version)
        .set("FileVersion", version);
    if let Err(e) = res.compile() {
        // Fail loudly: a release without version info can't be signed.
        panic!("failed to embed Windows version resource: {e}");
    }
}
