fn main() {
    println!("cargo:rustc-check-cfg=cfg(kestrel_layer_shell)");
    println!("cargo:rerun-if-env-changed=KESTREL_LAYER_SHELL");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    if std::env::var_os("KESTREL_LAYER_SHELL").as_deref() == Some(std::ffi::OsStr::new("off")) {
        return;
    }
    match pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("gtk4-layer-shell-0")
    {
        Ok(library) => {
            for path in library.link_paths {
                println!("cargo:rustc-link-search=native={}", path.display());
            }
            println!("cargo:rustc-link-lib=gtk4-layer-shell");
            println!("cargo:rustc-cfg=kestrel_layer_shell");
        }
        Err(error)
            if std::env::var_os("KESTREL_LAYER_SHELL").as_deref()
                == Some(std::ffi::OsStr::new("required")) =>
        {
            panic!("gtk4-layer-shell is required but was not found: {error}");
        }
        Err(_) => {}
    }
}
