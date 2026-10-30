// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only access to sysfs, procfs and the udev database under a root
//! directory, so that tests can use a fake system tree.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Largest attribute file read (sysfs attributes are small; this bounds
/// procfs and udev files too).
const MAX_ATTRIBUTE: u64 = 1024 * 1024;

/// The directory that contains `sys/`, `proc/`, `dev/` and `run/udev/data/`:
/// `/` on a real system.
#[derive(Debug, Clone)]
pub struct SysRoot {
    root: PathBuf,
}

impl Default for SysRoot {
    fn default() -> Self {
        Self::host()
    }
}

impl SysRoot {
    /// The running system.
    pub fn host() -> Self {
        Self {
            root: PathBuf::from("/"),
        }
    }

    /// A system tree rooted at `root` (for tests).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            root: fs::canonicalize(&root).unwrap_or(root),
        }
    }

    /// Whether this is the running system.
    pub fn is_host(&self) -> bool {
        self.root == Path::new("/")
    }

    /// `path` (absolute, as on a real system) inside this root.
    pub fn path(&self, path: impl AsRef<Path>) -> PathBuf {
        let path = path.as_ref();
        self.root.join(path.strip_prefix("/").unwrap_or(path))
    }

    /// The system path of `path` inside this root (the inverse of [`Self::path`]).
    pub fn system_path(&self, path: &Path) -> Option<PathBuf> {
        let relative = path.strip_prefix(&self.root).ok()?;
        Some(Path::new("/").join(relative))
    }

    /// Reads a text attribute, without its trailing newline. `None` if it
    /// does not exist or cannot be read.
    pub fn read(&self, path: impl AsRef<Path>) -> Option<String> {
        read_limited(&self.path(path))
    }

    /// Reads an attribute at an already-resolved path inside this root.
    pub fn read_at(&self, path: &Path) -> Option<String> {
        read_limited(path)
    }

    /// Reads a decimal attribute.
    pub fn read_u64(&self, path: &Path) -> Option<u64> {
        read_limited(path)?.trim().parse().ok()
    }

    /// Names of the entries of a directory, sorted. Empty if it does not exist.
    pub fn list(&self, path: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(path)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// The canonical form of a path inside this root (resolving symlinks).
    pub fn canonical(&self, path: &Path) -> Option<PathBuf> {
        let canonical = fs::canonicalize(path).ok()?;
        canonical.starts_with(&self.root).then_some(canonical)
    }

    /// The file name a symlink points to (for example a `driver` link).
    pub fn link_name(&self, path: &Path) -> Option<String> {
        fs::read_link(path)
            .ok()?
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }
}

fn read_limited(path: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(MAX_ATTRIBUTE)
        .read_to_string(&mut text)
        .ok()?;
    if text.ends_with('\n') {
        text.pop();
    }
    Some(text)
}
