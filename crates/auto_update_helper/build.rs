fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=manifest.xml");

        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let workspace_root = std::path::Path::new(&manifest_dir)
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let icon_path = workspace_root.join("crates/zed/resources/windows/app-icon.ico");

        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file("manifest.xml");
        res.set_icon(icon_path.to_str().unwrap());

        if let Some(explicit_rc_toolkit_path) = std::env::var("ZED_RC_TOOLKIT_PATH").ok() {
            res.set_toolkit_path(explicit_rc_toolkit_path.as_str());
        }

        if let Err(e) = res.compile() {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}
