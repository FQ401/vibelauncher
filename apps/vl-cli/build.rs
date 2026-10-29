//! Windows: embeds the launcher icon and the version resource (VERSIONINFO) into the executable.
//! Other targets: nothing.

fn main() -> std::io::Result<()> {
    const ICON: &str = "../../assets/icons/vibelauncher.ico";
    println!("cargo:rerun-if-changed={ICON}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    winresource::WindowsResource::new()
        .set_icon(ICON)
        .set("ProductName", "VibeLauncher")
        .set("FileDescription", "VibeLauncher command line")
        .set("InternalName", "vl-cli")
        .set("OriginalFilename", "vl-cli.exe")
        .set(
            "LegalCopyright",
            "Copyright (c) 2026 VibeLauncher contributors",
        )
        .compile()
}
