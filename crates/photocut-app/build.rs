fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    #[cfg(target_os = "windows")]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        res.set("ProductName", "PhotoCut");
        res.set(
            "FileDescription",
            "PhotoCut — Recorte automático y restauración de fotos escaneadas",
        );
        if let Err(err) = res.compile() {
            panic!("failed to embed Windows icon resource: {err}");
        }
    }
}
