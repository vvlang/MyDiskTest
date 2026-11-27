fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "MyDiskTest Next");
        res.set("CompanyName", "AIYeon");
        res.set(
            "LegalCopyright",
            "Copyright © 2026 AIYeon. All rights reserved.",
        );
        res.set("OriginalFilename", "MyDiskTest.exe");
        res.set(
            "FileDescription",
            "Cross-platform Disk Capacity, Error Scan & Benchmark Utility",
        );
        res.compile().expect("Failed to compile Windows icon resource");
    }
}
