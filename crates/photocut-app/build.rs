fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
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
