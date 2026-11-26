//! Embeds the app icon in `whipspot.exe` on Windows targets, so the taskbar and the window show it
//! (luneui has no window-icon API). On any other target this does nothing. Where no resource
//! compiler is found (a cross check from macOS, a Windows box without the SDK) it prints a cargo
//! warning and skips: it never fails the build.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/icon/whipspot.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icon/whipspot.ico");
    let Some(icon) = icon.to_str().map(str::to_owned) else {
        println!("cargo:warning=whipspot-app: the icon path is not UTF-8; the exe has no icon");
        return;
    };
    // `winresource` panics on a few unusual environments as well as returning an error.
    let embedded = std::panic::catch_unwind(|| {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(&icon)
            .set("ProductName", "Whipspot")
            .set("FileDescription", "Whipspot");
        res.compile()
    });
    match embedded {
        Ok(Ok(())) => {}
        _ => println!(
            "cargo:warning=whipspot-app: no Windows resource compiler found; the exe has no icon"
        ),
    }
}
