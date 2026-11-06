# Changelog

Notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- Device discovery on Windows and Linux with protection of system and internal disks.
- Analysis: MBR, GPT and Apple partition maps, about 30 filesystems and containers, content sampling.
- Health check: surface read test, speed profile, SMART over SAT, and a verdict.
- Capacity test that detects bad flash and fake capacity, with quick and full modes.
- Wipe with random data, zeros or both, verified by reading back.
- Factory reset with built-in FAT12/16/32 and exFAT formatters.
- Image backup tolerating read errors, and verified restore.
- JSON reports for all operations.
- `usbflash` command line tool and `usbflash-gui` graphical interface.
