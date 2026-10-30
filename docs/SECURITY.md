# Security and data-loss prevention

Ferrufus writes directly to storage devices. A wrong choice of device, a
misread image or a race with a hot-plugged disk can destroy data that cannot be
recovered. Memory safety (safe Rust only) removes one class of bugs; this
document describes the rest of the defenses.

## Reporting a vulnerability

Report security problems in Ferrufus privately to the repository owner: on
GitHub use "Report a vulnerability" on the Security tab when the repository is
public, otherwise contact the owner directly. Please do not open a public issue
for an unfixed vulnerability. Do not report Ferrufus problems to the Rufus
project: Ferrufus is an unofficial port and Rufus' maintainers are not
responsible for it.

## Assets

1. Data on every storage device attached to the machine, above all the
   system disk.
2. The integrity of the media Ferrufus creates (users boot and install
   operating systems from it).
3. The integrity of the user's system (Ferrufus must not become a privilege
   escalation path).
4. Files Ferrufus downloads (bootloaders, Windows and UEFI Shell images).

## Trust boundaries

- **Images are untrusted.** ISO, disk, VHD/VHDX and compressed images may be
  malformed or hostile. Every parser is bounded and must fail cleanly.
- **Device metadata is partly untrusted.** USB vendor, model and serial strings
  and partition labels are chosen by whoever made or formatted the device.
- **Downloads are untrusted until verified** (signature or pinned hash).
- **Configuration and command-line input** are trusted to express the user's
  intent, but can never disable a safety check.
- **The privileged side** (M3 and later) trusts nothing from the unprivileged
  side: it re-reads and revalidates everything it acts on.

## Threats and mitigations

| ID | Threat | Mitigations | Status |
|---|---|---|---|
| T1 | The user selects the wrong device (for example the system disk or an external backup disk) | Listing policy translated from `src/dev.c:GetDevices()` (non-USB fixed disks never listed, USB HDDs hidden unless requested); the system disk is excluded through partitions, dm/md holders, swap and the running executable; **no automatic selection**; the plan shows the exact device, its identity and every partition, label and mount that will be lost; typed confirmation (M3) | Listing, exclusion and plan: M1. Confirmation: M3 |
| T2 | The device changes between planning and writing (unplug and replug, `/dev/sdX` reuse, a race by another process) | `DeviceIdentity` (`diskseq`, `dev_t`, size, block size, serial, sysfs path) compared immediately before every destructive stage; open first, then verify that the descriptor still refers to the same `diskseq`; `O_EXCL` open (fails if the kernel uses the device: mounted, dm, md, swap); BSD `flock` so udev does not interfere | Revalidation: M1. Exclusive open and descriptor checks: M3 |
| T3 | A malformed or hostile image (integer overflow, huge allocations, path traversal, loops) | Own parsers with explicit bounds; checked arithmetic; no allocation sized from untrusted fields without a cap; no recursion without a depth limit; property tests feeding arbitrary bytes to every parser; fuzzing (M2); extracted paths sanitized (M4, from `src/iso.c` `sanitize_filename()`) | Bounds and property tests: M1. Fuzzing: M2 |
| T4 | An image stored on the target device | Refuse, as `src/drive.c:IsSourceImageLocatedOnTargetDrive()` does, by resolving the image's file system device through partitions and holders | M1 |
| T5 | An image larger than the device, or a partial sector at the end | Raw payloads must fit; writes are never truncated; padding is zero-filled | M1 (planner), M3 (writer) |
| T6 | Tampered downloads, or a downloaded file replaced after verification | HTTPS plus signature or pinned SHA-256; verify after opening and use the same descriptor (upstream fixed such a TOCTOU in Fido script execution, CVE-2026-2398, and in reused downloads after v4.15); cache in a private directory | M4/M8 |
| T7 | Privilege escalation through the privileged interface | Unprivileged GUI; UDisks2 with polkit per operation; any helper accepts only typed requests and revalidates everything; no shell; no user-controlled paths passed through unchecked | M3 |
| T8 | Command injection into external tools (`mkntfs` and others) | `std::process::Command` with an argument vector, never a shell; arguments built from typed values; the device path is the revalidated one; tool output parsed defensively; tool paths resolved from a fixed list | M4 |
| T9 | Dishonest results: success reported after a failed write, flush or verification; interrupted writes presented as rolled back | Every I/O result checked; short reads and writes handled; `fsync` and read-back verification before success; cancellation reported as "device contents are now undefined" | M3 |
| T10 | Supply-chain compromise (dependencies, CI, packaging tools) | `Cargo.lock` with `--locked` builds; `cargo deny` (advisories, licenses, sources, banned crates); `cargo xtask policy` (no `unsafe`, no C sources, no native-linking dependencies); GitHub Actions pinned to commit SHAs with read-only default permissions; AppImage tools pinned by version and SHA-256; Dependabot updates; `SHA256SUMS` with every release; build-provenance attestations when the repository is public | M1 |

## Deliberate deviations from upstream

Rufus is the behavioral reference, but these upstream behaviors are unsafe for
a Linux port and are intentionally not reproduced:

1. **Automatic device selection.** Rufus selects the first device when the
   previous one is gone (`src/dev.c:1080-1081` at v4.15). Ferrufus never
   selects a device for the user, in the GUI or the CLI.
2. **Silent truncation.** `src/format.c:WriteDrive()` writes
   `MIN(DiskSize, image_size)` bytes (`src/format.c:1157`). Ferrufus refuses a
   payload larger than the device instead.
3. **Stale padding.** When the last read is not a multiple of the sector size,
   Rufus rounds the write up using whatever the buffer held
   (`src/format.c:1372-1375`). Ferrufus pads with zeros.
4. **Size check slack and override.** Rufus accepts images up to 4 KB larger
   than the drive and lets Alt-S disable the check (`src/rufus.c:1531`).
   Ferrufus requires the raw payload to fit and has no override.
5. **No verification after a DD write.** Ferrufus verifies by default.
6. **Disabling drive locking (Alt-,)** is not ported; exclusive access is always
   required.
7. **Virtual disks listed by default.** Loop devices on Linux are mostly
   system-managed (snaps, containers), so they are only listed on request.

## Rules for contributors

- No `unsafe`, no `extern "C"`, no C or C++ sources, no build scripts that
  compile native code. `cargo xtask policy` enforces this in CI.
- No shell commands built from strings. External programs, when they arrive,
  are explicit, validated backends.
- Parsers of untrusted data: bounds-check everything, use checked arithmetic,
  cap allocations, and add a property test with arbitrary input.
- Never suppress an I/O error, and never report success before flushing and
  verifying.
- Tests must never write to physical devices (see [TESTING.md](TESTING.md)).
- Any intended difference from upstream behavior is recorded in
  [FEATURE_MATRIX.md](FEATURE_MATRIX.md) and, if it affects safety, here.

## Milestone 1 guarantees

The read-only milestone contains no code that writes to devices or files: no
writer module exists, and `cargo xtask policy` rejects `File::create`,
`fs::write`, `OpenOptions::write(true)` and `append(true)` in non-test sources.
The CLI and GUI only read sysfs, procfs, the udev database and the image files
the user selects. The GUI refuses to run as root.
