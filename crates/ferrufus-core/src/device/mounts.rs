// SPDX-License-Identifier: GPL-3.0-or-later
//! Parsing of `/proc/self/mountinfo` and `/proc/swaps`.

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::DevNum;
use serde::Serialize;

/// One line of `/proc/self/mountinfo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mount {
    /// `major:minor` of the mounted file system (0 for anonymous devices,
    /// such as btrfs subvolumes and overlays).
    pub dev: DevNum,
    /// Mount point.
    pub mount_point: String,
    /// File system type.
    pub fs_type: String,
    /// Mount source (a device path for block-based file systems).
    pub source: String,
    /// Per-superblock options (for example overlay `lowerdir=`).
    pub super_options: String,
}

/// Parses `mountinfo`. Malformed lines are skipped.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines().filter_map(parse_mountinfo_line).collect()
}

fn parse_mountinfo_line(line: &str) -> Option<Mount> {
    // 36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext3 /dev/root rw,errors=continue
    let (before, after) = line.split_once(" - ")?;
    let mut fields = before.split(' ');
    let _mount_id = fields.next()?;
    let _parent_id = fields.next()?;
    let dev = DevNum::parse(fields.next()?)?;
    let _root = fields.next()?;
    let mount_point = unescape(fields.next()?);
    let mut rest = after.split(' ');
    let fs_type = unescape(rest.next()?);
    let source = unescape(rest.next().unwrap_or_default());
    let super_options = unescape(rest.next().unwrap_or_default());
    Some(Mount {
        dev,
        mount_point,
        fs_type,
        source,
        super_options,
    })
}

/// One active swap area from `/proc/swaps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Swap {
    /// Device or file path.
    pub path: String,
    /// `partition` or `file`.
    pub kind: String,
}

/// Parses `/proc/swaps` (the first line is a header).
pub fn parse_swaps(text: &str) -> Vec<Swap> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some(Swap {
                path: unescape(fields.next()?),
                kind: fields.next()?.to_string(),
            })
        })
        .collect()
}

/// Decodes the kernel's octal escapes (`\040` for a space, `\011`, `\012`,
/// `\134`). Invalid escapes are kept as written.
pub fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&byte) = bytes.get(i) {
        let octal = (byte == b'\\')
            .then(|| i.checked_add(1).zip(i.checked_add(4)))
            .flatten()
            .and_then(|(start, end)| bytes.get(start..end))
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 8).ok());
        match octal {
            Some(value) => {
                out.push(value);
                i = i.saturating_add(4);
            }
            None => {
                out.push(byte);
                i = i.saturating_add(1);
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_lines() {
        let text = "28 1 254:0 / / rw,relatime - ext4 /dev/vda rw,discard\n\
                    40 28 0:33 / /home rw shared:1 - btrfs /dev/nvme0n1p3 rw,subvol=/home\n\
                    41 28 8:17 / /media/user/MY\\040STICK rw,nosuid - vfat /dev/sdb1 rw,uid=1000\n\
                    garbage line\n";
        let mounts = parse_mountinfo(text);
        assert_eq!(mounts.len(), 3);
        assert_eq!(
            mounts[0].dev,
            DevNum {
                major: 254,
                minor: 0
            }
        );
        assert_eq!(mounts[0].mount_point, "/");
        assert_eq!(mounts[0].source, "/dev/vda");
        assert_eq!(mounts[1].dev.major, 0);
        assert_eq!(mounts[1].fs_type, "btrfs");
        assert_eq!(mounts[1].super_options, "rw,subvol=/home");
        assert_eq!(mounts[2].mount_point, "/media/user/MY STICK");
    }

    #[test]
    fn swaps() {
        let text = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
                    /dev/sda3                               partition\t8388604\t\t0\t\t-2\n\
                    /swap\\040file                          file\t\t1048572\t\t0\t\t-3\n";
        let swaps = parse_swaps(text);
        assert_eq!(swaps.len(), 2);
        assert_eq!(
            swaps[0],
            Swap {
                path: "/dev/sda3".into(),
                kind: "partition".into()
            }
        );
        assert_eq!(swaps[1].path, "/swap file");
        assert!(parse_swaps("Filename Type Size Used Priority\n").is_empty());
    }

    #[test]
    fn escapes() {
        assert_eq!(unescape(r"a\040b\011c\134d"), "a b\tc\\d");
        assert_eq!(unescape(r"trailing\04"), r"trailing\04");
        assert_eq!(unescape(r"\999"), r"\999");
        assert_eq!(unescape(""), "");
    }
}
