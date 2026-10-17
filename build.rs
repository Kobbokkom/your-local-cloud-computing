use std::path::Path;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(dashboard_assets)");
    if Path::new("dashboard/dist/index.html").exists() {
        println!("cargo:rustc-cfg=dashboard_assets");
    }
    println!("cargo:rerun-if-changed=dashboard/dist/index.html");
}
