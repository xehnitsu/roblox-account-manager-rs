// Embeds the icon and version info (shown in Explorer's Details tab).
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico")
            .set("ProductName", "Roblox Account Manager")
            .set("FileDescription", "Roblox Account Manager")
            .set("CompanyName", "xehnitsu")
            .set("LegalCopyright", "Copyright (c) 2026 xehnitsu")
            .set("OriginalFilename", "RobloxAccountManager.exe");
        res.compile().expect("failed to embed Windows resources");
    }
}
