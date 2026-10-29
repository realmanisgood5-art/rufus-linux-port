# Notices

Ferrufus is an **unofficial** Linux port of Rufus, written in Rust. It is not
affiliated with, endorsed by, or supported by Pete Batard or the Rufus project.

Ferrufus is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. See [LICENSE](LICENSE).

## Rufus

Rufus: The Reliable USB Formatting Utility
Copyright © 2011-2026 Pete Batard <pete@akeo.ie> and the Rufus contributors.
https://rufus.ie — https://github.com/pbatard/rufus
Licensed under the GNU General Public License, version 3 or later.

Ferrufus reproduces the behavior of Rufus v4.15
(commit `6d8fbf98305ff37eb531c45cbd6ff44563c53917`). Rust code translated from
Rufus names its upstream file and function in its header and comments.

## Translated components

The following upstream code has been translated into Ferrufus so far. Each
translated file repeats the applicable notice.

| Ferrufus code | Upstream source | Copyright | License |
|---|---|---|---|
| Device listing policy | Rufus `src/dev.c:GetDevices()` | © 2014-2026 Pete Batard | GPL-3.0-or-later |
| HDD versus flash drive heuristic and tables | Rufus `src/smart.c:IsHDD()`, `src/hdd_vs_ufd.h`; based in part on smartmontools `drivedb.h`, the Linux USB ID list and the flashboot.ru iflash database | © 2013-2023 Pete Batard; © 2003-11 Philip Williams, Bruce Allen; © 2008-13 Christian Franke | GPL-2.0-or-later (smartmontools), GPL-3.0-or-later (Rufus) |
| MBR boot code identification and tables | Rufus `src/drive.c:AnalyzeMBR()`; ms-sys `src/ms-sys/br.c`, `src/ms-sys/inc/mbr_*.h` | © 2011-2026 Pete Batard; © 2009-2015 Henrik Carlqvist | GPL-2.0-or-later (ms-sys), GPL-3.0-or-later (Rufus) |
| ISO 9660 volume label rule | libcdio `src/libcdio/iso9660/iso9660_fs.c:get_member_id()` as shipped with Rufus | © 2003-2024 Rocky Bernstein; © 2018-2024 Pete Batard; © 2018 Thomas Schmitt; © 2001 Herbert Valerio Riedel | GPL-3.0-or-later |
| Image classification | Rufus `src/vhd.c:IsBootableImage()`, `IsCompressedBootableImage()`; `src/iso.c:ExtractISO()`; `src/rufus.c:ImageScanThread()` | © 2011-2026 Pete Batard; © 2003-2014 Rocky Bernstein (iso.c) | GPL-3.0-or-later |
| Boot checks and DD stage sequence | Rufus `src/rufus.c:BootCheckThread()`, `src/format.c:FormatThread()`, `WriteDrive()`, `src/drive.c:IsSourceImageLocatedOnTargetDrive()` | © 2011-2026 Pete Batard | GPL-3.0-or-later |
| Human-readable sizes | Rufus `src/stdio.c:SizeToHumanReadable()` | © 2011-2026 Pete Batard | GPL-3.0-or-later |

The ms-sys tables contain boot code bytes from DOS, Windows, GRUB, Grub4DOS,
Syslinux, ReactOS, KolibriOS and Rufus master boot records. Ferrufus uses them,
as ms-sys and Rufus do, to recognize and (in later milestones) write boot
records.

## Branding

The Ferrufus name and icon are distinct from Rufus'. Rufus' icons are not used.
