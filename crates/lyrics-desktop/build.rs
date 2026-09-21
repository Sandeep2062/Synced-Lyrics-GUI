fn main() {
    slint_build::compile("ui/app.slint").expect("failed to compile Slint UI");

    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Synced Lyrics");
        res.set("FileDescription", "Synced Lyrics GUI");
        res.set("LegalCopyright", "Synced Lyrics GUI contributors");
        let _ = res.compile();
    }
}
