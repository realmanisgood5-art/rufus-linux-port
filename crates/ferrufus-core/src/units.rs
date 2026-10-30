// SPDX-License-Identifier: GPL-3.0-or-later
//! Size constants and human-readable sizes.
//!
//! `size_to_human_readable()` is translated from Rufus
//! `src/stdio.c:SizeToHumanReadable()` (Copyright © 2011-2026 Pete Batard,
//! GPL-3.0-or-later) at the v4.15 baseline, including its rounding rules, so
//! that sizes read the same as in Rufus.

/// 1024 bytes.
pub const KB: u64 = 1024;
/// 1024 KB.
pub const MB: u64 = 1024 * KB;
/// 1024 MB.
pub const GB: u64 = 1024 * MB;
/// 1024 GB.
pub const TB: u64 = 1024 * GB;
/// 1024 TB.
pub const PB: u64 = 1024 * TB;

/// Unit conventions, as selected in Rufus by the "use proper size units"
/// toggle (Alt-U, `SETTING_USE_PROPER_SIZE_UNITS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeUnits {
    /// Rufus' default ("fake units"): powers of 1000, and sizes of 8 units or
    /// more within 5% of a power of two are shown as that power of two, so a
    /// drive sold as 16 GB reads "16 GB".
    #[default]
    Fake,
    /// Powers of 1024.
    Proper,
}

/// Unit names (`MSG_020` to `MSG_025` in Rufus' English localization).
const SUFFIXES: [&str; 6] = ["bytes", "KB", "MB", "GB", "TB", "PB"];

/// Formats a size the way Rufus does.
///
/// Note the upstream quirk, kept on purpose: with [`SizeUnits::Fake`], values
/// below 8 units have no space before the unit ("7.5GB"), while all other
/// forms do ("16 GB", "28.9 GB", "512 bytes").
pub fn size_to_human_readable(size: u64, units: SizeUnits) -> String {
    let fake = units == SizeUnits::Fake;
    let divider = if fake { 1000.0 } else { 1024.0 };
    let mut hr_size = u64_to_f64(size);
    let mut suffix = 0;
    while suffix < SUFFIXES.len() - 1 && hr_size >= divider {
        hr_size /= divider;
        suffix += 1;
    }
    let unit = SUFFIXES.get(suffix).copied().unwrap_or("PB");

    if suffix == 0 {
        format!("{} {unit}", truncate_to_u16(hr_size))
    } else if fake {
        if hr_size < 8.0 {
            if ((hr_size * 10.0) - ((hr_size + 0.5).floor() * 10.0)).abs() < 0.5 {
                format!("{hr_size:.0}{unit}")
            } else {
                format!("{hr_size:.1}{unit}")
            }
        } else {
            let t = f64::from(upper_power_of_two(truncate_to_u16(hr_size)));
            // Upstream compares against the `float` constant 0.05f.
            let i_size = if (1.0 - hr_size / t).abs() < f64::from(0.05_f32) {
                truncate_to_u16(t)
            } else {
                truncate_to_u16(hr_size)
            };
            format!("{i_size} {unit}")
        }
    } else if hr_size * 10.0 - hr_size.floor() * 10.0 < 0.5 {
        format!("{hr_size:.0} {unit}")
    } else {
        format!("{hr_size:.1} {unit}")
    }
}

/// `upo2()` from `src/stdio.c`: the smallest power of two greater than or
/// equal to `v`, with the same 16-bit wrapping arithmetic.
fn upper_power_of_two(v: u16) -> u16 {
    let mut v = v.wrapping_sub(1);
    v |= v >> 1;
    v |= v >> 2;
    v |= v >> 4;
    v |= v >> 8;
    v.wrapping_add(1)
}

/// C's `(uint16_t)value` conversion for the non-negative values below 1024
/// used here. Rust's `as` truncates toward zero, like C, and saturates instead
/// of being undefined outside the range.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn truncate_to_u16(value: f64) -> u16 {
    value as u16
}

