// SPDX-License-Identifier: GPL-3.0-or-later
//! Embeds the icon and the manifest into the Windows executable. Raw disk access
//! needs administrator rights, so the manifest requests them at startup.

fn main() {
    // `cfg(windows)` refers to the build host, the variable to the compile target.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=../../assets/icon.ico");
        println!("cargo:rerun-if-changed=windows.manifest");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        res.set_manifest_file("windows.manifest");
        res.set("ProductName", "USB Flash Memory Toolset");
        res.set("FileDescription", "USB Flash Memory Toolset");
        res.set("LegalCopyright", "Copyright (C) Jacob Dawid and contributors. GPL-3.0-or-later.");
        res.compile().expect("failed to embed Windows resources");
    }
}
