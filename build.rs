// Build scripts SHOULD fail loudly: a panic here aborts the build with the message, which is
// exactly what we want if env/OUT_DIR/file writes go wrong — so unwrap/expect are correct.
#![allow(clippy::unwrap_used, clippy::expect_used)]

// Build script. Embeds the Windows app icon + version metadata into kyde.exe, and on macOS
// embeds a minimal `Info.plist` into the binary so the *unbundled* executable (run as
// `target/release/kyde` via the `ky` shell function) shows "Kyde" in the dock / menu bar
// instead of the raw lowercase executable name. No-op on Linux.
fn main() {
    #[cfg(target_os = "macos")]
    {
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Kyde</string>
<key>CFBundleDisplayName</key><string>Kyde</string>
<key>CFBundleIdentifier</key><string>com.kyde.Kyde</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
<key>CFBundleVersion</key><string>{version}</string>
</dict></plist>"#
        );
        let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("Info.plist");
        std::fs::write(&out, plist).expect("write Info.plist");
        // Link the plist into the binary's `__TEXT,__info_plist` section — macOS reads it for
        // unbundled apps to name the dock tile.
        println!(
            "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            out.display()
        );
    }

    #[cfg(windows)]
    {
        // Guard on the file so the build never breaks if the icon is absent.
        if std::path::Path::new("assets/AppIcon.ico").exists() {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("assets/AppIcon.ico");
            res.set("ProductName", "Kyde");
            res.set("FileDescription", "Fast native git commit/diff tool");
            // Versions come from Cargo (single source of truth — see RELEASING.md).
            res.set("FileVersion", env!("CARGO_PKG_VERSION"));
            res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
            if let Err(e) = res.compile() {
                println!("cargo:warning=winresource failed to embed icon: {e}");
            }
        }
    }
}
