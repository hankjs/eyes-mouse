fn main() {
    println!("cargo:rerun-if-changed=src/camera_access.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/camera_access.m")
            .flag("-fobjc-arc")
            .compile("camera_access");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
    }
    tauri_build::build();
}
