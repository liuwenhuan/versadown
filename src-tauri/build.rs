fn main() {
    // The quick build injects a content signature for the Vite source tree.
    // This forces Tauri to re-embed the matching dist assets without clearing
    // Cargo's incremental Rust target.
    println!("cargo:rerun-if-env-changed=ALLDOWNLOADER_FRONTEND_SIGNATURE");
    tauri_build::build()
}
