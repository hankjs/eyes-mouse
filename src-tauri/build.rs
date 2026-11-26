fn main() {
    println!("cargo:rerun-if-changed=src/accessibility_access.m");
    println!("cargo:rerun-if-changed=src/camera_access.m");
    println!("cargo:rerun-if-changed=src/snake_window.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/accessibility_access.m")
            .flag("-fobjc-arc")
            .compile("accessibility_access");
        cc::Build::new()
            .file("src/camera_access.m")
            .flag("-fobjc-arc")
            .compile("camera_access");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        cc::Build::new()
            .file("src/snake_window.m")
            .flag("-fobjc-arc")
            .compile("snake_window");
        println!("cargo:rustc-link-lib=framework=AppKit");
    }
    tauri_build::build();
}
