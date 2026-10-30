// SPDX-License-Identifier: GPL-3.0-or-later
//! Hard drive versus flash drive heuristic.
//!
//! Translated from Rufus `src/smart.c:IsHDD()` (Copyright © 2013-2023 Pete
//! Batard, GPL-3.0-or-later; tables based in part on smartmontools,
//! GPL-2.0-or-later) at the v4.15 baseline. A positive score means "hard
//! drive or SSD", which Rufus hides unless "List USB Hard Drives" is set.
//! The tables in [`super::hdd_tables`] are generated from upstream.

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::hdd_tables::{STR_ADJUST, STR_SCORE, VID_SCORE, VIDPID_SCORE};
use crate::units::GB;
use serde::Serialize;

/// The score and how it was obtained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HddScore {
    /// Total score; positive means hard drive.
    pub total: i32,
    /// Each term that contributed, in upstream order.
    pub terms: Vec<ScoreTerm>,
}

/// One term of the score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScoreTerm {
    /// What matched.
    pub reason: String,
    /// The value added.
    pub score: i32,
}

impl HddScore {
    /// Whether Rufus treats the device as a hard drive (`score > 0`).
    pub fn is_hdd(&self) -> bool {
        self.total > 0
    }
}

/// Inputs of the heuristic.
#[derive(Debug, Clone, Copy)]
pub struct HddInput<'a> {
    /// `GetDriveTypeFromIndex() == DRIVE_FIXED`: the media is not removable.
    pub fixed: bool,
    /// Drive size in bytes.
    pub size: u64,
    /// The device's friendly name, as Windows would build it.
    pub name: Option<&'a str>,
    /// USB vendor ID.
    pub vid: u16,
    /// USB product ID.
    pub pid: u16,
}

/// `IsHDD()`.
pub fn score(input: &HddInput<'_>) -> HddScore {
    let mut terms = Vec::new();
    let mut add = |reason: String, score: i32| terms.push(ScoreTerm { reason, score });

    // Boost the score if fixed, as these are generally HDDs.
    if input.fixed {
        add("fixed media".to_string(), 3);
    }

    // Adjust the score depending on the size.
    if input.size > 800 * GB {
        add("larger than 800 GB".to_string(), 15);
        if input.size > 1800 * GB {
            add("larger than 1800 GB".to_string(), 15);
        }
    } else if input.size < 128 * GB {
        add("smaller than 128 GB".to_string(), -15);
    }

    if let Some(name) = input.name {
        // Well-known HDD model prefixes. Upstream stops at the first pattern
        // longer than the name, even though the table is not sorted by length.
        let name_bytes = name.as_bytes();
        for &(pattern, score) in STR_SCORE {
            let pattern_bytes = pattern.as_bytes();
            let mlen = pattern_bytes.len();
            if mlen > name_bytes.len() {
                break;
            }
            let wildcard = pattern_bytes.last() == Some(&b'#');
            let compare_len = if wildcard {
                mlen.saturating_sub(1)
            } else {
                mlen
            };
            let prefix_matches = match (
                name_bytes.get(..compare_len),
                pattern_bytes.get(..compare_len),
            ) {
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                _ => false,
            };
            // Upstream checks the character *after* the wildcard position
            // (strid[mlen]), reproduced as is.
            let digit_matches = !wildcard || name_bytes.get(mlen).is_some_and(u8::is_ascii_digit);
            if prefix_matches && digit_matches {
                add(format!("name starts with \"{pattern}\""), score);
                break;
            }
        }

        // Adjust for oddball devices: every substring found counts.
        let lower = name.to_ascii_lowercase();
        for &(pattern, score) in STR_ADJUST {
            if lower.contains(&pattern.to_ascii_lowercase()) {
                add(format!("name contains \"{pattern}\""), score);
            }
        }
    }

    // Known VIDs, then known VID:PIDs (first match wins).
    if let Some(&(_, score)) = VID_SCORE.iter().find(|(vid, _)| *vid == input.vid) {
        add(format!("vendor {:04x}", input.vid), score);
    }
    if let Some(&(_, _, score)) = VIDPID_SCORE
        .iter()
        .find(|(vid, pid, _)| *vid == input.vid && *pid == input.pid)
    {
        add(format!("device {:04x}:{:04x}", input.vid, input.pid), score);
    }

    let total = terms
        .iter()
        .fold(0i32, |sum, term| sum.saturating_add(term.score));
    HddScore { total, terms }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)] // Fixture arithmetic.
