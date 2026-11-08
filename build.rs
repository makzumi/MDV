fn main() {
    // Windows only: embed the app icon and version metadata into the .exe.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/mdv.ico");
        res.compile().expect("failed to embed Windows resources");
    }
}
