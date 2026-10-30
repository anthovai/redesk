// Embeds the icon and version information into the Windows executables.
fn main() {
    println!("cargo:rerun-if-changed=assets/aegiron.ico");
    // mingw: OpenH264's C++ runtime goes into the executables, which are all the installer
    // ships (a clean Windows has no libstdc++-6.dll). .cargo/config.toml stops cc from linking
    // it dynamically; as link arguments it comes after OpenH264's objects, followed by the C
    // runtime libraries it needs (rustc links with -nodefaultlibs and lists them earlier).
    println!("cargo:rerun-if-env-changed=CXXSTDLIB_x86_64_pc_windows_gnu");
    if std::env::var("TARGET").as_deref() == Ok("x86_64-pc-windows-gnu") {
        for lib in ["-Wl,--start-group", "-l:libstdc++.a", "-lgcc", "-lgcc_eh", "-l:libpthread.a", "-lmingw32", "-lmingwex", "-lmsvcrt", "-lkernel32", "-Wl,--end-group"] {
            println!("cargo:rustc-link-arg={lib}");
        }
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        // Cross-compiling from macOS/Linux: use the mingw resource compiler (Homebrew on Apple
        // silicon, /usr/local on Intel Macs, /usr/bin from the Linux mingw-w64 packages).
        if std::env::var("HOST").is_ok_and(|h| !h.contains("windows")) {
            let dir = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
                .into_iter()
                .find(|d| std::path::Path::new(d).join("x86_64-w64-mingw32-windres").exists())
                .unwrap_or("/opt/homebrew/bin");
            res.set_toolkit_path(dir).set_windres_path("x86_64-w64-mingw32-windres").set_ar_path("x86_64-w64-mingw32-ar");
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
