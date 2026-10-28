// Embeds the icon and version information into the Windows executables.
fn main() {
    println!("cargo:rerun-if-changed=assets/aegiron.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        // Cross-compiling from macOS/Linux: use the mingw resource compiler.
        if std::env::var("HOST").is_ok_and(|h| !h.contains("windows")) {
            res.set_toolkit_path("/opt/homebrew/bin").set_windres_path("x86_64-w64-mingw32-windres").set_ar_path("x86_64-w64-mingw32-ar");
        }
        res.set_icon("assets/aegiron.ico")
            .set("ProductName", "Aegiron")
            .set("FileDescription", "Aegiron remote desktop agent")
            .set("CompanyName", "Aegiron")
            .set("LegalCopyright", "Aegiron");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}
