# Contributing

Thanks for your interest! The [developer guide](docs/DEVELOPMENT.md) explains how to build and
test, and the [architecture document](docs/ARCHITECTURE.md) how things fit together.

## Reporting bugs

Strange sticks are the most valuable reports. Please include:

- The output of `usbflash list --all`. Remove serial numbers if you wish.
- The output of the failing command with `--json`, if possible.
- Your operating system, and what you expected to happen.

Never attach a disk image of a stick that held personal data.

If the tool modified something it should have refused, follow [SECURITY.md](SECURITY.md) instead.

## Pull requests

1. For larger changes, open an issue first to talk about the approach.
2. Add tests. Nearly everything can be tested against fake devices, without hardware.
3. Run these before pushing:

   ```bash
   cargo fmt --all
   ```

   ```bash
   cargo clippy --workspace --all-targets -- -D warnings
   ```

   ```bash
   cargo test --workspace
   ```

4. Add a line to `CHANGELOG.md` under `Unreleased`, if users would notice the change.

Changes to `guard.rs`, `ops.rs` and `platform/` get extra scrutiny, since mistakes there destroy
data. Please say how you tested them, and on which hardware.

## License of contributions

This project is licensed under GPL-3.0-or-later. By submitting a contribution, you agree to
license it under the same terms.

## Community

Be kind and assume good intent. Harassment is not tolerated.
