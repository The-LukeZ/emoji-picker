use std::path::Path;

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let generated = manifest.join("../assets/generated");
    let bundle = generated.join("emoji.bin");
    let icon = generated.join("app.ico");
    println!("cargo:rerun-if-changed={}", bundle.display());
    println!("cargo:rerun-if-changed={}", icon.display());
    if !bundle.exists() || !icon.exists() {
        panic!("assets/generated is missing: run `cargo xtask assets` first");
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id(&icon.to_string_lossy(), "1");
        res.compile().expect("embedding the app icon failed");
    }
    // slint_build::compile is added with the picker UI (Task 12).
}
