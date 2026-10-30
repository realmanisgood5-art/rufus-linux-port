// SPDX-License-Identifier: GPL-3.0-or-later
//! The udev database (`/run/udev/data/b<major>:<minor>`), read directly
//! instead of through libudev.

use super::DevNum;
use super::sysfs::SysRoot;
use std::collections::BTreeMap;

/// Properties and symlinks udev recorded for a block device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UdevRecord {
    /// `E:` lines: environment properties such as `ID_FS_LABEL`.
    pub properties: BTreeMap<String, String>,
    /// `S:` lines: symlinks under `/dev`, such as `disk/by-id/...`.
    pub links: Vec<String>,
}

impl UdevRecord {
    /// Reads the record of a block device, if udev has one.
    pub fn read(root: &SysRoot, dev: DevNum) -> Option<Self> {
        let text = root.read(format!("/run/udev/data/b{}:{}", dev.major, dev.minor))?;
        Some(Self::parse(&text))
    }

    /// Parses a record.
    pub fn parse(text: &str) -> Self {
        let mut record = Self::default();
        for line in text.lines() {
            if let Some(property) = line.strip_prefix("E:") {
                if let Some((key, value)) = property.split_once('=') {
                    record.properties.insert(key.to_string(), value.to_string());
                }
            } else if let Some(link) = line.strip_prefix("S:") {
                record.links.push(link.to_string());
            }
        }
        record
    }

    /// A property, if present and not empty.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.properties
            .get(key)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    /// The first `/dev/disk/by-id/` link name.
    pub fn by_id(&self) -> Option<&str> {
        self.links
            .iter()
            .find_map(|link| link.strip_prefix("disk/by-id/"))
            .filter(|name| !name.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_properties_and_links() {
        let record = UdevRecord::parse(
            "S:disk/by-path/pci-0000:00:14.0-usb-0:1:1.0-scsi-0:0:0:0\n\
             S:disk/by-id/usb-SanDisk_Cruzer_Blade_4C530001-0:0\n\
             I:1234\n\
             E:ID_SERIAL=SanDisk_Cruzer_Blade_4C530001-0:0\n\
             E:ID_FS_LABEL=\n\
             E:ID_BUS=usb\n\
             G:systemd\n",
        );
        assert_eq!(record.get("ID_BUS"), Some("usb"));
        assert_eq!(record.get("ID_FS_LABEL"), None);
        assert_eq!(
            record.by_id(),
            Some("usb-SanDisk_Cruzer_Blade_4C530001-0:0")
        );
        assert_eq!(record.links.len(), 2);
    }
}
