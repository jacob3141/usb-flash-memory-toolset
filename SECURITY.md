# Security policy

Please report these privately, through
[GitHub's private vulnerability reporting](https://github.com/jacob3141/usb-flash-memory-toolset/security/advisories/new):

- The tool modifies a device it should have refused: system disks, internal drives.
- A wipe reports success, although data remained on addressable sectors.
- Random data is predictable.
- Memory safety problems from parsing malicious partition tables or filesystems. The tool reads
  untrusted sticks with administrator rights, so this matters.

Please include your OS, the output of `usbflash list --all`, and the steps to reproduce.

That data can survive in the controller's spare blocks is a limitation of the hardware, and not a
vulnerability. See [docs/SAFETY.md](docs/SAFETY.md).

Only the latest release gets security fixes.
