fn main() {
    // Re-embed the Windows icon resource whenever the icons are regenerated (`tauri icon`).
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
