fn main() {
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=assets/turbo-app.exe.manifest");
    println!("cargo:rerun-if-changed=assets/turbo.rc");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_UI");

    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows") {
        #[cfg(windows)]
        {
            let _ = embed_resource::compile("assets/turbo.rc", embed_resource::NONE);
        }
        #[cfg(not(windows))]
        {
            // Cross-compilação a partir do Linux: o windres do mingw embute o manifesto.
            let out = std::env::var("OUT_DIR").unwrap();
            let status = std::process::Command::new("x86_64-w64-mingw32-windres")
                .args(["assets/turbo.rc", "-O", "coff", "-o"])
                .arg(format!("{out}/turbo-res.o"))
                .status();
            if let Ok(status) = status {
                if status.success() {
                    println!("cargo:rustc-link-arg={out}/turbo-res.o");
                }
            }
        }
    }

    compile_ui();
}

#[cfg(feature = "ui")]
fn compile_ui() {
    slint_build::compile("ui/app.slint").expect("compilar ui/app.slint");
}

#[cfg(not(feature = "ui"))]
fn compile_ui() {}
