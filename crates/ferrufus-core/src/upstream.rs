// SPDX-License-Identifier: GPL-3.0-or-later
//! The upstream Rufus revision whose behavior Ferrufus reproduces, and
//! references to the upstream code behind each translated behavior.

use serde::Serialize;

/// Upstream release used as the behavioral baseline.
pub const BASELINE_VERSION: &str = "4.15";

/// Git tag of the baseline release in <https://github.com/pbatard/rufus>.
pub const BASELINE_TAG: &str = "v4.15";

/// Commit of the baseline release.
pub const BASELINE_COMMIT: &str = "6d8fbf98305ff37eb531c45cbd6ff44563c53917";

/// A reference to the upstream code a behavior is translated from, written as
/// `path:Function()` relative to the Rufus source tree (for example
/// `src/dev.c:GetDevices()`), or a description when the behavior is new in
/// Ferrufus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct UpstreamRef(pub &'static str);

impl UpstreamRef {
    /// Marks a behavior that has no upstream counterpart.
    pub const FERRUFUS: Self = Self("ferrufus (no upstream counterpart)");

    /// The reference text.
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for UpstreamRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
