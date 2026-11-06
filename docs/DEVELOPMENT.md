# Developer guide

## Prerequisites

- **Rust**, stable, at least the `rust-version` from `Cargo.toml`. Install via [rustup](https://rustup.rs).
- **Windows:** Visual Studio Build Tools with the MSVC toolset and a Windows SDK. Unattended:

  ```bash
  winget install --id Microsoft.VisualStudio.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.26100"
  ```

- **Linux, for the GUI:**

  ```bash
  sudo apt install libxkbcommon-dev libgl1-mesa-dev libwayland-dev libx11-dev
  ```

- **Optional, for `scripts/fsck-check.sh`:** `dosfstools`, `exfatprogs`, `fdisk`.

## Building and running

```bash
cargo build --workspace
cargo run -p usbflash-cli -- list --all
```

The GUI's manifest requests administrator rights on Windows, so start it from an elevated terminal:

```bash
cargo run -p usbflash-gui
```

Release builds, with a statically linked C runtime on Windows:

```bash
cargo build --workspace --release
```

## Working on the interface

The `demo` feature shows made-up devices and simulated jobs. That build lists no real disks and
never calls a device operation, so it is safe to use, and needs no hardware:

```bash
cargo run -p usbflash-gui --features demo -- --tab health --theme light
```

`--tab` is `overview`, `health`, `erase` or `image`. `--theme` is `light` or `dark`. `--device` selects
the n-th made-up device, and `--scene` is one of `running`, `result`, `failed`, `dialog`, `dialog-typed`,
`empty` or `fresh`. On Windows, start the binary with `__COMPAT_LAYER=RunAsInvoker` set to skip the
elevation prompt.

The screenshots in `docs/images` come from this build. Do not use real devices for them, since names
and serial numbers would end up in the repository.

## Working safely

You do not need a real stick for most work, and you should not use your disks for experiments:

```bash
truncate -s 2G test.img
cargo run -p usbflash-cli -- --image reset test.img --quick --fs exfat --label TEST
cargo run -p usbflash-cli -- --image analyze test.img
cargo run -p usbflash-cli -- --image --sector-size 4096 wipe new4k.img
```

For tests on hardware, keep a few sticks without any data around. Unplug everything else
you care about while developing changes to `guard.rs`, `ops.rs` or `platform/`.

## Tests

```bash
cargo test --workspace
```

| Layer | Where | What it covers |
|---|---|---|
| Unit | `#[cfg(test)]` in each module | All algorithms, on fake devices with fault injection |
| End to end on images | `ops.rs` tests, `crates/usbflash-cli/tests/` | The binary, on image files |
| Independent validators | `scripts/fsck-check.sh`, in CI | `fsck.vfat` and `fsck.exfat` accept our filesystems |
| Windows itself | `scripts/windows-mount-check.ps1`, elevated, manually | Windows mounts our volumes, chkdsk accepts them, raw reading works |
| Hardware | manually, see below | Real device access, not automated |

Run the validators on Linux:

```bash
cargo build --release -p usbflash-cli
scripts/fsck-check.sh target/release/usbflash
```

### New behavior needs a test

- **Anything in `guard.rs`:** a test for allowed and refused.
- **A new filesystem signature:** a case in `fsdetect::tests::detects_simple_magics`, ideally
  verified against an image from the real tool.
- **Format changes:** must keep `scripts/fsck-check.sh` passing.

### Manual hardware checklist

Before a release, on Windows and with a stick without valuable data:

1. `usbflash list` shows the stick. `usbflash list --all` shows internal disks as `SYSTEM` or
   with their bus type.
2. `usbflash wipe <internal disk> --allow-fixed` is refused, from a non-elevated terminal and
   **without `--yes`**, so that a regression cannot harm anything.
3. `usbflash analyze`, then `usbflash health`.
4. `usbflash reset <stick> --quick`: Windows mounts the volume by itself without a format dialog.
   Write a file to it, then run `chkdsk` on the drive.
5. Repeat with `--fs exfat`, and with `--table gpt`.
6. `usbflash wipe <stick>`: while the stick was mounted with an open file before the start.
7. Pull the stick during a wipe. The result is an error or `incomplete`, never success.
8. GUI: plug and unplug while it is running; run two sticks at once; cancel a job.

## Code conventions

- `cargo fmt`, and `cargo clippy --workspace --all-targets -- -D warnings`. CI enforces both.
- `SPDX-License-Identifier: GPL-3.0-or-later` at the top of each source file.
- Every `unsafe` block has a `// SAFETY:` comment.
- Interface text is plain English that a non-expert can act on. Say what happened and what it
  means, without jargon where possible.
- Never claim more than was verified. If it was not read back, the report says so.
- New dependencies need a GPL-3.0 compatible license. `deny.toml` is checked in CI.

## Further reading

- [ARCHITECTURE.md](ARCHITECTURE.md): how it fits together
- [SAFETY.md](SAFETY.md): the safety model
- [RELEASING.md](RELEASING.md): how releases are made
- [PACKAGING.md](PACKAGING.md): notes for Linux distribution packagers
