# Rust porting plan

Ferrufus is a **source-guided port** of Rufus: the original C implementation
defines the behavior to reproduce, and each algorithm is translated into safe
Rust and checked against that behavior. It is not a mechanical conversion and
not a new design. Where Linux needs a different mechanism (device enumeration,
privileges, mounting), the *observable behavior* of Rufus is the target and the
difference is documented in [FEATURE_MATRIX.md](FEATURE_MATRIX.md).

## Principles

1. **Upstream defines behavior.** Option meanings, defaults, size limits, scan
   rules and error conditions come from the Rufus source, not from guesses.
   Every translated item names its upstream file and function.
2. **Safety overrides fidelity.** Where Rufus behavior could destroy data by
   accident (auto-selection, silent truncation), Ferrufus deviates and records
   the deviation (see [SECURITY.md](SECURITY.md#deliberate-deviations-from-upstream)).
3. **Rust only.** No project-owned C/C++, no `unsafe`, no FFI written by this
   project, no Wine, no wrapping the Windows executable. External programs are
   allowed only as explicit, validated backends for complex format-specific
   work (for example `mkntfs`), never as the application itself.
4. **One implementation.** The GUI and CLI share `ferrufus-core`; neither has
   its own storage logic.
5. **Honest status.** The feature matrix records exactly what is implemented,
   what is tested, and what is blocked.

## Upstream baseline

| Item | Value |
|---|---|
| Repository | https://github.com/pbatard/rufus |
| Behavioral baseline | **v4.15**, tag `v4.15`, commit `6d8fbf98305ff37eb531c45cbd6ff44563c53917` ("Rufus 4.15 (Build 2396)", 2026-06-30) |
| Tracked development head | `master` at `2a8ecf282801668f70945f99eb1f78a979ded5aa` (2026-10-28, `configure.ac` version 4.16) |
| License | GPL-3.0-or-later (`LICENSE.txt`) |
| Build systems | Visual Studio solution (`rufus.sln`), MinGW autotools (`configure.ac`, `Makefile.am`) |
| Upstream CI | `.github/workflows/vs2026.yml`, `mingw.yml`, `codeql.yml`, `coverity.yml`, `setup.yml` |

A released tag is used as the baseline because its signed executable
(`rufus-4.15.exe`) can be run on Windows to compare behavior for the same
inputs. The baseline moves only deliberately: when it does, every translated
module is re-diffed against the new tag and this document is updated.

### Upstream drift

Changes on `master` after v4.15 that affect files Ferrufus translates:

| Area | Change after v4.15 | Effect on Ferrufus |
|---|---|---|
| `src/hdd_vs_ufd.h` | Adds `{ "SSK Port able", -20 }` and VID:PID `1058:0702` (+5) | Not in the M1 tables; adopt when the baseline moves to 4.16 |
| `src/dev.c` | USB 3.x SuperSpeedPlus maximum-speed reporting (`get_max_bps()`, `version_to_speed()`) | Informational only; listing rules unchanged |
| `src/rufus.c:BootCheckThread()`, `src/stdio.c` | Downloaded GRUB/Syslinux files are re-validated when reused ("prevent TOCTOU when reusing the files downloaded for BIOS/Legacy boot") | Ferrufus' download cache must verify signatures on every use (M4/M8) |
| `src/drive.c` | File-system detection refactor in `GetDrivePartitionData()` | Re-check when porting partition-data probing |
| `src/format.c` | Drive-letter choice and remounting after writing raw images | Windows-only behavior |
| `src/libcdio/iso9660/iso9660_fs.c` | libcdio update | `get_member_id()` (label rule) is unchanged |

## Source map

| Upstream source | Responsibility | Ferrufus location |
|---|---|---|
| `src/dev.c` | Device enumeration, USB properties, port cycling, optical media | `ferrufus-core::device` |
| `src/smart.c`, `src/hdd_vs_ufd.h` | HDD-versus-flash heuristic, ATA/SCSI passthrough | `device::hdd_heuristic` (heuristic only; passthrough not needed) |
| `src/drive.c` | Handles, drive layout, MBR/PBR analysis, partitioning, mounting, ESP toggling | `device`, `image::mbr`, `partition` (M2/M4) |
| `src/format.c`, `src/format_fat32.c`, `src/format_ext.c` | Format thread, MBR/PBR/SBR writing, DD writes, FAT32 and ext formatting | `plan` (M1), write engine (M3), `filesystem`, `boot` (M4) |
| `src/iso.c` | ISO/UDF scanning and extraction, configuration patching | `image::iso9660`, `image::udf` (M1), ISO scanner (M2), extractor (M4) |
| `src/vhd.c` | Image type detection, VHD/VHDX, WIM operations, save-to-image | `image` (M1), image readers (M3/M6), save image (M7) |
| `src/hash.c` | Hash algorithms, hash UI thread, PE/Authenticode hashing, revocation checks | `hash` (M1), `secureboot` (M7) |
| `src/badblocks.c` | Bad blocks and fake drive detection | `diagnostics` (M7) |
| `src/syslinux.c`, `src/dos.c`, `src/dos_locale.c` | Syslinux, MS-DOS and FreeDOS installation | `boot` (M4/M5) |
| `src/wue.c`, `src/xml.c` | Windows unattend generation, WinPE, Windows To Go, edition lists | `windows` (M6) |
| `src/net.c`, `src/pki.c` | Downloads, update checks, Fido, signature validation | `downloads` (M8) |
| `src/rufus.c` | Main dialog logic: option rules, image scan thread, boot checks, cheat modes, command line | `rules` (M2), `image` and `plan` (M1), CLI/GUI |
| `src/ui.c`, `src/stdlg.c`, `src/darkmode.c`, `src/icon.c` | Win32 UI | `ferrufus-gui` (new egui UI; behavior only) |
| `src/localization.c`, `src/parser.c`, `res/loc/rufus.loc` | Localization | `localization` (M8, reusing `rufus.loc`) |
| `src/stdio.c`, `src/stdfn.c` | Logging, size formatting, file helpers, Windows helpers | `units`, `diagnostics` |
| `src/process.c` | Processes holding the drive | `device` (M3) |

## Windows APIs and their Linux equivalents

| Upstream mechanism | Used for | Linux equivalent |
|---|---|---|
| SetupAPI / Configuration Manager (`SetupDiGetClassDevs`, `CM_Get_Device_ID`) | Enumerating disks and USB properties | `/sys/class/block`, sysfs device ancestry, udev database `/run/udev/data` |
| `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`, `IOCTL_DISK_GET_DRIVE_LAYOUT_EX` | Size, sector size, partitions | sysfs `size`, `queue/logical_block_size`, partition directories; device reads through the privileged boundary later |
| `IOCTL_STORAGE_GET_DEVICE_NUMBER`, drive letters | Identifying volumes | `dev_t`, `/proc/self/mountinfo` |
| `FSCTL_LOCK_VOLUME`, `FSCTL_DISMOUNT_VOLUME` | Exclusive access | `O_EXCL` open of the block device, BSD `flock` (honored by udev), unmount through UDisks2 |
| VDS (`IVdsAdvancedDisk`), `IOCTL_DISK_CREATE_DISK`, `IOCTL_DISK_SET_DRIVE_LAYOUT_EX` | Partitioning | Own MBR/GPT writer; partition rescan via udev or UDisks2 |
| fmifs `FormatEx`, VDS `Format` | Formatting | Rust FAT16/FAT32; validated `mkntfs`, `mkfs.exfat`, `mkudffs`, `mke2fs` backends |
| `CreateVirtualDisk`, `AttachVirtualDisk` | VHD/VHDX mounting and saving | Rust VHD/VHDX readers and writers; loop devices for testing |
| `dism.exe`, `bcdboot.exe` | FFU apply/capture, Windows To Go boot files | No equivalent (blocked; see below) |
| WinINet | Downloads | Rust HTTP client with rustls (see the TLS blocker) |
| WinTrust / CryptoAPI | Signature checks | Rust RSA and X.509 verification |
| Registry, `rufus.ini` | Settings | XDG configuration file |
| `WM_DEVICECHANGE` | Hotplug | sysfs polling (M1), kernel uevents (later) |
| Process elevation (manifest) | Raw disk access | Unprivileged GUI; UDisks2 `OpenForRestore` file descriptors authorized by polkit; narrow helper as fallback |
| `NtQuerySystemInformation` handle scan | Conflicting processes | `/proc/*/fd` |

## Bundled third-party code

Rufus compiles several C libraries into its executable. None of them can be
reused by Ferrufus (no C), so each is replaced. Licenses are as stated in the
upstream About box (`src/license.h`, `additional_copyrights`) and file headers.

| Bundled code | License | Used for | Ferrufus replacement |
|---|---|---|---|
| `src/bled` (BusyBox/Bled decompressors) | GPL-2.0-or-later | gzip, xz, bzip2, lzma, zstd, zip, LZW, VTSI decompression | Pure-Rust decoders (`flate2` with `miniz_oxide`, `lzma-rs`, `bzip2` with the Rust backend, `ruzstd`, `zip`); own LZW and VTSI readers (M3) |
| `src/libcdio` | GPL-3.0-or-later | ISO 9660, Joliet, Rock Ridge, UDF reading | Own bounded, fuzzed parsers (volume descriptors in M1, directories in M2) |
| `src/ext2fs` (e2fsprogs) | e2fsprogs licenses (GPL-2.0 / LGPL-2.0 family, GPL-3.0 compatible) | ext2/3/4 formatting | Validated `mke2fs` backend first (M4) |
| `src/wimlib` | GPL-3.0-or-later as used by Rufus (wimlib files are LGPL-3.0-or-later) | WIM info, extraction, splitting, applying | Validated `wimlib-imagex` backend first; long-term pure-Rust WIM reader (M6) |
| `src/ms-sys` (Henrik Carlqvist) | GPL-2.0-or-later | MBR/PBR identification and writing | Byte tables generated from upstream; matchers translated (M1), writers (M4) |
| `src/syslinux` (libinstaller, libfat) | GPL-2.0-or-later | Syslinux installation | Translation of the installer logic (M4) |
| `src/getopt` | LGPL-2.1-or-later | Command-line parsing | `clap` |
| `src/cregex_*.c` (tiny-regex-c by Jim Huang) | BSD-2-Clause | Regular expressions for ISO label matching | Small explicit matchers or the `regex` crate (M4) |
| `src/xml.c` (ezxml by Aaron Voisine) | MIT | XML parsing of WIM metadata | A Rust XML parser (M6) |

Upstream code that is itself *derived* from other projects carries those
projects' attribution into Ferrufus' translations: `src/format_fat32.c` from
fat32format by Tom Thornhill (GPL-2.0-or-later), `src/hdd_vs_ufd.h` and parts
of `src/smart.c` from smartmontools (GPL-2.0-or-later), `src/badblocks.c` from
e2fsprogs, ETA computation in `src/ui.c` from GNU wget (GPL-3.0-or-later), PE
hashing in `src/hash.c` from U-Boot (GPL-2.0-or-later), and partition type tables
in `src/gpt_types.h`/`src/mbr_types.h` from GNU fdisk and GPT fdisk
(GPL-2.0-or-later). [NOTICE.md](../NOTICE.md) lists what has been translated so far.

## Validation strategy

Behaviors that **can be validated against the original executable**
(run Rufus v4.15 on Windows with the same inputs and compare):

- Cluster-size options and defaults for a range of drive sizes
  (`SetFileSystemAndClusterSize()`), file systems offered per boot selection,
  partition scheme and target system choices.
- Image classification and scan results, as printed in the Rufus log
  ("Image is an ISO9660 image", "Image has a Grub 2.0 Master Boot Record", the
  ISO properties list from `DisplayISOProps()`).
- Proposed volume labels and label sanitizing.
- Partition layouts: Rufus logs every partition as
  `● Creating <name> (offset: <bytes>, size: <size>)`.
- MBR/PBR bytes written to the media (by reading back a drive or VHD that Rufus
  produced).
- `unattend.xml` content for each Windows User Experience option set.
- Checksums of images.

These comparisons are recorded as test vectors in the repository, so that
Windows is only needed to produce them, not to run the tests.

Behaviors that **need independent specifications** (no Windows equivalent):
Linux device enumeration, system-disk detection, UDisks2/polkit privilege
handling, exclusive access and identity revalidation, and boot behavior of the
produced media (validated by booting it in virtual machines; see
[TESTING.md](TESTING.md)).

## Milestones

Phases A–G from the project brief map onto these milestones. Every milestone
ends with the feature matrix updated from actual test results.

| Milestone | Phase | Scope | Writes to devices |
|---|---|---|---|
| **M1** (this milestone) | A–D | Audit and documentation; workspace and policy gates; CI and AppImage releases; read-only device discovery with the Rufus listing policy; image identification; checksums; DD-mode dry-run plan; read-only CLI and GUI | None. No code path can open a device or file for writing |
| M2 | D | ISO 9660/Joliet/Rock Ridge/UDF directory reader (fuzzed); `check_iso_props()` scan rules; bootloader version detection; option rules (`SetAllowedFileSystems()`, `SetPartitionSchemeAndTargetSystem()`, `SetFileSystemAndClusterSize()`, `SetFSFromISO()`, `SetProposedLabel()`); `CreatePartition()` layout calculation; the remaining `BootCheckThread()` checks; dry-run plans for ISO mode and non-bootable formats | None |
| M3 | E | Write engine for disposable targets (regular files, loop devices): streaming DD writes, decompression, VHD reader, flush, read-back verification, cancellation, failure injection; privileged boundary (UDisks2); confirmation flow; physical writes behind a compile-time feature and explicit confirmation | Disposable targets; physical devices only behind the feature gate |
| M4 | F | MBR/GPT writing; FAT16/FAT32 (Large FAT32) formatting; NTFS, exFAT, UDF and ext through validated backends; ISO-mode extraction; configuration patching; MBR/PBR/SBR writing; Syslinux and GRUB installation; UEFI:NTFS | Yes (gated) |
| M5 | F | Linux persistence; FreeDOS; MS-DOS (runtime download); Grub4DOS; ReactOS; KolibriOS | Yes (gated) |
| M6 | F | Windows media: WIM support, edition selection, `install.wim` splitting, Windows User Experience `unattend.xml`, WinPE and Windows 7 EFI fixups, VHDX reader | Yes (gated) |
| M7 | F | Diagnostics: bad blocks and fake drive detection; save a drive to raw/VHD/VHDX; optical disc to ISO; Secure Boot revocation checks; runtime UEFI media validation | Yes (gated) |
| M8 | F | Downloads (UEFI Shell, signed files, update checks, DBX updates; Fido feasibility); localization from `rufus.loc`; settings; hotplug through uevents; user ignore lists | — |
| Release readiness | G | Feature coverage review; VM boot test matrix; hardware validation; security review; distribution packaging; documented limitations | — |

## Hardest blockers and proposed solutions

| Blocker | Why | Proposed solution |
|---|---|---|
| WIM split and apply (W02, W15) | wimlib is C; LZX/XPRESS/LZMS and the WIM format are large | Use `wimlib-imagex` as an explicit, validated backend first; build a pure-Rust WIM reader (decompression and metadata) for splitting later |
| NTFS, exFAT, UDF formatting (F03–F05) | No mature pure-Rust formatters | Validated external backends (`mkntfs`, `mkfs.exfat`, `mkudffs`) invoked without a shell, with arguments built from typed values and a re-validated device path; evaluate Rust exFAT later |
| ReFS (F06) | No ReFS formatter exists on Linux | Blocked; documented |
| Windows To Go (W15, W16) | Needs `bcdboot` and a BCD registry hive | Blocked; investigate a Rust registry-hive writer after WIM apply exists |
| FFU apply and capture (I11, H06) | Only `dism` implements it | Blocked pending an independent FFU specification |
| Windows ISO downloads (N01) | Upstream runs the Fido PowerShell script against Microsoft's download service | Evaluate a Rust implementation of the same requests, including Microsoft's terms and rate limiting; until then, link users to Microsoft's download page |
| MS-DOS boot files (B03) | Microsoft files cannot be redistributed | Same as upstream: download `diskcopy.dll` from the Microsoft symbol server at run time and verify its SHA-256 (`DISKCOPY_HASH`) |
| TLS without C (M8) | rustls' usual crypto providers (`ring`, `aws-lc-rs`) contain C and assembly | Prefer a pure-Rust provider (`rustls-rustcrypto`) if it is mature enough; otherwise document `ring` as the single exception in DEPENDENCIES.md |
| Privileged access without a root GUI (M3) | Raw block devices need privileges | UDisks2 `Block.OpenForRestore`/`OpenForBackup` return a file descriptor after polkit authorization (D-Bus via `zbus`); fallback: a small helper started through `pkexec` that accepts only a typed, fully revalidated request |
| Device identity races | `/dev/sdX` names are reused | Compare `diskseq` (Linux 5.15+), `dev_t`, size, serial and sysfs path before every destructive stage; verify that an opened file descriptor still refers to the same `diskseq` |

## Translation conventions

- Every translated file starts with an SPDX `GPL-3.0-or-later` header naming
  the upstream files and functions, their copyright holders, and the baseline
  tag.
- Code comments reference upstream as `src/file.c:Function()`.
- Data tables (ms-sys MBR byte arrays, HDD heuristic tables) are **generated**
  from an upstream checkout by `cargo xtask gen-tables --upstream <path>`, never
  retyped. Generated files say so and name the upstream tag.
- Rust names follow Rust conventions, but report fields keep the upstream names
  where practical (`is_iso`, `is_bootable_img`, `projected_size`) so that log
  output and test vectors can be compared directly.
- Floating-point comparisons in upstream rules are reproduced with the same
  precision (`f32` where upstream uses `float`).
