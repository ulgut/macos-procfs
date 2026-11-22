fn main() {
    let sources = [
        "src/bridge/fuse.c",
        "src/macos/process.c",
        "src/macos/system.c",
    ];
    for path in sources
        .iter()
        .copied()
        .chain(["src/bridge/fuse.h", "src/macos/native.h"])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rustc-link-lib=framework=IOKit");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
    assert_eq!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("macos"),
        "mac_procfs requires macOS"
    );
    let fuse = pkg_config::Config::new()
        .atleast_version("2.9")
        .probe("fuse")
        .expect(
            "Install macFUSE 5.5+ including its development files; pkg-config must find fuse.pc",
        );
    let mut build = cc::Build::new();
    build.files(sources).includes(fuse.include_paths);
    for (name, value) in fuse.defines {
        build.define(&name, value.as_deref());
    }
    build
        .flag("-std=c11")
        .warnings(true)
        .compile("procfs_bridge");
}
