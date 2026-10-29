# Dependencies and licensing

## Policy

- Application code is stable Rust only. No project-owned C or C++, no
  project-owned `unsafe`, no FFI written by this project.
- Dependencies are chosen for maintenance, safety and a pure-Rust build.
  Crates that compile or link native C/C++ libraries are avoided; any exception
  must be listed here with a justification and allowlisted in `xtask`.
- `cargo xtask policy` fails the build if any resolved dependency declares a
  native `links` key or depends on `cc`, `cmake`, `bindgen` or `pkg-config` at
  build time without an allowlist entry.
- The packaging check (`packaging/appimage/build-appimage.sh`) fails if a
  release binary links shared libraries other than the C runtime (`libc`,
  `libm`, `libgcc_s`, the dynamic loader) or needs a newer glibc than the
  documented baseline.
- `cargo deny check` enforces licenses compatible with GPL-3.0-or-later,
  RustSec advisories, crates.io as the only source, and bans known C-wrapping
  crates (`openssl-sys`, `libz-sys`, `zstd-sys`, `lzma-sys`, `bzip2-sys`).
- `unsafe` inside dependencies is accepted only in widely used, maintained
  crates; notable uses are listed below.

## Rust crates (milestone 1)

| Crate | Used by | Purpose | License | Native code / `unsafe` notes |
|---|---|---|---|---|
| `thiserror` | core | Error types | MIT OR Apache-2.0 | Proc macro, no `unsafe` |
| `serde`, `serde_json` | core, CLI | Report and plan serialization, JSON output | MIT OR Apache-2.0 | Some `unsafe` for performance in `serde_json` |
| `md-5`, `sha1`, `sha2` (RustCrypto) | core | Checksums | MIT OR Apache-2.0 | Runtime CPU feature detection (`cpufeatures`) and SIMD intrinsics inside the crates |
| `crc32fast` | core | GPT header and entry CRC32 | MIT OR Apache-2.0 | SIMD intrinsics inside the crate |
| `tracing`, `tracing-subscriber` | core, CLI, GUI | Structured logging | MIT | — |
| `clap` | CLI, GUI | Argument parsing | MIT OR Apache-2.0 | — |
| `eframe`, `egui` (glow renderer) | GUI | Desktop UI | MIT OR Apache-2.0 | OpenGL through `glow`/`glutin`; X11, Wayland and EGL libraries are **loaded at run time** (`dlopen`), not linked |
| `winit` | GUI | Windowing (listed directly to enable run-time loading of X11/Wayland) | Apache-2.0 | As above |
| `rfd` | GUI | File chooser through the XDG desktop portal (D-Bus, pure Rust) | MIT | No GTK |
| `proptest` (dev) | tests | Property-based tests | MIT OR Apache-2.0 | — |
| `tempfile` (dev) | tests | Temporary fake systems and images | MIT OR Apache-2.0 | — |
| `egui_kittest` (dev) | GUI tests | Headless UI tests through AccessKit | MIT OR Apache-2.0 | No GPU needed without the snapshot feature |

The exact resolved versions are pinned in `Cargo.lock`.

### Crates considered and not used in M1

| Crate | Reason |
|---|---|
| `gpt`, `mbrman` | Designed for editing devices; Ferrufus needs small, strictly bounded, fuzzable read-only parsers for untrusted images |
| `hadris-iso`, `cdfs`, `iso9660` | Same reason; revisit for the M2 directory reader |
| `udev`, `libudev` | Bind the C libudev; Ferrufus reads sysfs and the udev database files directly |
| `nix` ioctl macros | Expand to `unsafe` code in the calling crate |
| `xz2`, `zstd`, `liblzma` | Bind C libraries; pure-Rust decoders will be used in M3 |

## Build, CI and packaging tools

| Tool | Version | Used for | Notes |
|---|---|---|---|
| Rust toolchain | pinned in `rust-toolchain.toml` | Building | Stable channel |
| `cargo-deny` | installed by `taiki-e/install-action` (pinned SHA) | License, advisory and source checks | CI only |
| `appimagetool` | 1.9.1 (pinned SHA-256) | Building the AppImage | Third-party C tool, packaging only, never shipped inside the application |
| AppImage `type2-runtime` | 20251108 (pinned SHA-256) | The AppImage's startup stub | Statically linked C program embedded in the AppImage file by the AppImage format itself; it mounts the image and starts `AppRun`. It is not project-owned code |
| GitHub Actions | pinned by commit SHA | CI | `actions/checkout`, `Swatinem/rust-cache`, `taiki-e/install-action`, `actions/upload-artifact`, `actions/download-artifact`, `actions/attest-build-provenance` |
| `gh` | runner image | Creating releases | Uses the workflow's `GITHUB_TOKEN` |
| `actionlint`, `zizmor` | local | Workflow linting and security review | Development only |

