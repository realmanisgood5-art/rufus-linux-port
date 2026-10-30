// SPDX-License-Identifier: GPL-3.0-or-later
//! Which disks the running system depends on.
//!
//! Rufus removes the disk holding Windows and the disk it runs from
//! (`src/dev.c:GetDevices()`, "This is the system disk"). On Linux the
//! equivalent is every disk that backs the root file system, other system
//! directories, active swap, or the running program, through partitions,
//! device mapper (LVM, LUKS), MD RAID, loop devices, btrfs multi-device file
//! systems and overlays. A critical path whose disk cannot be determined is
//! recorded as unresolved, and the planner then refuses destructive plans.

use super::DevNum;
use super::mounts::{Mount, Swap};
use super::sysfs::SysRoot;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Mount points whose disks the system depends on, besides `/`: system
/// directories (and anything mounted below them) and the usual mount points
/// of a running live medium.
const SYSTEM_DIRS: &[&str] = &[
    "/boot",
    "/efi",
    "/etc",
    "/gnu",
    "/home",
    "/nix",
    "/opt",
    "/root",
    "/srv",
    "/sysroot",
    "/usr",
    "/var",
    "/cdrom",
    "/isodevice",
    "/lib/live",
    "/run/archiso",
    "/run/initramfs/live",
    "/run/live",
    "/run/rootfsbase",
];

/// File systems without a local backing device.
const DEVICELESS_FS: &[&str] = &[
    "autofs",
    "binfmt_misc",
    "bpf",
    "cgroup",
    "cgroup2",
    "cifs",
    "configfs",
    "debugfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "fusectl",
    "hugetlbfs",
    "mqueue",
    "nfs",
    "nfs4",
    "nsfs",
    "proc",
    "pstore",
    "ramfs",
    "rpc_pipefs",
    "securityfs",
    "smb3",
    "sysfs",
    "tmpfs",
    "tracefs",
    "virtiofs",
    "9p",
];

/// Maximum nesting followed when resolving stacked devices.
const MAX_DEPTH: u8 = 16;

/// What the running system depends on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SystemInfo {
    /// Whole disks the system depends on, with the reasons.
    pub system_disks: BTreeMap<String, Vec<String>>,
    /// Mount points of file systems on each whole disk.
    pub disk_mounts: BTreeMap<String, Vec<String>>,
    /// Critical paths whose backing disks could not be determined.
    pub unresolved: Vec<String>,
    /// Non-critical problems.
    pub warnings: Vec<String>,
}

impl SystemInfo {
    /// Analyzes mounts, swap and the running program's files.
    pub fn analyze(
        root: &SysRoot,
        mounts: &[Mount],
        swaps: &[Swap],
        program_paths: &[PathBuf],
    ) -> Self {
        let resolver = Resolver { root, mounts };
        let mut info = Self::default();

        for mount in mounts {
            let resolution = resolver.mount(mount, 0);
            for disk in &resolution.disks {
                info.disk_mounts
                    .entry(disk.clone())
                    .or_default()
                    .push(mount.mount_point.clone());
            }
            if is_critical(&mount.mount_point) {
                for disk in &resolution.disks {
                    info.add(disk, format!("backs {}", mount.mount_point));
                }
                info.unresolved.extend(resolution.unresolved);
            }
        }
        for swap in swaps {
            let resolution = if swap.kind == "partition" || swap.path.starts_with("/dev/") {
                resolver.device_path(&swap.path, 0)
            } else {
                resolver.path(Path::new(&swap.path), 0)
            };
            for disk in &resolution.disks {
                info.add(disk, format!("holds active swap ({})", swap.path));
            }
            info.unresolved.extend(
                resolution
                    .unresolved
                    .into_iter()
                    .map(|u| format!("swap {}: {u}", swap.path)),
            );
        }
        for path in program_paths {
            let resolution = resolver.path(path, 0);
            for disk in &resolution.disks {
                info.add(
                    disk,
                    format!("holds the running program ({})", path.display()),
                );
            }
            info.warnings.extend(
                resolution
                    .unresolved
                    .into_iter()
                    .map(|u| format!("could not locate {}: {u}", path.display())),
            );
        }
        info.unresolved.sort();
        info.unresolved.dedup();
        for points in info.disk_mounts.values_mut() {
            points.sort();
            points.dedup();
        }
        info
    }

    fn add(&mut self, disk: &str, reason: String) {
        let reasons = self.system_disks.entry(disk.to_string()).or_default();
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }

    /// The disks that back `path` (for example the image file a plan reads).
    pub fn disks_of_path(root: &SysRoot, mounts: &[Mount], path: &Path) -> Resolution {
        Resolver { root, mounts }.path(path, 0)
    }
}