mod tests {
    use super::*;

    fn input(fixed: bool, size: u64, name: &str, vid: u16, pid: u16) -> HddInput<'_> {
        HddInput {
            fixed,
            size,
            name: Some(name),
            vid,
            pid,
        }
    }

    #[test]
    fn typical_flash_drive() {
        // A removable 32 GB SanDisk stick: -15 (size) -15 (SANDISK) -5 (VID 0781).
        let s = score(&input(
            false,
            32 * GB,
            "SanDisk Cruzer Blade USB Device",
            0x0781,
            0x5567,
        ));
        assert_eq!(s.total, -35);
        assert!(!s.is_hdd());
    }

    #[test]
    fn typical_usb_hard_drive() {
        // A fixed 2 TB WD drive: +3 (fixed) +15 +15 (size) +10 (WDC) +10 (VID 1058).
        let s = score(&input(
            true,
            2000 * GB,
            "WDC WD20EARX-00PASB0 USB Device",
            0x1058,
            0x1140,
        ));
        assert_eq!(s.total, 53);
        assert!(s.is_hdd());
    }

    #[test]
    fn size_thresholds() {
        assert_eq!(
            score(&HddInput {
                fixed: false,
                size: 128 * GB,
                name: None,
                vid: 0,
                pid: 0
            })
            .total,
            0
        );
        assert_eq!(
            score(&HddInput {
                fixed: false,
                size: 128 * GB - 1,
                name: None,
                vid: 0,
                pid: 0
            })
            .total,
            -15
        );
        assert_eq!(
            score(&HddInput {
                fixed: false,
                size: 800 * GB,
                name: None,
                vid: 0,
                pid: 0
            })
            .total,
            0
        );
        assert_eq!(
            score(&HddInput {
                fixed: false,
                size: 800 * GB + 1,
                name: None,
                vid: 0,
                pid: 0
            })
            .total,
            15
        );
        assert_eq!(
            score(&HddInput {
                fixed: false,
                size: 1800 * GB + 1,
                name: None,
                vid: 0,
                pid: 0
            })
            .total,
            30
        );
    }

    #[test]
    fn digit_wildcard_checks_the_character_after_it() {
        let neutral = |name: &str| score(&input(false, 500 * GB, name, 0, 0)).total;
        // "ST#": compares "ST" then requires name[3] (not name[2]) to be a digit.
        assert_eq!(neutral("ST31000528AS"), 10);
        assert_eq!(neutral("ST3X"), 0);
        assert_eq!(neutral("STX1"), 10);
        // Too short: the digit position is past the end.
        assert_eq!(neutral("ST3"), 0);
        // Case-insensitive.
        assert_eq!(neutral("st31000528as"), 10);
    }

    #[test]
    fn scan_stops_at_the_first_longer_pattern() {
        // "PNY" (3 letters) comes after longer patterns such as "EXCELSTOR"
        // (9 letters); a 6-letter name stops the scan before reaching it.
        let s = score(&input(false, 500 * GB, "PNY 64", 0, 0));
        assert_eq!(s.total, 0);
        // A longer name gets there.
        let s = score(&input(false, 500 * GB, "PNY USB 3.0 FD USB Device", 0, 0));
        assert_eq!(s.total, -15);
    }

    #[test]
    fn adjustments_all_apply() {
        // "SATA" and "SSD" both add 20.
        let s = score(&input(false, 500 * GB, "Generic SATA SSD", 0, 0));
        assert_eq!(s.total, 40);
        assert_eq!(s.terms.len(), 2);
    }

    #[test]
    fn vendor_and_product_tables() {
        // Kingston VID (-5) with no product entry.
        assert_eq!(
            score(&HddInput {
                fixed: false,
                size: 500 * GB,
                name: None,
                vid: 0x0011,
                pid: 1
            })
            .total,
            -5
        );
        // Genesys VID (-5), then the first duplicate 05e3:0731 entry (+10)
        // wins over the second (+2).
        assert_eq!(
            score(&HddInput {
                fixed: false,
                size: 500 * GB,
                name: None,
                vid: 0x05e3,
                pid: 0x0731
            })
            .total,
            5
        );
        // Verbatim: VID (-2) plus the VID:PID exception (-20).
        assert_eq!(
            score(&HddInput {
                fixed: false,
                size: 500 * GB,
                name: None,
                vid: 0x18a5,
                pid: 0x0243
            })
            .total,
            -22
        );
    }
}
