use std::io::Result;

fn main() -> Result<()> {
    prost_build::Config::new()
        .btree_map(["."])
        .compile_protos(
            &[
                "proto/GDISmart.proto",
                "proto/GDIEventSharing.proto",
                "proto/GDILaunchMonitor.proto",
            ],
            &["proto/"],
        )?;

    // Declare custom cfg names so rustc doesn't warn about them.
    println!("cargo::rustc-check-cfg=cfg(ble_backend_bluez)");
    println!("cargo::rustc-check-cfg=cfg(ble_backend_btleplug)");

    // When `ble` feature is active, auto-select backend by target OS.
    if std::env::var("CARGO_FEATURE_BLE").is_ok() {
        let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        if target_os == "linux" {
            println!("cargo::rustc-cfg=ble_backend_bluez");
        } else {
            println!("cargo::rustc-cfg=ble_backend_btleplug");
        }
    }

    Ok(())
}