/// Whether a mount point is one the running system needs.
pub fn is_critical(mount_point: &str) -> bool {
    mount_point == "/"
        || SYSTEM_DIRS.iter().any(|dir| {
            mount_point == *dir
                || mount_point
                    .strip_prefix(dir)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

/// Whole disks found while resolving something, and what could not be resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    /// Kernel names of whole disks.
    pub disks: BTreeSet<String>,
    /// Descriptions of what could not be resolved.
    pub unresolved: Vec<String>,
}

impl Resolution {
    fn disk(name: &str) -> Self {
        Self {
            disks: BTreeSet::from([name.to_string()]),
            unresolved: Vec::new(),
        }
    }

    fn unresolved(what: String) -> Self {
        Self {
            disks: BTreeSet::new(),
            unresolved: vec![what],
        }
    }

    fn merge(&mut self, other: Self) {
        self.disks.extend(other.disks);
        self.unresolved.extend(other.unresolved);
    }
}

struct Resolver<'a> {
    root: &'a SysRoot,
    mounts: &'a [Mount],
}

impl Resolver<'_> {
    /// Whole disks backing the block device `name`.
    fn device(&self, name: &str, depth: u8) -> Resolution {
        if depth > MAX_DEPTH {
            return Resolution::unresolved(format!("{name}: devices nested too deeply"));
        }
        let next = depth.saturating_add(1);
        let Some(dir) = self
            .root
            .canonical(&self.root.path(format!("/sys/class/block/{name}")))
        else {
            return Resolution::unresolved(format!("no block device {name}"));
        };
        if dir.join("partition").exists() {
            let disk = dir
                .parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned());
            return match disk {
                Some(disk) => self.device(&disk, next),
                None => Resolution::unresolved(format!("cannot find the disk of partition {name}")),
            };
        }
        let slaves = self.root.list(&dir.join("slaves"));
        if !slaves.is_empty() {
            let mut resolution = Resolution::default();
            for slave in slaves {
                resolution.merge(self.device(&slave, next));
            }
            return resolution;
        }
        let mut resolution = Resolution::disk(name);
        if name.starts_with("loop")
            && let Some(backing) = self.root.read_at(&dir.join("loop/backing_file"))
        {
            // "(deleted)" is appended when the backing file was removed.
            let backing = backing.trim_end_matches(" (deleted)");
            resolution.merge(self.path(Path::new(backing), next));
        }
        resolution
    }

    /// Whole disks backing a `/dev/...` path.
    fn device_path(&self, path: &str, depth: u8) -> Resolution {
        let resolved = self
            .root
            .canonical(&self.root.path(path))
            .and_then(|p| self.root.system_path(&p));
        let name = resolved
            .as_deref()
            .unwrap_or(Path::new(path))
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        match name {
            Some(name) if self.root.path(format!("/sys/class/block/{name}")).exists() => {
                self.device(&name, depth)
            }
            _ => Resolution::unresolved(format!("cannot map {path} to a block device")),
        }
    }

    fn device_of_dev(&self, dev: DevNum) -> Option<String> {
        let link = self
            .root
            .path(format!("/sys/dev/block/{}:{}", dev.major, dev.minor));
        let dir = self.root.canonical(&link)?;
        dir.file_name().map(|n| n.to_string_lossy().into_owned())
    }

    /// Whole disks backing a mounted file system.
    fn mount(&self, mount: &Mount, depth: u8) -> Resolution {
        if depth > MAX_DEPTH {
            return Resolution::unresolved(format!(
                "{}: mounts nested too deeply",
                mount.mount_point
            ));
        }
        let next = depth.saturating_add(1);
        if mount.dev.major != 0
            && let Some(name) = self.device_of_dev(mount.dev)
        {
            return self.device(&name, next);
        }
        if mount.fs_type == "overlay" {
            return self.overlay(mount, next);
        }
        if mount.source.starts_with("/dev/") {
            let mut resolution = Resolution::default();
            // bcachefs lists several devices separated by ':'.
            for part in mount.source.split(':').filter(|p| p.starts_with("/dev/")) {
                resolution.merge(self.device_path(part, next));
            }
            if mount.fs_type == "btrfs" {
                resolution.merge(self.btrfs_members(&mount.source, next));
            }
            return resolution;
        }
        if DEVICELESS_FS.contains(&mount.fs_type.as_str()) || mount.fs_type.starts_with("fuse") {
            return Resolution::default();
        }
        Resolution::unresolved(format!(
            "{} is mounted from {} ({}), which is not a block device",
            mount.mount_point, mount.source, mount.fs_type
        ))
    }

    /// All member devices of the btrfs file system that contains `source`.
    fn btrfs_members(&self, source: &str, depth: u8) -> Resolution {
        let source_name = self
            .root
            .canonical(&self.root.path(source))
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .or_else(|| {
                Path::new(source)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            });
        let mut resolution = Resolution::default();
        let base = self.root.path("/sys/fs/btrfs");
        for uuid in self.root.list(&base) {
            let members = self.root.list(&base.join(&uuid).join("devices"));
            if source_name
                .as_ref()
                .is_some_and(|name| members.contains(name))
            {
                for member in members {
                    resolution.merge(self.device(&member, depth));
                }
            }
        }
        resolution
    }

    /// Whole disks backing the layers of an overlay.
    fn overlay(&self, mount: &Mount, depth: u8) -> Resolution {
        let mut layers = Vec::new();
        for option in mount.super_options.split(',') {
            for key in ["lowerdir=", "upperdir=", "workdir="] {
                if let Some(value) = option.strip_prefix(key) {
                    layers.extend(split_unescaped_colons(value));
                }
            }
        }
        if layers.is_empty() {
            return Resolution::unresolved(format!(
                "overlay on {} has no visible layers",
                mount.mount_point
            ));
        }
        let mut resolution = Resolution::default();
        for layer in layers {
            resolution.merge(self.path(Path::new(&layer), depth));
        }
        resolution
    }

    /// Whole disks backing the file system that holds `path`.
    fn path(&self, path: &Path, depth: u8) -> Resolution {
        if depth > MAX_DEPTH {
            return Resolution::unresolved(format!("{}: nested too deeply", path.display()));
        }
        // A relative path depends on the working directory; callers must
        // make paths absolute first.
        if !path.is_absolute() {
            return Resolution::unresolved(format!("{} is not an absolute path", path.display()));
        }
        if let Some(mount) = self
            .mount_by_dev(path)
            .or_else(|| self.covering_mount(path))
        {
            return self.mount(mount, depth.saturating_add(1));
        }
        Resolution::unresolved(format!("no mount contains {}", path.display()))
    }

    /// On the running system, the mount whose device is the file's `st_dev`.
    fn mount_by_dev(&self, path: &Path) -> Option<&Mount> {
        if !self.root.is_host() {
            return None;
        }
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(path).ok()?.dev();
        let dev = DevNum::from_dev_t(dev);
        let canonical = std::fs::canonicalize(path).ok()?;
        // Among mounts of that device, the one with the longest mount point
        // containing the path (bind mounts can repeat a device).
        self.mounts
            .iter()
            .filter(|m| m.dev == dev && contains_path(&m.mount_point, &canonical))
            .max_by_key(|m| Path::new(&m.mount_point).components().count())
    }

    /// The mount with the longest mount point containing `path` (later
    /// entries win, as they are mounted on top).
    fn covering_mount(&self, path: &Path) -> Option<&Mount> {
        let path = lexical_normalize(path);
        let mut best: Option<&Mount> = None;
        for mount in self.mounts {
            if contains_path(&mount.mount_point, &path) {
                let depth = Path::new(&mount.mount_point).components().count();
                if best.is_none_or(|b| depth >= Path::new(&b.mount_point).components().count()) {
                    best = Some(mount);
                }
            }
        }
        best
    }
}

