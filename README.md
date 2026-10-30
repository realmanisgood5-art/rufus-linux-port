# Ferrufus

**An unofficial Linux port of [Rufus](https://rufus.ie), written in Rust.**

Ferrufus aims to reproduce the features and behavior of Rufus, the Reliable
USB Formatting Utility, as a Linux-native application: formatting USB drives
and cards, creating bootable Windows, Linux, BSD and DOS media from ISO and
disk images, persistent Linux live drives, checksums, bad-block checks, and
more. It is a source-guided port: the original Rufus C code is the reference
for what to do, translated into safe Rust.

> Ferrufus is **not** affiliated with, endorsed by, or supported by Pete Batard
> or the Rufus project. Please do not report Ferrufus problems to the Rufus
> issue tracker.

## Status

**Milestone 2 (read-only preview).** This build cannot write to devices: it
has no code that opens a device or file for writing, and a source policy
check enforces that.

Milestones 1 and 2 provide:

- Device discovery that applies Rufus' rules for which drives to list (USB
  drives and cards; USB hard drives only on request; internal disks never; the
  system disk always excluded), without root privileges.
- Image identification: ISO 9660/UDF labels, El Torito, ISOHybrid, MBR boot
  code (ms-sys signatures), GPT, VHD, VHDX, WIM, FFU and compressed images.
- MD5, SHA-1, SHA-256 and SHA-512 checksums.
- An ISO file scan as Rufus does it: boot loaders (Syslinux, GRUB, Windows,
  ReactOS, KolibriOS, UEFI), their versions, files over 4 GB, and the images
  that only work in DD mode.
- The format options Rufus would offer and select: partition scheme, target
  system, file system, cluster size and label.
- Dry-run plans for DD mode, ISO mode and plain formats, with every safety
  check, the partitions that would be created and the exact data that would be
  lost.
- `ferrufus-cli` (with JSON output) and the `ferrufus` desktop application.

See [docs/FEATURE_MATRIX.md](docs/FEATURE_MATRIX.md) for the status of every
Rufus feature.

## Downloads

Every merge to `main` publishes `Ferrufus-x86_64.AppImage` in the rolling
[`continuous`](../../releases/tag/continuous) pre-release; each new version
also gets a permanent `v<version>` release. Each release has a `SHA256SUMS`
file.

```bash
chmod +x Ferrufus-x86_64.AppImage
```

```bash
./Ferrufus-x86_64.AppImage
```

```bash
./Ferrufus-x86_64.AppImage cli devices --all
```

```bash
./Ferrufus-x86_64.AppImage cli plan --device /dev/sdX --image image.iso
```

`cli` runs `ferrufus-cli`; anything else starts the desktop application,
which refuses to run as root. Without FUSE, add `--appimage-extract-and-run`
before the arguments.

### Command line

| Command | Does |
|---|---|
| `ferrufus-cli devices [--all] [-x] [--list-non-usb-removable] [--list-virtual]` | Lists the drives that can be written to (none is ever selected for you); `--all` shows excluded drives and why |
| `ferrufus-cli inspect IMAGE` | Analyzes an image (and the files of an ISO) the way Rufus does |
| `ferrufus-cli options --device DEV [--image IMAGE]` | Shows the format options Rufus would offer and select |
| `ferrufus-cli hash FILE [--sha512] [--algo LIST] [--expect ALGO:HEX]` | Computes checksums |
| `ferrufus-cli plan --device DEV [--image IMAGE] [--mode dd\|iso\|format] [--fs FS] [--scheme mbr\|gpt\|sfd] [--target bios\|uefi\|bios-or-uefi] [--label L] ...` | Shows what writing or formatting would do (dry run) |

Every command accepts `--json` (schemas `ferrufus.devices/v1`,
`ferrufus.image-report/v2`, `ferrufus.hash/v1`, `ferrufus.options/v1`,
`ferrufus.plan/v2`). Exit
codes: 0 success, 1 error, 2 usage, 3 plan blocked, 4 device not found or
ambiguous, 5 image unreadable, 6 checksum mismatch, 130 interrupted.

## Safety

Ferrufus writes to raw storage devices, so its design starts with not
destroying the wrong data: it never selects a device for you, never runs its
GUI as root, re-checks the device's identity before each destructive step, and
never reports success before data has been flushed and verified. Some Rufus
behaviors are intentionally not reproduced for this reason; see
[docs/SECURITY.md](docs/SECURITY.md).

## Building

Requires the Rust toolchain pinned in `rust-toolchain.toml` (installed
automatically by `rustup`).

```bash
cargo build --release --locked
```

```bash
cargo test --workspace --locked
```

## Documentation

| Document | Contents |
|---|---|
| [docs/FEATURE_MATRIX.md](docs/FEATURE_MATRIX.md) | Every upstream feature, where it lives in Rufus, and its status here |
| [docs/RUST_PORTING_PLAN.md](docs/RUST_PORTING_PLAN.md) | Upstream baseline, source map, Windows-to-Linux mapping, milestones, blockers |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crates, the operation pipeline, data model, privilege model, CLI and GUI design |
| [docs/SECURITY.md](docs/SECURITY.md) | Threat model and data-loss prevention |
| [docs/TESTING.md](docs/TESTING.md) | Test strategy and acceptance criteria |
| [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) | Dependencies, tools, platform requirements and licensing |

## License

Ferrufus is free software, licensed under the GNU General Public License,
version 3 or (at your option) any later version. See [LICENSE](LICENSE) and
[NOTICE.md](NOTICE.md).

Rufus is Copyright © Pete Batard and contributors, and is also distributed under
the GNU General Public License, version 3 or later.
