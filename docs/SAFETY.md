# Safety model and guarantees

This document states what the toolset protects you from, and what it cannot do. If you
change anything related to it, update this file and the tests in
`crates/usbflash-core/src/guard.rs`.

## Which devices can be modified

All destructive operations go through one function, `ops::open_destructive`. Immediately
before opening a device for writing, it asks the OS for a fresh device list and checks:

| Check | Result if it fails | Can it be overridden? |
|---|---|---|
| The device still has the path, model, serial and size the user selected | Refused | No |
| It does not hold the running OS or this program | Refused | No |
| It is connected via USB, SD or MMC | Refused | No |
| It has media and a non-zero capacity | Refused | No |
| It reports removable media | Refused | Yes: `--allow-fixed` or typing `ERASE` in the GUI |

The system disk is determined by asking the OS where the system directory and the running
executable are, never by assuming `C:`. On Linux this follows partitions, LVM, LUKS and RAID
down to the physical disks, and includes `/`, `/boot`, `/usr`, `/var`, `/etc`, `/home` and
active swap. If Windows cannot determine the system disk, enumeration fails, and nothing can
be selected.

## Known limits of these checks

- **External USB disks look like big USB sticks.** The bus is USB in both cases. Most external
  disks report fixed media, which requires the typed confirmation, as does anything from
  512 GiB up. But a removable-reporting external disk smaller than that only gets the normal
  confirmation. Read the name and size in the dialog.
- **Sticks without a serial number** are told apart by path, model and size only. Swapping
  for an identical model between selection and start goes unnoticed.
- **Other programs can write to a device** after the checks. On Windows, volumes are locked
  and dismounted, and on Linux the device is opened with `O_EXCL`, which covers cooperating
  programs.

## What wiping does and does not achieve

**Achieved:** every logical sector the controller exposes is overwritten. With verification,
it was read back and matched. Ordinary recovery tools, and anyone who reads the stick through
its USB port, find nothing of the old data.

**Not achieved:** flash controllers map logical sectors to physical blocks and keep spare
blocks that software cannot address. Retired blocks and spare area can hold old data, which
an attacker with a chip reader might extract. No software can rule this out, and that
includes the commercial tools claiming otherwise. USB sticks have no standardized secure
erase command.

**So, for highly sensitive data, physically destroy the stick.** Shred or break the flash chip
itself, not only the casing.

An incomplete wipe never reports success: failed ranges are listed, the status is `incomplete`,
and the CLI exits with code 2. The GUI tells you to destroy the stick. A write protected stick
cannot be wiped at all. Flash controllers often lock themselves into this state at the end
of their life, while the data stays readable.

## Quick reset

`--quick` clears the first and last 32 MiB and writes a new filesystem. This looks like a wiped
stick, but **almost all old data remains recoverable**. It is meant for sticks without
sensitive data. Reports and the interface state this as `data_erased: false`.

## What the health verdict means

USB sticks have almost no self-diagnostics. "Good" means: the device answers, the tested areas
can be read at a plausible speed, and no available signal indicates a problem. It does not
predict how long the stick will last, and a read test cannot prove that it can still be written
to. Use the capacity test for that. Do not trust old sticks with your only copy of anything.

## Fake capacity detection

The full capacity test is authoritative within the tested device: every sector gets unique
content and is read back after all writes finished.

The quick test checks about a thousand probes plus offsets where wrapping is likely. It can
miss fakes whose real capacity lies at an unusual value, which is why a passing quick test says
so. Controllers with a large write cache could hide errors from the quick test; the full test
overwhelms any such cache.

## Reporting problems

If the tool ever modifies a device it should have refused, please treat this as a security
issue: see [SECURITY.md](../SECURITY.md).