/// Whether `mount_point` contains `path` (component-wise).
fn contains_path(mount_point: &str, path: &Path) -> bool {
    path.starts_with(Path::new(mount_point))
}

/// Removes `.` and resolves `..` without touching the file system.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// Splits overlay layer lists on `:` while honoring `\:` escapes.
fn split_unescaped_colons(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            ':' => parts.push(std::mem::take(&mut current)),
            other => current.push(other),
        }
    }
    parts.push(current);
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_mount_points() {
        for point in [
            "/",
            "/boot",
            "/boot/efi",
            "/efi",
            "/usr",
            "/var/lib/docker",
            "/home",
            "/cdrom",
            "/run/live/medium",
        ] {
            assert!(is_critical(point), "{point}");
        }
        for point in [
            "/media/user/STICK",
            "/run/media/user/X",
            "/mnt/usb",
            "/data",
            "/variable",
            "/homes",
        ] {
            assert!(!is_critical(point), "{point}");
        }
    }

    #[test]
    fn overlay_layer_lists() {
        assert_eq!(
            split_unescaped_colons("/a:/b\\:c:/d"),
            vec!["/a", "/b:c", "/d"]
        );
        assert_eq!(split_unescaped_colons(""), Vec::<String>::new());
    }

    #[test]
    fn normalization() {
        assert_eq!(
            lexical_normalize(Path::new("/usr/./bin/../lib")),
            PathBuf::from("/usr/lib")
        );
        assert_eq!(lexical_normalize(Path::new("/../..")), PathBuf::from("/"));
    }
}
