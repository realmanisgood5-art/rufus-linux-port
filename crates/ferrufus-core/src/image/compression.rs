// SPDX-License-Identifier: GPL-3.0-or-later
//! Image container types, chosen from the file name extension as Rufus does.
//!
//! Translated from `file_assoc[]` and the extension lookup in Rufus
//! `src/vhd.c:IsCompressedBootableImage()` (Copyright © 2013-2026 Pete Batard,
//! GPL-3.0-or-later), v4.15. Rufus decides how to read an image only from its
//! extension; Ferrufus additionally checks the file's magic bytes and reports
//! a mismatch, which the planner treats as blocking.

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::io::slice;
use serde::Serialize;

/// Container types Rufus associates with file extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Association {
    /// `.zip` (`BLED_COMPRESSION_ZIP`).
    Zip,
    /// `.Z` (`BLED_COMPRESSION_LZW`).
    Lzw,
    /// `.gz` (`BLED_COMPRESSION_GZIP`).
    Gzip,
    /// `.lzma` (`BLED_COMPRESSION_LZMA`).
    Lzma,
    /// `.bz2` (`BLED_COMPRESSION_BZIP2`).
    Bzip2,
    /// `.xz` (`BLED_COMPRESSION_XZ`).
    Xz,
    /// `.vtsi`, Ventoy sparse image (`BLED_COMPRESSION_VTSI`).
    Vtsi,
    /// `.zst` (`BLED_COMPRESSION_ZSTD`).
    Zstd,
    /// `.ffu`, Full Flash Update image (`IMG_COMPRESSION_FFU`).
    Ffu,
    /// `.vhd` (`IMG_COMPRESSION_VHD`).
    Vhd,
    /// `.vhdx` (`IMG_COMPRESSION_VHDX`).
    Vhdx,
}

/// `file_assoc[]`, in upstream order.
const FILE_ASSOC: [(&str, Association); 11] = [
    (".zip", Association::Zip),
    (".Z", Association::Lzw),
    (".gz", Association::Gzip),
    (".lzma", Association::Lzma),
    (".bz2", Association::Bzip2),
    (".xz", Association::Xz),
    (".vtsi", Association::Vtsi),
    (".zst", Association::Zstd),
    (".ffu", Association::Ffu),
    (".vhd", Association::Vhd),
    (".vhdx", Association::Vhdx),
];

impl Association {
    /// Whether this is one of bled's compression formats (as opposed to a
    /// virtual disk container).
    pub fn is_compression(self) -> bool {
        !matches!(self, Self::Ffu | Self::Vhd | Self::Vhdx)
    }

    /// The extension, as listed in `file_assoc[]`.
    pub fn extension(self) -> &'static str {
        FILE_ASSOC
            .iter()
            .find(|(_, assoc)| *assoc == self)
            .map_or("", |(ext, _)| ext)
    }
}

/// The association of `path`, using upstream's rule: the extension starts at
/// the last `.` anywhere in the path (paths of four characters or fewer have
/// none) and is compared without regard to ASCII case.
pub fn associate(path: &str) -> Option<Association> {
    if path.len() <= 4 {
        return None;
    }
    let extension = path.rfind('.').and_then(|dot| path.get(dot..))?;
    FILE_ASSOC
        .iter()
        .find(|(ext, _)| ext.eq_ignore_ascii_case(extension))
        .map(|(_, assoc)| *assoc)
}

/// Result of checking a file's first bytes against its association.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MagicCheck {
    /// The magic bytes match the association.
    Match,
    /// The magic bytes contradict the association.
    Mismatch,
    /// The format has no magic number at the start of the file; the header
    /// looks plausible (LZMA) or the signature is elsewhere (VHD, VTSI).
    NotApplicable,
}

