# Testing

## Principles

1. **Automated tests never write to physical devices.** Not in CI, not on a
   developer machine.
2. **A successful build does not establish correctness.** Features are marked
   tested in [FEATURE_MATRIX.md](FEATURE_MATRIX.md) only when automated tests
   cover them.
3. **A successful write does not establish bootability.** Boot behavior is
   validated separately, in virtual machines and on hardware, and tracked
   separately.
4. **Upstream is the oracle.** Where Rufus behavior can be observed (logs,
   produced media), test vectors come from running Rufus v4.15, not from what
   the Ferrufus code happens to do.
5. **Every fixed bug gets a regression test.**

## Running the tests

```bash
cargo test --workspace --locked
```

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
```

```bash
cargo run -p xtask --locked -- policy
```

```bash
cargo deny check
```

CI runs the same commands (see `.github/workflows/ci.yml`), plus an AppImage
build and smoke test.

## Test layers

| Layer | What it covers | Where |
|---|---|---|
| Unit tests | Parsers, size formatting, heuristics, policy rules, plan arithmetic | `#[cfg(test)]` modules in `ferrufus-core` |
| Property tests (`proptest`) | Checked arithmetic for sizes, offsets and alignment never overflows; "arbitrary bytes never panic" for every parser of untrusted data | `ferrufus-core` |
| Fake-system integration tests | Device discovery and listing policy against synthetic `/sys`, `/proc` and udev trees | `crates/ferrufus-core/tests/` |
| Synthetic image tests | Image identification against images built in memory (MBR, GPT, ISO 9660 with Joliet and El Torito, UDF, VHD, VHDX, WIM, compressed signatures) | `crates/ferrufus-core/tests/` |
| CLI tests | The real `ferrufus-cli` binary: JSON output, schemas, exit codes | `crates/ferrufus-cli/tests/` |
| Headless GUI tests (`egui_kittest`) | No device preselected, no write controls, read-only banner present | `crates/ferrufus-gui/tests/` |
| Real-host smoke test | Discovery on the machine running the tests, read-only; the disk backing `/` must never be listed as eligible | `crates/ferrufus-core/tests/` |
| Packaging smoke test | The built AppImage runs the CLI | CI `appimage` job |

### Fixtures

No binary fixtures are committed. `ferrufus-testkit` builds what tests need:

- `FakeSystem`: a temporary directory tree with `sys/class/block`,
  `sys/devices/...` (with USB ancestors, SCSI attributes, partitions and
  holders), `proc/self/mountinfo`, `proc/swaps` and `run/udev/data`.
- Image builders: MBR with or without boot marker and with any ms-sys boot code
  table, GPT (512- and 4096-byte sectors, valid or corrupted CRCs), ISO 9660
  primary and Joliet volume descriptors, El Torito boot catalogs, UDF anchor and
  logical volume descriptors, VHD footers, VHDX headers, WIM headers.

Generated tables (ms-sys MBR boot code, HDD heuristic tables) are checked
against the upstream sources they came from by regenerating them
(`cargo xtask gen-tables --upstream <rufus checkout> --check`).

## How "never write to physical devices" is enforced

- **M1:** there is no writer. `cargo xtask policy` fails if non-test sources
  contain `File::create`, `fs::write`, `OpenOptions::write(true)` or
  `append(true)`.
- **M3 onward:** the writer only accepts a target produced by a validated
  constructor. In tests, that constructor accepts only regular files and loop
  devices whose backing file lives in the test's temporary directory. The
  physical-device path is behind a Cargo feature that test and CI builds do not
  enable.
- CI runs on disposable virtual machines that have no removable media.

## Planned test infrastructure

| Item | Milestone | Plan |
|---|---|---|
| Fuzzing | M2 | `cargo-fuzz` targets for every parser of untrusted data in a separate `fuzz/` workspace (nightly toolchain and libFuzzer are test-only tools and are never shipped); corpus seeded from the synthetic builders; scheduled CI job |
| Virtual devices | M3 | Loop devices attached to temporary files (`losetup` with `sudo` on the CI runner) for real block-device writes, exclusive-open behavior and partition rescans |
| Failure injection | M3 | Reader and writer wrappers that return short reads, short writes, `EIO` at chosen offsets, and disconnects; tests assert that no success is reported and that the report is honest |
| Boot tests | M4+ | QEMU with SeaBIOS (BIOS) and OVMF (UEFI, with and without Secure Boot) booting the produced media; KVM when the runner provides it |
| Upstream comparison vectors | M2+ | Inputs and Rufus v4.15 results (cluster sizes, partition layouts, labels, scan reports, `unattend.xml`) recorded once on Windows and replayed in CI |
| Hardware validation | Release readiness | A documented matrix of real USB drives, card readers and target PCs, run by hand |

## Milestone 1 acceptance criteria

| Area | Criteria |
|---|---|
| Gates | `fmt`, `clippy -D warnings`, tests, `xtask policy` and `cargo deny` pass locally and in CI |
| Device discovery | Fake-system cases: USB flash drive listed; USB HDD hidden unless `-x`; UAS drive; SD card; card reader without media hidden; NVMe and SATA disks never listed; non-USB removable only on request; loop devices only on request; system disk excluded when `/` is on a partition, on LVM on LUKS (holders), or on btrfs (anonymous `0:N` device); swap disk excluded; disk holding the executable or AppImage excluded; devices under 8 MB excluded; read-only devices flagged; size ordering; no automatic selection; `diskseq` in the identity |
| Heuristic | `IsHDD()` scoring cases taken from the upstream tables, including the `#` digit wildcard and VID:PID entries |
| Image inspection | Every ms-sys MBR type identified in upstream order; GPT CRC validation at both sector sizes; ISO label rule including Joliet edge cases; El Torito BIOS and EFI entries; UDF label preferred over ISO 9660; ISOHybrid detection; VHD footer checksum; extension and magic mismatches reported; arbitrary input never panics |
| Checksums | Known-answer vectors for MD5, SHA-1, SHA-256 and SHA-512; one-pass results equal separate computations; cancellation and injected I/O errors are reported as errors |
| Planner | Blocks: system disk, payload larger than the device (and accepts exactly equal), image on the target, read-only device, unsupported images, non-hybrid ISOs, compressed/VHDX/FFU images, changed identity, active holders or swap. Plans are never executable in M1 |
| Front ends | CLI JSON schemas and exit codes; GUI headless checks |
| Packaging | AppImage builds, its CLI runs, the binaries link only to the C runtime libraries, and the required glibc version stays within the documented baseline |
