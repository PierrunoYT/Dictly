fn main() {
    println!("cargo:rerun-if-changed=assets/dictly.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/dictly.ico");
        resource
            .compile()
            .expect("cannot embed the Windows executable icon");
    }
}