## Platform requirements at run time

| Requirement | Needed by | Notes |
|---|---|---|
| Linux with sysfs and procfs | everything | `diskseq` (Linux 5.15+) strengthens identity checks |
| glibc | everything | The AppImage is built against an older glibc baseline documented in the CI check (≤ 2.35) |
| udev database (`/run/udev/data`) | device details | Optional; less detail without it |
| OpenGL or GLES, X11 or Wayland, xkbcommon | GUI | Provided by the desktop; loaded at run time |
| `xdg-desktop-portal` | GUI file chooser | Optional; path entry and drag and drop work without it |
| UDisks2 and polkit | writing (M3 and later) | Preferred privilege mechanism |

## Planned external backends (not used in M1)

External programs are allowed only where reimplementing a format would be
unreasonable, and only as explicit backends: located from a fixed list of
paths, run without a shell with typed arguments, given only a revalidated
device path, version-checked, and with their output parsed defensively.

| Program | Package | Purpose | Milestone |
|---|---|---|---|
| `mkntfs` | ntfs-3g | NTFS formatting | M4 |
| `mkfs.exfat` | exfatprogs | exFAT formatting (unless a Rust formatter is adopted) | M4 |
| `mkudffs` | udftools | UDF formatting | M4 |
| `mke2fs` | e2fsprogs | ext2/3/4 formatting, persistence partitions | M4/M5 |
| `wimlib-imagex` | wimlib | WIM splitting and applying until a Rust implementation exists | M6 |

## Upstream assets (for later milestones)

Rufus embeds or downloads boot components. Before Ferrufus bundles any of them,
its license and source-availability obligations must be recorded here. Licenses
below are as stated in Rufus' About box (`src/license.h`); entries marked
"verify" are not stated there and must be checked against the component's own
project.

| Asset | Upstream location | License | Notes |
|---|---|---|---|
| GRUB 2.14 `boot.img`/`core.img` | `res/grub2/` | GPL-3.0-or-later | Built from GRUB sources (see `res/grub2/readme.txt`); source must be offered |
| Syslinux 4.07 and 6.04 `ldlinux` files | `res/syslinux/` | GPL-2.0-or-later | — |
| Grub4DOS 0.4.6a | `res/grub/`, downloaded `grldr` | GPL-2.0-or-later | — |
| FreeDOS 1.4 files | `res/freedos/` | GPL-2.0-or-later with binary redistribution allowed | `KERNEL.SYS` is modified upstream (FORCELBA) |
| KolibriOS loader | extracted from KolibriOS images | GPL-2.0-or-later | — |
| UEFI:NTFS image | `res/uefi/uefi-ntfs.img` | verify (pbatard/uefi-ntfs and its drivers) | — |
| uefi-md5sum bootloaders | `res/md5/` | verify (pbatard/uefi-md5sum) | Some are Secure Boot signed |
| UEFI revocation lists (DBX) | `res/dbx/` | verify (Microsoft `secureboot_objects`) | Updated from Microsoft |
| MS-DOS boot files (`diskcopy.dll`) | downloaded from the Microsoft symbol server | Microsoft proprietary | **Never redistributed**; downloaded at run time and verified against `DISKCOPY_HASH`, like upstream |
| Rufus icons | `res/icons/` | Rufus icon: freeware (PC Unleashed); other icons CC BY-ND 4.0 (Axialis) | **Not used**: Ferrufus has its own icon |

## Ferrufus' own license obligations

- Ferrufus is a derivative work of Rufus (GPL-3.0-or-later) and is licensed
  GPL-3.0-or-later. Every release must make the corresponding source available
  (the tagged commit in this repository).
- Translated code keeps its origin and copyright notices in file headers, and
  [NOTICE.md](../NOTICE.md) credits Rufus and the projects its translated parts
  derive from.
- Ferrufus must not be presented as an official Rufus release.
