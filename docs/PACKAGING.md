# Notes for packagers

Thanks for packaging this. The project does not ship Linux binaries itself and relies on you.

## Building

```bash
cargo build --release --locked --workspace
```

Produces `target/release/usbflash` and `target/release/usbflash-gui`. Minimum Rust version: see
`rust-version` in `Cargo.toml`. A split into a CLI package and a GUI package works well, as only the
GUI needs graphics libraries:

```bash
cargo build --release --locked -p usbflash-cli
```

Build dependencies of the GUI: X11 and/or Wayland, xkbcommon, and OpenGL headers. File dialogs use
the XDG desktop portal at runtime (`xdg-desktop-portal` with a backend), so GTK is not needed.

The GUI uses Noto Sans if it is installed, and otherwise an embedded font. A dependency on your Noto
Sans package gives a better result, but is optional.

## Files to install

| File | Destination |
|---|---|
| `usbflash`, `usbflash-gui` | `/usr/bin/` |
| `packaging/linux/usb-flash-memory-toolset.desktop` | `/usr/share/applications/` |
| `packaging/linux/io.github.jacob3141.usb_flash_memory_toolset.metainfo.xml` | `/usr/share/metainfo/` |
| `assets/icon-256.png` as `usb-flash-memory-toolset.png` | `/usr/share/icons/hicolor/256x256/apps/` |
| `docs/SAFETY.md` | `/usr/share/doc/usb-flash-memory-toolset/` |

## Privileges

Raw disk access needs root or the `disk` group. The desktop file starts the GUI through `pkexec`.
Running under `pkexec` needs a polkit rule allowing GUI programs, namely
`org.freedesktop.policykit.exec.allow_gui`. Decide what fits your distribution. Please do not
install the binaries setuid.

The `disk` group alone does not allow unmounting. Destructive operations on a mounted stick then fail
with a clear error, and work after unmounting it by hand.

## Tests

`cargo test --workspace` needs no hardware or privileges, and writes only temporary files.
`scripts/fsck-check.sh` needs `dosfstools`, `exfatprogs` and `fdisk`. It is a good choice for a
package check.

## Testing status on Linux

The Linux backend (`crates/usbflash-core/src/platform/linux.rs`) compiles and its parsing is unit
tested, but it gets less real-world use than the Windows backend. Reports of how it behaves on your
distribution are very welcome, especially with LVM or LUKS on the system disk.
