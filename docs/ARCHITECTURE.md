# Architecture

## Crates

```text
usbflash-gui   egui front end        thin: presentation, job threads
usbflash-cli   `usbflash` binary     thin: argument parsing, output
      \            /
     usbflash-core                   all logic, no UI dependencies
```

Rule: anything that could be useful to both front ends goes into the core. Front ends never
talk to the OS about disks.

## Core modules

| Module | Responsibility |
|---|---|
| `device` | `DeviceInfo`, the `BlockDevice` trait, `FileDevice`, aligned buffers |
| `guard` | The safety policy, as pure functions. See [SAFETY.md](SAFETY.md) |
| `ops` | Entry points for front ends. **The choke point that applies the guards** |
| `platform/` | OS backends: enumeration, raw access, locking, SMART transport |
| `partition/` | MBR, GPT (read and write), Apple Partition Map (read) |
| `fsdetect` | Filesystem identification by signature |
| `analyze` | Layout plus content sampling, and findings in plain words |
| `scan` | Non-destructive surface read test |
| `capacity` | Destructive write and verify test, fake capacity detection |
| `smart` | SMART attribute parser |
| `health` | Combines all signals into a verdict, as a pure function |
| `wipe` | Overwriting with verification |
| `format/` | Built-in FAT12/16/32 and exFAT formatters |
| `reset` | Factory reset: orchestrates wipe, format and partition table |
| `image` | Raw backup and restore |
| `sweep` | Error tolerant I/O shared by the above |
| `pattern` | Random-access ChaCha20 stream |
| `progress` | Progress reporting and cancellation (`Ctx`) |
| `report` | JSON report envelope |

## Key design decisions

**Everything works on `BlockDevice`.** Algorithms never see a real disk. Tests run them against
`testutil::MemDevice`, which can simulate bad blocks, dropped writes and fake capacity, and
against `FileDevice`. This is why almost everything can be tested without hardware, and why it
is safe to work on this project without risking your disks.

**Alignment is part of the contract.** Raw devices opened with `FILE_FLAG_NO_BUFFERING` or
`O_DIRECT` reject unaligned access. Both fake devices assert the contract, so a mistake fails
a test instead of only failing on real hardware.

**One choke point for writes.** `platform::open_exclusive` is only called from
`ops::open_destructive`, right after a fresh enumeration and the policy check. Keep it that
way. The handle from `open_read` has no write access at the OS level.

**Own formatters instead of `format.com` and `mkfs`.** That gives the same behavior on all
platforms, testability against images, no 32 GB limit for FAT32, and no dependency on the
system's tools. The price is that the correctness burden is ours, which CI addresses by checking
the results with the independent `fsck.vfat` and `fsck.exfat` (`scripts/fsck-check.sh`).

**The partition table is written last** during a factory reset. Operating systems mount new
partitions the moment they appear. Announcing a partition before its filesystem exists would
trigger "you need to format this disk" dialogs and races with the automounter.

**Wiping tolerates errors, reporting does not.** A bad block does not stop a wipe, since the
rest should still get overwritten. But the result is `Incomplete`, with the affected ranges
listed. After 32 consecutive failing chunks, the device is considered gone.

**Random-access pattern.** The ChaCha20 stream is addressed by byte offset. Verification
regenerates the data rather than storing it, and every offset has unique content, so a fake
stick mapping two addresses to the same flash is caught.

## Windows backend

- Disks are `\\.\PhysicalDrive0..63`. Discovery uses handles without data access, so listing
  works without elevation; reading and writing do not.
- Before writing: clear the read-only and offline disk attributes, then lock and dismount each
  volume, keeping the handles. Then delete the drive layout, because Windows blocks raw writes to
  sectors inside a known volume.
- After writing: unlock and call `IOCTL_DISK_UPDATE_PROPERTIES`, so the new volume mounts
  without replugging.
- The system disk is resolved via the volume of `GetSystemDirectoryW`. Remember that it is not
  always `C:`.
- The GUI's manifest requests elevation (`crates/usbflash-gui/windows.manifest`). The CLI does
  not, so that `usbflash list` works in a normal terminal.

## Linux backend

- Discovery reads `/sys/block`. The bus comes from the device path in sysfs.
- `/proc/self/mountinfo` and `/proc/swaps` give mounts; stacked devices are followed via
  `/sys/class/block/*/slaves`.
- Writing: unmount (lazily if busy), then open with `O_DIRECT | O_SYNC | O_EXCL`. After writing,
  `BLKRRPART` re-reads the table.

## Adding a platform

Create `platform/<os>.rs` with these four functions and add it to `platform/mod.rs`:

```rust
pub fn enumerate() -> Result<Vec<DeviceInfo>>;
pub fn open_read(info: &DeviceInfo) -> Result<Box<dyn BlockDevice>>;
pub fn open_exclusive(info: &DeviceInfo) -> Result<Box<dyn BlockDevice>>;
pub fn read_smart(info: &DeviceInfo) -> Result<Vec<u8>>;
```

The critical part is `enumerate`: `is_system` and `bus` must be correct, since the policy relies on
them. When in doubt about the system disk, return an error instead of a guess.

## GUI

`app.rs` is immediate mode: the whole interface is redrawn from state. `jobs.rs` holds the device
monitor thread (polls every 2 seconds and reports changes) and one thread per running operation,
keyed by device, so several sticks can be processed at once. Results come back over channels;
`summary.rs` turns reports into short texts.
