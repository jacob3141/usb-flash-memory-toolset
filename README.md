# USB Flash Memory Toolset

Free software to deal with that bucket of old USB sticks: find out what is on them, how
healthy they are, wipe them for privacy and return them to a clean factory state with one
click. No ads, no "unlock more features", no nag screens.

Windows and Linux. GPL-3.0-or-later.

> [!WARNING]
> This tool deliberately destroys data. It refuses to touch system disks and internal
> drives, but it cannot know whether the USB stick in your hand holds the only copy of
> something. Read [what it can and cannot guarantee](docs/SAFETY.md).

> [!NOTE]
> Early stage. Formatting is validated with `fsck` and by Windows on disk images, and all algorithms
> are tested against simulated faulty devices. Reports from real sticks are very welcome.

## Features

| | |
|---|---|
| **Discover** | Lists USB and SD devices and updates when you plug one in. Vendor, model, serial, capacity, mount points, write protection. |
| **Analyze** | Reads partition tables (MBR, GPT, Apple) and recognizes filesystems Windows cannot read, without any drivers. Samples the device to tell blank, random or encrypted, and regular data apart. |
| **Health check** | Surface read test with a speed profile and a list of bad areas, SMART where the stick supports it, and a verdict in plain words. |
| **Capacity test** | Writes and verifies a pattern to find bad flash and counterfeit sticks that lie about their capacity, including an estimate of the real size. |
| **Wipe** | Overwrites every sector with random data, zeros or both, verifies by reading back, and reports honestly when areas could not be overwritten. |
| **Factory reset** | One click: wipe, one aligned partition, fresh FAT32 or exFAT. No 32 GB limit for FAT32. |
| **Image** | Saves a whole device to a file before you wipe it, tolerating read errors, and restores images with verification. |
| **Reports** | Every operation produces a JSON report, for example as a record of which stick was wiped. |
| **Batch friendly** | Works on several sticks at the same time. |

**Recognized filesystems and containers:** FAT12/16/32, exFAT, NTFS, ReFS, ext2/3/4, XFS,
Btrfs, F2FS, bcachefs, NILFS2, JFS, ReiserFS, Minix, HFS, HFS+, HFSX, APFS, UFS1/2, ZFS,
SquashFS, CramFS, ISO 9660, UDF, LUKS1/2, BitLocker, LVM2, Linux RAID, Linux swap.

## Installation

**Windows:** download the ZIP from the [releases page][releases], unpack it, and start
`usbflash-gui.exe`. It asks for administrator rights, because raw disk access requires them.
The builds are not code signed yet, so SmartScreen may warn you the first time.

**Linux:** use your distribution's package if there is one, or
[build from source](docs/DEVELOPMENT.md). Run it as root or as a member of the `disk` group.
Packagers find notes in [docs/PACKAGING.md](docs/PACKAGING.md).

macOS is not supported at the moment. The core is portable, and a backend for it is welcome;
see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

[releases]: https://github.com/jacob3141/usb-flash-memory-toolset/releases

## Graphical interface

Plug in a stick and select it. The analysis runs automatically. **Factory reset** is one
button away, and the confirmation shows exactly which device will be erased.

A stick that turns out to be interesting can be saved under **Image** first.

## Command line

```text
usbflash list                      List USB and SD devices (--all for all disks)
usbflash analyze  <device>         Partitions, filesystems, type of content
usbflash health   <device>         Read test, SMART, verdict (--full reads every sector)
usbflash test     <device>         Find bad flash and fake capacity (--full) [destroys data]
usbflash wipe     <device>         Overwrite everything (--mode random|zero|random-zero) [destroys data]
usbflash reset    <device>         Factory state (--quick, --fs, --table, --label) [destroys data]
usbflash backup   <device> <file>  Save to an image, tolerating read errors
usbflash restore  <file> <device>  Write an image and verify it [destroys data]
```

`<device>` is the path from `usbflash list` (`\\.\PhysicalDrive3`, `PhysicalDrive3`,
`/dev/sdb`) or the serial number.

`--json` prints machine readable reports. The exit code is `0` on success, `2` if the
operation ran but the result is negative (bad blocks, incomplete wipe), and `1` on errors.

Everything works on image files too, which is a safe way to try it out:

```bash
truncate -s 1G test.img
usbflash --image reset test.img --quick --label DEMO
usbflash --image analyze test.img
```

## Why are there no "35 pass" wipes?

Multi-pass patterns were designed for magnetic hard disks from the 1990s. On flash, one pass
overwrites what can be overwritten, and additional passes only wear out the stick.

What no software can reach are the spare blocks the controller has retired or set aside. A
stick that held highly sensitive data should be physically destroyed.
See [docs/SAFETY.md](docs/SAFETY.md).

## Contributing

Bug reports, especially about odd sticks, are very welcome. See
[CONTRIBUTING.md](CONTRIBUTING.md) and the [developer documentation](docs/DEVELOPMENT.md).

## License

Copyright (C) Jacob Dawid and contributors.

This program is free software: you can redistribute it and/or modify it under the terms of
the GNU General Public License as published by the Free Software Foundation, either version
3 of the License, or (at your option) any later version. See [LICENSE](LICENSE).

It is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY.