/// C's `(double)size` conversion. Sizes above 2^53 lose precision exactly as
/// they do upstream.
#[allow(clippy::cast_precision_loss)]
fn u64_to_f64(size: u64) -> f64 {
    size as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_shown_as_integers() {
        assert_eq!(size_to_human_readable(0, SizeUnits::Fake), "0 bytes");
        assert_eq!(size_to_human_readable(512, SizeUnits::Proper), "512 bytes");
        assert_eq!(size_to_human_readable(999, SizeUnits::Fake), "999 bytes");
        assert_eq!(
            size_to_human_readable(1023, SizeUnits::Proper),
            "1023 bytes"
        );
    }

    #[test]
    fn proper_units_use_powers_of_1024() {
        assert_eq!(size_to_human_readable(KB, SizeUnits::Proper), "1 KB");
        assert_eq!(size_to_human_readable(8 * MB, SizeUnits::Proper), "8 MB");
        assert_eq!(
            size_to_human_readable(3 * GB / 2, SizeUnits::Proper),
            "1.5 GB"
        );
        assert_eq!(size_to_human_readable(2 * TB, SizeUnits::Proper), "2 TB");
        // A "32 GB" stick: 31,004,295,168 bytes is 28.875 GiB.
        assert_eq!(
            size_to_human_readable(31_004_295_168, SizeUnits::Proper),
            "28.9 GB"
        );
    }

    #[test]
    fn proper_units_round_like_upstream() {
        // The fraction is below 0.05: printed with "%0.0f".
        assert_eq!(
            size_to_human_readable(28 * GB + 40 * MB, SizeUnits::Proper),
            "28 GB"
        );
        // The fraction is 0.97: printed with "%0.1f", which rounds up.
        assert_eq!(
            size_to_human_readable(28 * GB + 993 * MB, SizeUnits::Proper),
            "29.0 GB"
        );
    }

    #[test]
    fn fake_units_snap_to_powers_of_two() {
        // 31.0 GB (decimal) is within 5% of 32.
        assert_eq!(
            size_to_human_readable(31_004_295_168, SizeUnits::Fake),
            "32 GB"
        );
        // 15.5 GB is within 5% of 16.
        assert_eq!(
            size_to_human_readable(15_500_000_000, SizeUnits::Fake),
            "16 GB"
        );
        // 12 GB is not within 5% of 16.
        assert_eq!(
            size_to_human_readable(12_000_000_000, SizeUnits::Fake),
            "12 GB"
        );
        // 500 GB is within 5% of 512 (500 / 512 = 0.977).
        assert_eq!(
            size_to_human_readable(500_000_000_000, SizeUnits::Fake),
            "512 GB"
        );
        assert_eq!(
            size_to_human_readable(1_000_000_000_000, SizeUnits::Fake),
            "1TB"
        );
    }

    #[test]
    fn fake_units_below_eight_have_no_space() {
        assert_eq!(
            size_to_human_readable(4_000_000_000, SizeUnits::Fake),
            "4GB"
        );
        assert_eq!(
            size_to_human_readable(7_500_000_000, SizeUnits::Fake),
            "7.5GB"
        );
        assert_eq!(size_to_human_readable(1_000, SizeUnits::Fake), "1KB");
    }

    #[test]
    fn upper_power_of_two_matches_upstream() {
        assert_eq!(upper_power_of_two(8), 8);
        assert_eq!(upper_power_of_two(9), 16);
        assert_eq!(upper_power_of_two(31), 32);
        assert_eq!(upper_power_of_two(512), 512);
        assert_eq!(upper_power_of_two(999), 1024);
        // 16-bit wrapping, as in C.
        assert_eq!(upper_power_of_two(0), 0);
        assert_eq!(upper_power_of_two(40_000), 0);
    }

    #[test]
    fn never_panics() {
        for size in [u64::MAX, u64::MAX - 1, 1 << 63, PB, 1023 * PB] {
            let _ = size_to_human_readable(size, SizeUnits::Fake);
            let _ = size_to_human_readable(size, SizeUnits::Proper);
        }
    }
}
