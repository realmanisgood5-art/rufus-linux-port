# Architecture

## Goals

- One authoritative implementation of every storage operation, shared by the
  GUI and the CLI.
- Destructive work expressed as explicit, inspectable stages that can be shown
  to the user as a dry-run plan before anything is written.
- No project-owned `unsafe`, no project-owned C, no root GUI.
- Pure, testable logic separated from the code that touches the system.

## Workspace

| Crate | Kind | Responsibility |
|---|---|---|
| `ferrufus-core` | library | All shared logic: device discovery, listing policy, image inspection, checksums, safety checks, operation planning. Later: rules, partitioning, file systems, bootloaders, Windows and persistence workflows, downloads |
| `ferrufus-cli` | binary `ferrufus-cli` | Command-line front end: argument parsing, text and JSON output, exit codes |
| `ferrufus-gui` | binary `ferrufus` | Desktop front end (egui/eframe), unprivileged |
| `ferrufus-testkit` | dev library | Fake `/sys`, `/proc` and udev trees and synthetic disk images for tests. Depends only on `std` (never on `ferrufus-core`), so tests exercise the real public API |
| `xtask` | dev binary | Repository policy checks and generation of data tables from an upstream Rufus checkout |

Every crate root has `#![forbid(unsafe_code)]`, and the workspace sets
`unsafe_code = "forbid"` as a lint for all members. `cargo xtask policy`
verifies both.

When privileged writing arrives (M3), the code that runs with elevated rights
moves into its own small crate with a minimal dependency set, so that it can be
audited separately.

## `ferrufus-core` modules

| Module | Responsibility | Upstream counterpart |
|---|---|---|
| `upstream` | Baseline tag and commit; `UpstreamRef` labels attached to checks and plan steps | — |
| `error` | Structured error type (`thiserror`) with a stable error kind | — |
| `units` | Size constants and `size_to_human_readable()` | `src/stdio.c:SizeToHumanReadable()` |
| `cancel` | Cancellation token shared with worker threads | `ErrorStatus = ERROR_CANCELLED` checks |
| `hash` | Single-pass MD5/SHA-1/SHA-256/SHA-512 over any reader, with progress and cancellation | `src/hash.c` |
| `image` | Image inspection: extension and magic detection, MBR and ms-sys identification, GPT, ISO 9660/Joliet/El Torito, UDF, VHD, VHDX, WIM, FFU | `src/vhd.c:IsBootableImage()`, `src/drive.c:AnalyzeMBR()`, `src/iso.c:ExtractISO()` |
| `device` | Read-only discovery from sysfs, procfs and the udev database; system-disk detection; the Rufus listing policy; device identity and revalidation | `src/dev.c:GetDevices()`, `src/smart.c:IsHDD()` |
| `plan` | DD dry-run plans: pass/warning/blocking checks with their upstream rule, stages, a data-loss summary and a fingerprint; `prepare_dd()` runs the scan, inspection and planning for both front ends | `src/rufus.c:BootCheckThread()`, `src/format.c:FormatThread()`, `src/format.c:WriteDrive()` |

Planned modules: `rules` (M2), `partition` (M2/M4), `writer` (M3),
`filesystem` and `boot` (M4), `persistence` (M5), `windows` (M6),
`diagnostics` and `secureboot` (M7), `downloads` and `localization` (M8).

### System access is behind a root path

`device::SysRoot` names the directory that contains `sys/`, `proc/` and
`run/udev/data/`. Production code uses `/`; tests point it at a fake tree built
by `ferrufus-testkit`. Every read is size-limited, and missing or unreadable
attributes become `None` rather than errors where the kernel legitimately omits
them.

## The operation pipeline

Every destructive operation follows these stages. The same functions back the
GUI and the CLI; no front end can skip a stage.

| # | Stage | API (M1 unless noted) | Notes |
|---|---|---|---|
| 1 | Discover devices | `device::scan()` | Whole disks only |
| 2 | Identify each device and collect its properties | `DeviceFacts`, `DeviceIdentity` | `diskseq`, `dev_t`, size, block sizes, serial, transport, partitions, mounts, holders |
| 3 | Inspect the image | `image::inspect()` | Bounded reads of untrusted data |
| 4 | Calculate the layout and required capacity | `plan` (DD payload in M1; partition layouts in M2) | Checked arithmetic only |
| 5 | Produce a dry-run plan | `plan::plan_dd()` → `OperationPlan` | Device, partition changes, image size, write mode, formatting choices, data loss |
| 6 | Validate every target and parameter | `safety` checks inside the plan | Blocking checks make the plan non-executable |
| 7 | Confirm the exact operation (M3) | front end | Shows the plan; CLI requires the device identity and plan fingerprint |
| 8 | Revalidate the device immediately before writing (M3; `device::revalidate()` exists in M1) | `device::revalidate()` | Any identity change aborts |
| 9 | Execute with bounded buffers and checked I/O (M3) | `writer` | Exclusive open; short reads and writes handled; no truncation |
| 10 | Flush and verify (M3) | `writer` | `fsync`, read-back comparison |
| 11 | Report success only after required checks pass | `OperationReport` (M3) | Interruptions are reported as "contents undefined", never as rolled back |

In M1 stages 1–6 are implemented, and the plan's `executable` flag is always
`false`: there is no writer.

## Data model

