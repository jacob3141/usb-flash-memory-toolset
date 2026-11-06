// SPDX-License-Identifier: GPL-3.0-or-later
//! Drives the real binary against image files. Touches no disks.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_usbflash"))
}

struct Image(PathBuf);

impl Image {
    fn new(name: &str, size: u64) -> Self {
        let p = std::env::temp_dir().join(format!("usbflash-cli-{name}-{}.img", std::process::id()));
        let f = fs::File::create(&p).unwrap();
        f.set_len(size).unwrap();
        Self(p)
    }
    fn arg(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        fs::remove_file(&self.0).ok();
    }
}

fn run(args: &[&str]) -> Output {
    bin().args(args).output().expect("binary runs")
}

fn json(o: &Output) -> serde_json::Value {
    assert!(o.status.success(), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).expect("valid JSON")
}

#[test]
fn reset_then_analyze_reports_the_new_filesystem() {
    let img = Image::new("reset", 64 << 20);
    let r = json(&run(&["--image", "--json", "reset", img.arg(), "--quick", "--fs", "exfat", "--label", "HELLO"]));
    assert_eq!(r["result"]["verified"], true);
    assert_eq!(r["result"]["data_erased"], false, "a quick reset must not claim erasure");

    let a = json(&run(&["--image", "--json", "analyze", img.arg()]));
    let part = &a["result"]["partitions"][0];
    assert_eq!(a["result"]["table_kind"], "mbr");
    assert_eq!(part["filesystem"]["name"], "exFAT");
    assert_eq!(part["filesystem"]["label"], "HELLO");
}

#[test]
fn wipe_erases_a_marker_in_the_image() {
    let img = Image::new("wipe", 4 << 20);
    let mut data = fs::read(&img.0).unwrap();
    data[1000..1006].copy_from_slice(b"SECRET");
    fs::write(&img.0, &data).unwrap();

    let r = json(&run(&["--image", "--json", "wipe", img.arg(), "--mode", "random"]));
    assert_eq!(r["result"]["status"], "verified");
    let after = fs::read(&img.0).unwrap();
    assert!(!after.windows(6).any(|w| w == b"SECRET"));
    assert_ne!(after, vec![0u8; after.len()], "random mode must not leave zeros");
}

#[test]
fn backup_and_restore_through_the_binary() {
    let src = Image::new("src", 2 << 20);
    let dst = Image::new("dst", 2 << 20);
    let backup = std::env::temp_dir().join(format!("usbflash-cli-backup-{}.img", std::process::id()));
    fs::remove_file(&backup).ok();
    let mut data = fs::read(&src.0).unwrap();
    data[4096] = 0x42;
    fs::write(&src.0, &data).unwrap();

    let b = backup.to_str().unwrap();
    json(&run(&["--image", "--json", "backup", src.arg(), b]));
    let r = json(&run(&["--image", "--json", "restore", b, dst.arg()]));
    assert_eq!(r["result"]["verify_failures"]["ranges"].as_array().unwrap().len(), 0);
    assert_eq!(fs::read(&dst.0).unwrap(), data);
    fs::remove_file(&backup).ok();
}

#[test]
fn capacity_test_passes_on_an_image() {
    let img = Image::new("cap", 4 << 20);
    let r = json(&run(&["--image", "--json", "test", img.arg(), "--full"]));
    assert_eq!(r["result"]["looks_fake"], false);
    assert_eq!(r["result"]["verified_bytes"], 4 << 20);
}

#[test]
fn unknown_devices_are_rejected_before_any_write() {
    let o = run(&["wipe", "does-not-exist", "--yes"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no device"));
}

/// Deliberately runs without `--yes` and with empty stdin. Even if the guard ever
/// regressed, the confirmation step stops the command, so this test can never
/// touch a disk of the machine it runs on.
#[test]
fn protected_disks_are_refused_by_the_policy() {
    let list = run(&["--json", "list", "--all"]);
    if !list.status.success() {
        return; // No device access in this environment.
    }
    let devices: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    for d in devices.as_array().unwrap() {
        if d["is_system"] == true || !matches!(d["bus"].as_str(), Some("usb" | "sd" | "mmc")) {
            let path = d["path"].as_str().unwrap();
            let o = bin().args(["wipe", path, "--allow-fixed"]).stdin(Stdio::null()).output().unwrap();
            let err = String::from_utf8_lossy(&o.stderr);
            assert_eq!(o.status.code(), Some(1), "{path}");
            assert!(err.contains("holds the running") || err.contains("only USB"), "{path}: {err}");
        }
    }
}

#[test]
fn health_needs_a_real_device() {
    let img = Image::new("health", 1 << 20);
    assert_eq!(run(&["--image", "health", img.arg()]).status.code(), Some(1));
}