/// Checks the first bytes of a file against its association.
pub fn check_magic(association: Association, first_bytes: &[u8]) -> MagicCheck {
    let starts = |magic: &[u8]| slice(first_bytes, 0, magic.len()) == Some(magic);
    let matches = match association {
        // Local file header, or the end of central directory of an empty archive.
        Association::Zip => starts(b"PK\x03\x04") || starts(b"PK\x05\x06"),
        Association::Lzw => starts(&[0x1f, 0x9d]),
        Association::Gzip => starts(&[0x1f, 0x8b]),
        // .lzma ("LZMA alone") has no magic: check that the properties byte
        // (lc + lp * 9 + pb * 45) is valid.
        Association::Lzma => first_bytes
            .first()
            .is_some_and(|&properties| properties < 225),
        Association::Bzip2 => {
            starts(b"BZh")
                && first_bytes
                    .get(3)
                    .is_some_and(|b| (b'1'..=b'9').contains(b))
        }
        Association::Xz => starts(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]),
        Association::Zstd => {
            starts(&[0x28, 0xb5, 0x2f, 0xfd])
                // A skippable frame: 0x184D2A50 to 0x184D2A5F, little-endian.
                || (slice(first_bytes, 1, 3) == Some([0x2a, 0x4d, 0x18].as_slice())
                    && first_bytes.first().is_some_and(|b| b & 0xf0 == 0x50))
        }
        Association::Ffu => super::vhd::is_ffu(first_bytes),
        Association::Vhdx => super::vhd::is_vhdx(first_bytes),
        Association::Vhd | Association::Vtsi => return MagicCheck::NotApplicable,
    };
    match (matches, association) {
        (true, Association::Lzma) => MagicCheck::NotApplicable,
        (true, _) => MagicCheck::Match,
        (false, _) => MagicCheck::Mismatch,
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // Fixture arithmetic.
mod tests {
    use super::*;

    #[test]
    fn extensions_follow_upstream_rules() {
        assert_eq!(associate("/home/u/debian.img.xz"), Some(Association::Xz));
        assert_eq!(associate("/home/u/Image.GZ"), Some(Association::Gzip));
        assert_eq!(associate("/home/u/archive.z"), Some(Association::Lzw));
        assert_eq!(associate("/home/u/disk.VHDX"), Some(Association::Vhdx));
        assert_eq!(associate("/home/u/disk.vhd"), Some(Association::Vhd));
        assert_eq!(associate("/home/u/win.ffu"), Some(Association::Ffu));
        assert_eq!(associate("/home/u/ubuntu.iso"), None);
        assert_eq!(associate("/home/u/raw-image"), None);
        // The last dot of the whole path counts, even in a directory name.
        assert_eq!(associate("/media/v1.gz/image"), None);
        // Paths of four characters or fewer have no extension.
        assert_eq!(associate(".gz"), None);
        assert_eq!(associate("a.gz"), None);
        assert_eq!(associate("ab.gz"), Some(Association::Gzip));
    }

    #[test]
    fn magic_numbers() {
        assert_eq!(
            check_magic(Association::Gzip, &[0x1f, 0x8b, 8]),
            MagicCheck::Match
        );
        assert_eq!(check_magic(Association::Gzip, b"MZ"), MagicCheck::Mismatch);
        assert_eq!(
            check_magic(Association::Xz, &[0xfd, b'7', b'z', b'X', b'Z', 0]),
            MagicCheck::Match
        );
        assert_eq!(check_magic(Association::Bzip2, b"BZh9"), MagicCheck::Match);
        assert_eq!(
            check_magic(Association::Bzip2, b"BZh0"),
            MagicCheck::Mismatch
        );
        assert_eq!(
            check_magic(Association::Zstd, &[0x28, 0xb5, 0x2f, 0xfd]),
            MagicCheck::Match
        );
        assert_eq!(
            check_magic(Association::Zstd, &[0x5e, 0x2a, 0x4d, 0x18]),
            MagicCheck::Match
        );
        assert_eq!(
            check_magic(Association::Zip, b"PK\x03\x04"),
            MagicCheck::Match
        );
        assert_eq!(
            check_magic(Association::Lzw, &[0x1f, 0x9d]),
            MagicCheck::Match
        );
        assert_eq!(
            check_magic(Association::Lzma, &[0x5d, 0, 0]),
            MagicCheck::NotApplicable
        );
        assert_eq!(
            check_magic(Association::Lzma, &[0xff]),
            MagicCheck::Mismatch
        );
        assert_eq!(
            check_magic(Association::Vhd, &[]),
            MagicCheck::NotApplicable
        );
        assert_eq!(
            check_magic(Association::Vhdx, b"vhdxfile"),
            MagicCheck::Match
        );
        assert_eq!(check_magic(Association::Gzip, &[]), MagicCheck::Mismatch);
    }

    #[test]
    fn compression_kinds() {
        assert!(Association::Xz.is_compression());
        assert!(!Association::Vhd.is_compression());
        assert_eq!(Association::Lzw.extension(), ".Z");
    }
}