- **`DeviceFacts`** — everything probed about one whole disk: kernel name,
  device node, `dev_t`, optional `diskseq`, size in bytes, logical and physical
  block size, removable and read-only flags, transport (USB with VID:PID,
  version, speed, serial and the `usb-storage`/`uas` driver; MMC; NVMe; ATA/SCSI;
  virtio; loop with its backing file; other), SCSI vendor and model, udev
  properties, partitions (number, start, size, file system type, label, UUID,
  mount points), holders, and whether it backs a system path or swap.
- **`ListingDecision`** — whether the disk is listed, with every reason
  (excluded because it is the system disk, hidden as a USB HDD with score +N,
  and so on). Reasons name the upstream rule they implement.
- **`DeviceIdentity`** — the subset of facts compared on revalidation.
- **`ImageReport`** — file size, extension-based compression association and
  magic verification, `is_iso`, `is_bootable_img` (non-bootable, bootable,
  forced), `is_vhd`, `is_windows_img`, `projected_size`, label, MBR type,
  partition table, ISO 9660 and UDF details, El Torito entries, VHD footer,
  warnings. Field names follow upstream `RUFUS_IMG_REPORT` where they mean the
  same thing; ISO-content fields (EFI loaders, Syslinux and GRUB versions,
  4 GB files) are absent until the M2 scanner exists.
- **`OperationPlan`** — schema version, target summary and identity, source
  summary, write mode, ordered `PlanStep`s (stage, description, destructive,
  implemented, upstream reference), `CheckResult`s, a data-loss summary,
  `executable` and the reasons it is not, and a SHA-256 fingerprint over the
  canonical JSON of the identity, the source and the steps.

## Threads, progress and cancellation

Core APIs are synchronous and take a `&CancelToken` and a progress callback
where work is long-running (hashing in M1). Front ends run them on worker
threads: the GUI uses `std::thread` and channels and never blocks its frame
loop; the CLI runs them on the main thread and cancels on Ctrl-C. Cancellation
is cooperative and reported as `Error::Cancelled`, never as success. No async
runtime is needed in M1.

## Privilege model

- The GUI **never runs as root**: it exits with an explanation if its
  effective UID is 0. Discovery, inspection, hashing and planning need no
  privileges (sysfs, procfs and the udev database are world-readable).
- Raw device access (M3) is requested per operation:
  1. **Preferred:** UDisks2 over D-Bus (`zbus`). `Block.OpenForRestore` and
     `Block.OpenForBackup` return a file descriptor after polkit
     authorization, so the process never gains general root rights.
     `Filesystem.Unmount` unmounts the target's partitions.
  2. **Fallback:** a small helper started with `pkexec` that accepts only a
     typed, versioned request (device identity, plan fingerprint, image path),
     revalidates everything itself, and never runs a shell.
- Configuration files, image metadata and downloaded files can never disable a
  safety check.

## Errors, logging and diagnostics

- `ferrufus-core` returns `Result<T, Error>`; every error has an `ErrorKind`
  used by the CLI to choose an exit code. I/O errors keep their source and the
  path or offset involved. Panics are treated as bugs, not error handling.
- Logging uses `tracing`. Listing decisions, inspection findings and plan
  checks are logged with the upstream rule they correspond to, so a Ferrufus
  log can be compared with a Rufus log.

## CLI

| Command | Purpose |
|---|---|
| `ferrufus-cli devices [--all] [-x/--list-usb-hdd] [--list-non-usb-removable] [--list-virtual]` | List candidate devices; `--all` also shows excluded devices with reasons |
| `ferrufus-cli inspect <IMAGE>` | Identify an image |
| `ferrufus-cli hash <FILE> [--sha512] [--algo LIST] [--expect ALGO:HEX]` | Checksums, optionally compared |
| `ferrufus-cli plan --device <DEVICE> --image <IMAGE> [--no-verify]` | Dry-run DD plan; the device can be a `/dev` path, a kernel name or a stable ID |

Every command accepts `--json`. JSON documents carry a `schema` field
(`ferrufus.devices/v1`, `ferrufus.image-report/v1`, `ferrufus.hash/v1`,
`ferrufus.plan/v1`); fields are only added within a schema version.

| Exit code | Meaning |
|---|---|
| 0 | Success |
| 1 | Runtime error |
| 2 | Usage error |
| 3 | Plan blocked by a safety check |
| 4 | Device not found or ambiguous |
| 5 | Image unreadable, malformed or unsupported |
| 6 | Checksum mismatch |
| 130 | Interrupted |

Non-interactive execution (M3) will require the exact device identity and the
plan fingerprint on the command line; there is no "yes to everything" switch
that skips revalidation.

## GUI

- egui through eframe with the `glow` (OpenGL) renderer, X11 and Wayland
  support loaded at run time, AccessKit for accessibility, and the XDG desktop
  portal file chooser (no GTK).
- The layout follows Rufus' main window (Drive Properties, Format Options,
  Status), but **only implemented features get controls**. M1 has no Start
  button and no formatting controls, and shows a permanent "read-only preview"
  banner.
- No device is ever selected automatically, including after a refresh.
- Hotplug: the GUI polls the list of block devices every two seconds and
  refreshes the list without changing the selection; a selected device that
  disappears is deselected with a message.

## Platform assumptions

- Linux with sysfs and procfs. `diskseq` (Linux 5.15 and later) is used when
  present; without it identity checks fall back to `dev_t`, size, serial and
  sysfs path, and the plan says so.
- The udev database is optional (containers may not have one); missing data
  reduces the detail shown but never relaxes the listing policy.
- The GUI needs OpenGL (or GLES), X11 or Wayland, and xkbcommon at run time.
  These are loaded dynamically from the system and are not bundled.
