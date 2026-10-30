# Ferrufus feature matrix

This matrix tracks every feature found in the audit of upstream Rufus and its
status in Ferrufus. It is the authoritative answer to "what works?". Nothing in
this project claims feature parity until every row is either implemented and
tested or has its remaining difference documented here.

- **Behavioral baseline:** Rufus **v4.15** — tag `v4.15`, commit
  `6d8fbf98305ff37eb531c45cbd6ff44563c53917` (2026-06-30). Upstream `master` at
  `2a8ecf282801668f70945f99eb1f78a979ded5aa` (4.16 in development) is tracked in
  [RUST_PORTING_PLAN.md](RUST_PORTING_PLAN.md#upstream-drift).
- **Upstream references** use `path:function()` relative to the Rufus source tree.
  Line numbers, where given, are at the v4.15 baseline.
- **Milestones** are defined in [RUST_PORTING_PLAN.md](RUST_PORTING_PLAN.md#milestones).

## Status legend

| Symbol | Meaning |
|---|---|
| ✅ | Implemented and tested |
| 🟡 | Implemented but not fully tested |
| 🟠 | Partially supported |
| ⛔ | Blocked by Windows-specific behavior (see notes for the proposed alternative) |
| ⬜ | Not implemented |
| ➖ | Not applicable to Linux |

"Tested" means covered by automated tests in this repository. A successful
write is never taken as proof of bootability; boot validation is tracked
separately in [TESTING.md](TESTING.md).

## A. Device discovery and selection

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| D01 | List USB mass-storage (BOT) drives | `src/dev.c:GetDevices()` (`usbstor_name[]`) | Whole disks from `/sys/class/block`; USB transport from the sysfs device path | ✅ | M1 |
| D02 | List UAS (UASP) drives | `src/dev.c:GetDevices()` (`UASPSTOR` and later entries) | USB interface driver `uas` | ✅ | M1 |
| D03 | List card readers (USB and non-USB SD/MMC) | `src/dev.c:GetDevices()` (`genstor_name[]`, `scsi_card_name[]`) | `mmcblk*` devices; upstream SCSI card substrings applied to the INQUIRY vendor/model | ✅ | M1 |
| D04 | List mounted virtual disks (VHD), `enable_VHDs`, Alt-G | `src/dev.c:GetDevices()` (`IsVHD()`), `src/rufus.c` (`SETTING_DISABLE_VHDS`) | Loop devices with a backing file. **Opt-in** (`--list-virtual`): Linux loops are usually system-managed (snaps, containers) | ✅ | M1 |
| D05 | Hide USB hard drives unless "List USB Hard Drives" (`-x/--extra-devs`, Alt-F) | `src/smart.c:IsHDD()`, `src/hdd_vs_ufd.h` | Faithful translation of the scoring heuristic and tables | ✅ | M1 |
| D06 | Hide cards larger than 500 GB unless "List USB Hard Drives" | `src/dev.c:GetDevices()` (`MAX_DEFAULT_LIST_CARD_SIZE`) | Same rule | ✅ | M1 |
| D07 | List non-USB removable drives only on request (Ctrl-Alt-F) | `src/dev.c:GetDevices()` (`list_non_usb_removable_drives`) | `--list-non-usb-removable`, with a caution | ✅ | M1 |
| D08 | Never list non-USB fixed disks | `src/dev.c:GetDevices()` ("most likely dealing with a system drive") | Same rule (NVMe, SATA, virtio, and so on) | ✅ | M1 |
| D09 | Hide drives without media or smaller than 8 MB | `src/dev.c:GetDevices()` (`IsMediaPresent()`, `MIN_DRIVE_SIZE`) | `size == 0` means no media; same 8 MB minimum | ✅ | M1 |
| D10 | Exclude the system disk and the disk Rufus runs from | `src/dev.c:GetDevices()` (`app_dir`, `system_dir`) | Disks backing `/` and system directories (`/boot`, `/efi`, `/usr`, `/var`, `/home`, ...), active swap, or the running executable/AppImage, through partitions, dm/md holders, loop backing files, btrfs members and overlays. An unresolved system mount blocks every plan | ✅ | M1 |
| D11 | User ignore lists (`IgnoreUsb01`–`08` VID:PID, `IgnoreDisk01`–`08` GPT disk GUID) | `src/dev.c:GetDevices()`, `src/drive.c:IsFilteredDrive()` | Configuration file entries | ⬜ | M8 |
| D12 | Hide Microsoft Dev Drives, Windows Sandbox and Bitdefender VHDs | `src/drive.c:IsMsDevDrive()`, `src/dev.c:GetDevices()` | Windows-only artifacts | ➖ | — |
| D13 | Sort drives by size; display "label (letters) [size]" | `src/dev.c:GetDevices()`, `src/stdio.c:SizeToHumanReadable()` | Same sort and size formatting; device node shown instead of drive letters | ✅ | M1 |
| D14 | Reselect the previous drive, otherwise select the first one | `src/dev.c:GetDevices()` (lines 1072–1081) | **Deliberate deviation:** never auto-select a device; selection always starts empty | ✅ | M1 |
| D15 | USB version and speed reporting | `src/dev.c:GetUSBProperties()` | sysfs `version` and `speed` of the USB device | ✅ | M1 |
| D16 | Refresh the list on hotplug | `src/rufus.c:MainCallback()` (`WM_DEVICECHANGE`) | Periodic sysfs polling in M1; kernel uevents later. Never changes the selection | 🟡 | M1 |
| D17 | Cycle USB port / reset device (Alt-C) | `src/dev.c:CyclePort()`, `CycleDevice()` | USB `authorized` toggle through the privileged boundary | ⬜ | M8 |
| D18 | Report processes that hold the drive open | `src/process.c` (`SearchProcessThread()`) | `/proc/*/fd` scan; requires privileges for other users' processes | ⬜ | M3 |
| D19 | Stable device identity and revalidation before destructive steps | Not in upstream (drive index and label only) | `diskseq`, `dev_t`, size, block size, serial, model, sysfs path | ✅ | M1 |
| D20 | Show existing partitions, file systems and labels | `src/drive.c:GetDrivePartitionData()` | udev database and sysfs (no device read access needed) | ✅ | M1 |
| D21 | USB enumeration debug output (Alt-.) | `src/dev.c` (`usb_debug`) | `ferrufus-cli devices --all` shows every exclusion reason with its upstream rule; `FERRUFUS_LOG=debug` | 🟡 | M1 |

## B. Partitioning

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| P01 | MBR partition scheme | `src/drive.c:CreatePartition()` | Own MBR writer through the privileged boundary | ⬜ | M4 |
| P02 | GPT partition scheme; force GPT above 2 TB | `src/rufus.c:SetPartitionSchemeAndTargetSystem()`, `src/drive.c:CreatePartition()` | Rule translation (M2), own GPT writer (M4) | ⬜ | M2/M4 |
| P03 | Super floppy disk (no partition table) for non-bootable drives | `src/rufus.c` (`PARTITION_STYLE_SFD`) | Same | ⬜ | M2/M4 |
| P04 | Layout calculation: 1 MB start, 260 MB ESP, 128 MB MSR, persistence, UEFI:NTFS, BIOS-compatibility partitions | `src/drive.c:CreatePartition()` | Pure calculation with property tests | ⬜ | M2 |
| P05 | "Add fixes for old BIOSes" (track alignment, extra partition) | `src/drive.c:CreatePartition()` (`IDC_OLD_BIOS_FIXES`, `XP_COMPAT`) | Same layout rules | ⬜ | M4 |
| P06 | Clear existing MBR/GPT before partitioning | `src/format.c:ClearMBRGPT()` (`MAX_SECTORS_TO_CLEAR`) | Same regions, checked writes | ⬜ | M4 |
| P07 | MBR "UEFI" disk signature marker | `src/drive.c:CreatePartition()` (`MBR_UEFI_MARKER`) | Same | ⬜ | M4 |
| P08 | Toggle the ESP partition type (Alt-P) | `src/drive.c:ToggleEsp()` | GPT type GUID rewrite | ⬜ | M8 |
| P09 | Delete existing partitions | `src/drive.c:DeletePartition()` (VDS) | Own partition-table writes; VDS itself is Windows-only | ⬜ | M4 |
| P10 | Refresh the drive layout after writing | `src/drive.c:RefreshDriveLayout()` | Kernel partition rescan through udev or UDisks2 | ⬜ | M3 |

## C. File systems and formatting

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| F01 | FAT16 | `src/format.c:FormatNative()` (fmifs `FormatEx`) | Rust FAT formatter | ⬜ | M4 |
| F02 | FAT32, including Large FAT32 above 32 GB (Alt-L forces it) | `src/format_fat32.c:FormatLargeFAT32()` | Translation of the Large FAT32 algorithm | ⬜ | M4 |
| F03 | NTFS (Alt-N enables compression) | `src/format.c:FormatNative()`, `FormatNativeVds()` | Validated `mkntfs` backend (no Rust implementation exists) | ⬜ | M4 |
| F04 | exFAT | `src/format.c:FormatNative()` | Validated `mkfs.exfat` backend, or a Rust formatter | ⬜ | M4 |
| F05 | UDF (`UseUdfVersion`) | `src/format.c` | Validated `mkudffs` backend | ⬜ | M4 |
| F06 | ReFS | `src/rufus.c:IsRefsAvailable()`, `src/format.c` (VDS) | No ReFS formatter exists on Linux | ⛔ | — |
| F07 | ext2/ext3/ext4 (`UseExtVersion`) | `src/format_ext.c:FormatExtFs()` (bundled ext2fs) | Validated `mke2fs` backend, or Rust later | ⬜ | M4 |
| F08 | Allowed cluster sizes and defaults per file system and drive size | `src/rufus.c:SetFileSystemAndClusterSize()`, `SetClusterSizes()` | Pure translation, including the `float` comparisons | ⬜ | M2 |
| F09 | Proposed volume label and label sanitizing | `src/rufus.c:SetProposedLabel()`, `src/format.c:ToValidLabel()` | Pure translation | ⬜ | M2 |
| F10 | Quick versus full format | `src/format.c:FormatThread()` (`FP_QUICK`) | Same | ⬜ | M4 |
| F11 | File systems allowed per boot selection | `src/rufus.c:SetAllowedFileSystems()` | Pure translation | ⬜ | M2 |
| F12 | Disable or enable file indexing (Alt-Q) | `src/format.c:FormatThread()` | Windows Search indexing attribute | ➖ | — |
| F13 | CheckDisk after NTFS ISO extraction | `src/format.c:CheckDisk()` | Evaluate `ntfsfix`; may be unnecessary on Linux | ⬜ | M4 |
| F14 | Zero the drive (Alt-Z), fast-zeroing (Ctrl-Alt-Z) | `src/format.c:WriteDrive()` (`bZeroDrive`, `fast_zeroing`) | Same algorithm through the write engine | ⬜ | M3 |
| F15 | Extended label and icon files (`autorun.inf`) | `src/icon.c:SetAutorun()` | Same files on the media | ⬜ | M4 |

## D. Boot selection

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| B01 | Non bootable | `src/rufus.c` (`BT_NON_BOOTABLE`) | Plan in M2, execute in M4 | ⬜ | M2/M4 |
| B02 | Disk or ISO image | `src/rufus.c` (`BT_IMAGE`) | DD-mode planning in M1; see section E | 🟠 | M1+ |
| B03 | MS-DOS (`diskcopy.dll` downloaded from the Microsoft symbol server) | `src/dos.c:ExtractMSDOS()`, `src/rufus.c:BootCheckThread()` | Runtime download only (Microsoft files are never redistributed) | ⬜ | M5 |
| B04 | FreeDOS | `src/dos.c:ExtractFreeDOS()`, `res/freedos/` | Same files and patching | ⬜ | M5 |
| B05 | Syslinux 4.07 and 6.04 (advanced mode) | `src/syslinux.c:InstallSyslinux()`, `res/syslinux/` | Translation of the installer logic | ⬜ | M4 |
| B06 | ReactOS | `src/format.c:FormatThread()` (`BT_REACTOS`), ms-sys `mbr_reactos.h` | Same | ⬜ | M5 |
| B07 | GRUB 2.14 (advanced mode) | `src/format.c:WriteMBR()`, `WriteSBR()`, `res/grub2/` | Same `boot.img`/`core.img` handling | ⬜ | M4 |
| B08 | Grub4DOS 0.4.6a | `src/format.c:FormatThread()` (`grldr` download) | Same | ⬜ | M5 |
| B09 | UEFI:NTFS (advanced mode; NTFS or exFAT) | `src/drive.c:CreatePartition()` (`XP_UEFI_NTFS`), `res/uefi/uefi-ntfs.img` | Same partition and image | ⬜ | M4 |
| B10 | KolibriOS | `src/format.c:FormatThread()` (`MTLD_F32` loader) | Same | ⬜ | M5 |

## E. Image handling

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| I01 | ISO 9660 detection and volume label (Joliet preference rule) | `src/iso.c:ExtractISO()`, libcdio `iso9660_fs.c:get_member_id()` | Own bounded parser; exact label rule | ✅ | M1 |
| I02 | UDF detection and logical volume label (tried before ISO 9660) | `src/iso.c:ExtractISO()` (`udf_open()`, `udf_get_logical_volume_id()`) | Own bounded parser | ✅ | M1 |
| I03 | ISO content scan (bootloaders, EFI loaders, files over 4 GB, Windows, and so on) | `src/iso.c:check_iso_props()`, `src/rufus.c:GetBootladerInfo()` | Directory reader plus pure translation of the scan rules | ⬜ | M2 |
| I04 | ISO extraction (file-copy mode) | `src/iso.c:ExtractISO()`, `iso_extract_files()`, `udf_extract_files()` | Same, with path sanitizing | ⬜ | M4 |
| I05 | ISOHybrid detection and the ISO-mode versus DD-mode choice (ISO mode recommended) | `src/rufus.c:ImageScanThread()`, `BootCheckThread()` | M1 offers DD mode only and reports that upstream recommends ISO mode | 🟠 | M1/M4 |
| I06 | ISO → ESP mode for small EFI images on GPT | `src/rufus.c:BootCheckThread()` (`MAX_ISO_TO_ESP_SIZE`) | Same | ⬜ | M4 |
| I07 | Write a raw disk image (DD mode) | `src/format.c:WriteDrive()`, `FormatThread()` | Dry-run plan in M1; streaming writer and read-back verification in M3 | 🟠 | M1/M3 |
| I08 | Compressed images (`.gz .xz .bz2 .zst .lzma .zip .Z .vtsi`) | `src/vhd.c:IsCompressedBootableImage()` (`file_assoc[]`), bled | Detection (extension plus magic) in M1; pure-Rust decompressors in M3 | 🟠 | M1/M3 |
| I09 | VHD images (fixed and dynamic) | `src/vhd.c:VhdMountImageAndGetSize()` | Footer parsing in M1; reader in M3 | 🟠 | M1/M3 |
| I10 | VHDX images | `src/vhd.c` | Signature in M1; reader in M6 | 🟠 | M1/M6 |
| I11 | Apply FFU images | `src/format.c:FormatThread()` (`dism /Apply-Ffu`) | dism-only; needs an independent FFU specification | ⛔ | — |
| I12 | WIM files as boot selection | `src/vhd.c:IsBootableImage()` (`WIM_MAGIC`), `src/rufus.c:ImageScanThread()` | Detection in M1; handling in M6 | 🟠 | M1/M6 |
| I13 | Boot marker check; ignore it on request (Alt-M) | `src/vhd.c:IsBootableImage()`, `src/drive.c:AnalyzeMBR()` | Same 0/1/2 result | ✅ | M1 |
| I14 | Identify the MBR type (ms-sys signatures) | `src/drive.c:AnalyzeMBR()`, `src/ms-sys/br.c` | Byte tables generated from upstream; upstream match order | ✅ | M1 |
| I15 | Image larger than the drive check (Alt-S disables it) | `src/rufus.c:BootCheckThread()` (line 1531) | **Stricter:** no 4 KB slack for raw payloads; the check cannot be disabled | ✅ | M1 |
| I16 | Refuse an image stored on the target drive | `src/drive.c:IsSourceImageLocatedOnTargetDrive()` | The image's disk is found through its `st_dev` (running system) or its mount, then partitions and holders; an image whose disk cannot be determined blocks the plan | 🟡 | M1 |
| I17 | Copy additional files from a zip archive (Ctrl-SELECT) | `src/rufus.c:MainCallback()`, `src/stdio.c:ExtractZip()` | Same | ⬜ | M4 |
| I18 | Toggle Joliet (Alt-J), Rock Ridge (Alt-K), ISO support (Alt-I) | `src/iso.c` (`enable_joliet`, `enable_rockridge`, `enable_iso`) | Expert options | ⬜ | M2 |
| I19 | Preserve timestamps (Alt-T) | `src/iso.c` (`preserve_timestamps`) | Same | ⬜ | M4 |
| I20 | Symbolic links (Rock Ridge and UDF) | `src/iso.c` (`has_symlinks`) | Same | ⬜ | M4 |
| I21 | Save an optical disc to ISO (Alt-O) | `src/iso.c:OpticalDiscSaveImage()`, `src/dev.c:GetOpticalMedia()` | Read `/dev/sr*` | ⬜ | M7 |

## F. Bootloader installation and configuration

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| L01 | Write the MBR (Rufus, Windows 7, GRUB, Syslinux, and others) | `src/format.c:WriteMBR()`, `src/ms-sys/br.c` | Same tables | ⬜ | M4 |
| L02 | Write partition boot records (FAT16, FAT32, NTFS) | `src/format.c:WritePBR()`, `src/ms-sys/fat16.c`, `fat32.c`, `ntfs.c` | Same | ⬜ | M4 |
| L03 | Write the GRUB 2 core image after the MBR | `src/format.c:WriteSBR()` | Same | ⬜ | M4 |
| L04 | Install Syslinux; match and download the ISO's Syslinux version | `src/syslinux.c:InstallSyslinux()`, `src/rufus.c:BootCheckThread()` | Same, with signed downloads | ⬜ | M4 |
| L05 | Match and download the ISO's GRUB `core.img` version | `src/rufus.c:BootCheckThread()`, `src/iso.c:GetGrubVersion()` | Same | ⬜ | M4 |
| L06 | Patch configuration files (labels, paths) | `src/iso.c:fix_config()` | Same | ⬜ | M4 |
| L07 | Replace obsolete `menu.c32`/`vesamenu.c32` | `src/rufus.c:BootCheckThread()`, `src/iso.c` | Same | ⬜ | M4 |
| L08 | Toggle the Rufus MBR for Windows boot (Alt-A) | `src/format.c:WriteMBR()` (`SETTING_DISABLE_RUFUS_MBR`) | Same | ⬜ | M4 |
| L09 | Windows 7 EFI boot setup | `src/format.c:FormatThread()` (Win7 EFI) | Same | ⬜ | M6 |
| L10 | WinPE (XP/2003) setup fixups | `src/wue.c:SetupWinPE()` | Same | ⬜ | M6 |
| L11 | Runtime UEFI media validation (uefi-md5sum, `md5sum.txt`) | `src/hash.c:UpdateMD5Sum()`, `res/md5/` | Same | ⬜ | M7 |
| L12 | El Torito UEFI image extraction (vendor firmware update ISOs) | `src/iso.c` (`[BOOT]/` images) | Same | ⬜ | M4 |

## G. Windows installation media

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| W01 | Windows 8/10/11 installation media (ISO mode, NTFS with UEFI:NTFS, or FAT32) | `src/format.c:FormatThread()` | Same flow | ⬜ | M6 |
| W02 | Split `install.wim` over 4 GB to `.swm` for FAT32 (dual UEFI/BIOS, Alt-E) | `src/iso.c:check_iso_props()`, `src/vhd.c:WimSplitFile()` | Needs WIM support (see blockers) | ⬜ | M6 |
| W03 | Detect the Windows version from the image | `src/wue.c:PopulateWindowsVersion()` | WIM XML metadata | ⬜ | M6 |
| W04 | Remove the 4 GB RAM, Secure Boot and TPM 2.0 requirements | `src/wue.c:CreateUnattendXml()` (`UNATTEND_SECUREBOOT_TPM_MINRAM`) | Same `unattend.xml` content | ⬜ | M6 |
| W05 | Remove the online Microsoft account requirement | `src/wue.c:CreateUnattendXml()` (`UNATTEND_NO_ONLINE_ACCOUNT`) | Same | ⬜ | M6 |
| W06 | Create a local account | `src/wue.c:CreateUnattendXml()` (`UNATTEND_SET_USER`, `USERNAME_INVALID_CHARS`) | Same validation | ⬜ | M6 |
| W07 | Copy regional options from the current user | `src/wue.c:CreateUnattendXml()` (`UNATTEND_DUPLICATE_LOCALE`) | Map the Linux locale to Windows locale names | ⬜ | M6 |
| W08 | Disable data collection (skip privacy questions) | `src/wue.c:CreateUnattendXml()` (`UNATTEND_NO_DATA_COLLECTION`) | Same | ⬜ | M6 |
| W09 | Disable BitLocker automatic device encryption | `src/wue.c:CreateUnattendXml()` (`UNATTEND_DISABLE_BITLOCKER`) | Same | ⬜ | M6 |
| W10 | Quality-of-life improvements (Copilot, OneDrive, Outlook, Fast Startup, and so on) | `src/wue.c:CreateUnattendXml()` (`UNATTEND_QOL_ENHANCEMENTS`) | Same | ⬜ | M6 |
| W11 | Use "Windows CA 2023" signed bootloaders | `src/wue.c` (`UNATTEND_USE_MS2023_BOOTLOADERS`, `bcdboot_supports_ex`) | Depends on `bcdboot`; evaluate | ⛔ | — |
| W12 | Apply `SkuSiPolicy.p7b` on installation | `src/wue.c:CreateUnattendXml()` (`UNATTEND_APPLY_SKUSIPOLICY`) | Same | ⬜ | M6 |
| W13 | Silent installation | `src/wue.c:CreateUnattendXml()` (`UNATTEND_SILENT_INSTALL`) | Same | ⬜ | M6 |
| W14 | Restrict Windows to S mode (expert mode) | `src/wue.c:CreateUnattendXml()` (`UNATTEND_FORCE_S_MODE`) | Same | ⬜ | M6 |
| W15 | Windows To Go | `src/wue.c:SetupWinToGo()` (wimlib apply, `bcdboot`) | Needs WIM apply and a BCD store writer | ⛔ | — |
| W16 | Prevent Windows To Go from accessing internal disks | `src/wue.c:CreateUnattendXml()` (`UNATTEND_OFFLINE_INTERNAL_DRIVES`) | Depends on W15 | ⛔ | — |
| W17 | Detect an existing `$OEM$/$$/Panther/unattend.xml` | `src/iso.c:check_iso_props()` | Scan rule | ⬜ | M2 |
| W18 | Choose among multiple Windows editions | `src/wue.c:SetWinToGoIndex()`, `GetEditions()` | Same | ⬜ | M6 |

## H. Linux and other distributions

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| X01 | Persistent partition: Ubuntu `casper-rw` or Debian `persistence` with `persistence.conf` | `src/format.c:FormatThread()`, `src/format_ext.c:FormatExtFs()`, `src/iso.c:fix_config()` | Per-distribution mechanism, as upstream | ⬜ | M5 |
| X02 | Persistence size controls | `src/ui.c:SetPersistenceSize()` (`PROJECTED_SIZE_RATIO`) | Same | ⬜ | M5 |
| X03 | Decide whether an image supports persistence | `src/rufus.h` (`HAS_PERSISTENCE`) | Same rule | ⬜ | M2 |
| X04 | Red Hat 8+ derivatives: `inst.stage2` → `inst.repo` | `src/rufus.c:ImageScanThread()` (`redhat8_derivative[]`), `src/iso.c:fix_config()` | Same patterns | ⬜ | M4 |
| X05 | Force DD mode for SUSE, Manjaro, Pop!_OS and Proxmox images | `src/rufus.c:ImageScanThread()`, `src/iso.c:check_iso_props()` | Same rules | ⬜ | M2 |
| X06 | Detect the file systems supported by an EFI GRUB | `src/iso.c:GetGrubFs()` | Same | ⬜ | M2 |
| X07 | Patch archiso `loader/entries/*.conf` | `src/iso.c:check_iso_props()`, `fix_config()` | Same | ⬜ | M4 |
| X08 | Replace a broken `bootx64.efi` symlink (Linux Mint) | `src/iso.c:check_iso_props()` | Same | ⬜ | M4 |

## I. Images and diagnostics

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| H01 | MD5, SHA-1 and SHA-256 of the selected image | `src/hash.c:HashThread()`, `HashFile()` | RustCrypto, one pass over the file | ✅ | M1 |
| H02 | SHA-512 (Alt-H, `EnableExtraHashes`) | `src/hash.c` (`enable_extra_hashes`) | `--sha512` | ✅ | M1 |
| H03 | Compare against an expected checksum | Not in upstream | `--expect algo:hex` | ✅ | M1 |
| H04 | Save a drive to VHD | `src/vhd.c:SaveImage()` (`CreateVirtualDisk`) | Fixed VHD writer | ⬜ | M7 |
| H05 | Save a drive to VHDX | `src/vhd.c:SaveImage()` | VHDX writer | ⬜ | M7 |
| H06 | Save a drive to FFU | `src/vhd.c` (`dism /Capture-Ffu`) | dism-only | ⛔ | — |
| H07 | Save a drive to ISO (UDF) | `src/vhd.c`, `OSCDIMG_URL` | Uses Microsoft `oscdimg.exe`; evaluate a Rust writer | ⛔ | — |
| H08 | Bad blocks check (1–4 passes; SLC, MLC and TLC patterns) | `src/badblocks.c:BadBlocks()` | Translation (derived from e2fsprogs badblocks) | ⬜ | M7 |
| H09 | Fake capacity detection (Alt-B disables it) | `src/badblocks.c` (`detect_fakes`) | Same block-numbering method | ⬜ | M7 |
| H10 | Read-back verification after a DD write | Not in upstream | Port addition, on by default | ⬜ | M3 |
| H11 | Log window (save, clear), persistent log (Ctrl-P) | `src/rufus.c:LogCallback()` | GUI log panel in M1; saving later | 🟡 | M1 |

## J. Secure Boot and UEFI checks

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| S01 | Warn about bootloaders revoked by the DBX | `src/hash.c:IsRevokedByDbx()`, `res/dbx/` | Authenticode hashing in Rust | ⬜ | M7 |
| S02 | SBAT revocation | `src/hash.c:IsRevokedBySbat()` | Same | ⬜ | M7 |
| S03 | Microsoft SVN revocation | `src/hash.c:IsRevokedBySvn()` | Same | ⬜ | M7 |
| S04 | Revoked certificates | `src/hash.c:IsRevokedByCert()` | Same | ⬜ | M7 |
| S05 | DBX update check | `src/net.c:CheckForDBXUpdates()` | HTTPS download | ⬜ | M8 |
| S06 | Detect Secure Boot signed bootloaders | `src/hash.c:IsSignedBySecureBootAuthority()` | Same | ⬜ | M7 |

## K. Downloads and updates

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| N01 | Download official Windows 8/10/11 ISOs (Fido) | `src/net.c:DownloadISO()` (Fido PowerShell script) | PowerShell is not available; evaluate a Rust implementation of the same protocol | ⛔ | M8 (evaluate) |
| N02 | Download UEFI Shell ISOs | Fido (`src/net.c:DownloadISO()`) | Direct HTTPS download of pbatard/UEFI-Shell releases | ⬜ | M8 |
| N03 | Check for application updates (interval, betas) | `src/net.c:CheckForUpdates()` | Point at Ferrufus releases, never at Rufus | ⬜ | M8 |
| N04 | Signed file downloads | `src/net.c:DownloadSignedFile()`, `src/pki.c:ValidateOpensslSignature()` | Rust signature verification | ⬜ | M8 |
| N05 | Database of known-good downloadable files | `src/db.h`, `src/hash.c:IsFileInDB()` | Same hashes | ⬜ | M4 |
| N06 | Verify the Authenticode signature of a new version | `src/pki.c:ValidateSignature()` | Windows-only; Ferrufus uses its own release checksums and provenance | ➖ | — |

## L. User interface

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| U01 | Main window layout (Drive Properties, Format Options, Status) | `src/rufus.rc` (`IDD_DIALOG`) | egui; only implemented controls are shown | 🟠 | M1 (read-only subset) |
| U02 | Advanced drive properties and format options sections | `src/ui.c:ToggleAdvancedDeviceOptions()`, `ToggleAdvancedFormatOptions()` | Same | 🟠 | M1/M2 |
| U03 | Localization (38 languages, `rufus.loc`, right-to-left) | `src/localization.c`, `src/parser.c`, `res/loc/rufus.loc` | Reuse `rufus.loc` | ⬜ | M8 |
| U04 | Dark mode (Ctrl-Alt-D) | `src/darkmode.c` | Follows the desktop theme (eframe default; not covered by tests) | 🟡 | M1 |
| U05 | Progress with speed and time estimate; taskbar progress | `src/ui.c:UpdateProgress()`, `src/stdlg.c:SetTaskbarProgressValue()` | Progress in the write engine; taskbar progress is Windows-only | ⬜ | M3 |
| U06 | Destructive operation confirmation | `src/rufus.c:MainCallback()` (`MSG_003`) | Exact device, identity and data-loss summary; typed confirmation in the CLI | ⬜ | M3 |
| U07 | About and license dialogs | `src/stdlg.c:AboutCallback()`, `LicenseCallback()` | About panel | ⬜ | M1 |
| U08 | Settings (registry, or `rufus.ini` in portable mode) | `src/settings.h`, `src/registry.h` | XDG configuration file; never able to weaken safety checks | ⬜ | M8 |
| U09 | Accessibility (accessible control names) | `src/ui.c:SetAccessibleName()` | AccessKit | ✅ | M1 |
| U10 | Expert mode (Ctrl-Alt-E) | `src/rufus.c` (`expert_mode`) | Same | ⬜ | M6 |
| U11 | Thread priority (Alt +/-) | `src/rufus.c` (`SETTING_DEFAULT_THREAD_PRIORITY`) | Evaluate | ⬜ | M8 |
| U12 | Status bar timer | `src/rufus.c:ClockTimer()` | Same | ⬜ | M3 |

## M. Command line and cheat modes

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| C01 | `-x`, `--extra-devs` (list USB hard drives) | `src/rufus.c:WinMain()` | `ferrufus-cli devices -x`, GUI `-x` | ✅ | M1 |
| C02 | `-i PATH`, `--iso=PATH` (preselect an image) | `src/rufus.c:WinMain()` | GUI `-i/--iso`; the CLI uses `--image` | 🟡 | M1 |
| C03 | `-f FS`, `--filesystem=FS` (preferred file system) | `src/rufus.c:WinMain()` (`preselected_fs`) | Same | ⬜ | M2 |
| C04 | `-l LOCALE`, `--locale=LOCALE` | `src/rufus.c:WinMain()` | Same | ⬜ | M8 |
| C05 | `-g`, `--gui` and the `rufus.com` console hogger | `src/rufus.c:SetHogger()` | Separate `ferrufus` and `ferrufus-cli` binaries | ➖ | — |
| C06 | `-w TIMEOUT`, `--wait=TIMEOUT` (application mutex) | `src/rufus.c:WinMain()` | Single-instance handling, evaluate | ⬜ | M8 |
| C07 | Scriptable, non-interactive operation with structured output | Not in upstream | JSON output, stable exit codes, same safety checks | ✅ | M1 |

Cheat modes that map to rows above: Alt-B (H09), Alt-C (D17), Alt-E (W02), Alt-F (D05),
Alt-G (D04), Alt-H (H02), Alt-I/J/K (I18), Alt-L (F02), Alt-M (I13), Alt-N (F03), Alt-O (I21),
Alt-P (P08), Alt-Q (F12), Alt-S (I15), Alt-T (I19), Alt-Z and Ctrl-Alt-Z (F14), Alt-. (D21),
Ctrl-Alt-D (U04), Ctrl-Alt-E (U10), Ctrl-Alt-F (D07), Ctrl-P (H11), Ctrl-SELECT (I17).

## N. Windows platform behavior

| ID | Feature | Upstream reference | Linux / Rust approach | Status | Milestone |
|---|---|---|---|---|---|
| O01 | Drive letter management and volume mounting | `src/drive.c:GetDriveLetters()`, `MountVolume()`, `RemoveDriveLetters()` | Linux has no drive letters; Ferrufus only unmounts the target's file systems | ➖ | — |
| O02 | Suppress AutoMount and autorun prompts (`SetAutoMount()`, `NoDriveTypeAutorun`, Alt-X, `DisableLGP`) | `src/drive.c:SetAutoMount()`, `src/stdfn.c:SetLGP()` (renamed `SetNDTA()` after v4.15) | Desktop auto-mount is handled by holding the device exclusively; evaluate a UDisks2 inhibit | ⬜ | M3 |
| O03 | VDS (Alt-V) | `src/drive.c` (VDS functions), `src/format.c:FormatNativeVds()` | Windows-only service | ➖ | — |
| O04 | Registry cleanup (Alt-R), delete the application data folder (Alt-D) | `src/rufus.c:MainCallback()` | Windows registry; an XDG equivalent later | ➖ | — |
| O05 | Microsoft Store package and `setup.exe` helper | `res/appstore/`, `res/setup/` | Windows-only packaging | ➖ | — |
| O06 | VMware disk detection (Alt-W) | `src/rufus.c` (`SETTING_ENABLE_VMDK_DETECTION`) | Virtual disks are classified by transport | ➖ | — |
| O07 | Disable physical drive locking (Alt-,) | `src/rufus.c` (`lock_drive`) | **Not ported:** exclusive access is always required | ➖ | — |
| O08 | Elevation (runs as administrator) | `src/rufus.manifest` | The GUI never runs as root; privileged steps go through a narrow interface (see ARCHITECTURE.md) | ⬜ | M3 |

## Deliberate deviations from upstream

These differences are intentional and must not be "fixed" to match upstream.
Each one is justified in [SECURITY.md](SECURITY.md#deliberate-deviations-from-upstream).

| Upstream behavior | Reference (v4.15) | Ferrufus behavior |
|---|---|---|
| The first listed drive is selected automatically | `src/dev.c:1080-1081` | No device is ever selected automatically |
| A DD write is silently truncated to the drive size | `src/format.c:1157` (`MIN(DiskSize, image_size)`) | Writes are never truncated; oversized payloads are refused |
| The last partial sector is padded with stale buffer contents | `src/format.c:1372-1375` | Padding bytes are zeros |
| Images up to 4 KB larger than the drive pass the size check; Alt-S disables it | `src/rufus.c:1531` | Raw payloads must fit exactly; the check cannot be disabled |
| No verification after a DD write | `src/format.c:WriteDrive()` | Read-back verification is planned and on by default |
| Mounted virtual disks (VHDs) are listed by default | `src/rufus.c` (`enable_VHDs = !ReadSettingBool(SETTING_DISABLE_VHDS)`) | Loop devices are listed only on request |
| Drive locking can be disabled (Alt-,) | `src/rufus.c` | Exclusive access is always required |
