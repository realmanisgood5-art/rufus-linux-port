//! The ISO 9660/Joliet/Rock Ridge/UDF reader on synthetic images.

// Test code: a failed expectation should panic.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ferrufus_core::image::isofs::{Entry, Extensions, Iso, Udf, Volume, translate_name};
use ferrufus_testkit::images::BootEntry;
use ferrufus_testkit::isofs::{IsoFs, Node};
use proptest::prelude::*;

const GIB: u64 = 1024 * 1024 * 1024;

fn names(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|e| e.name.as_str()).collect()
}

fn find<'a>(entries: &'a [Entry], name: &str) -> &'a Entry {
    entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("{name} not in {:?}", names(entries)))
}

fn plain() -> Vec<u8> {
    let mut fs = IsoFs::new(
        "PLAIN",
        vec![
            Node::dir("BOOT", vec![Node::file("GRUB.CFG", b"set default=0\n")]),
            Node::file("README.TXT", b"hello"),
        ],
    );
    fs.joliet = Some("Plain".into());
    fs.build()
}

fn rock() -> Vec<u8> {
    let mut fs = IsoFs::new(
        "ROCK",
        vec![
            Node::dir(
                "boot",
                vec![Node::dir(
                    "grub",
                    vec![
                        Node::file("grub.cfg", b"set timeout=5\n"),
                        Node::file(&"x".repeat(150), b"long"),
                    ],
                )],
            ),
            Node::dir(
                "EFI",
                vec![Node::dir(
                    "BOOT",
                    vec![Node::file("BOOTX64.EFI", &[0x4d; 5000])],
                )],
            ),
            Node::symlink("live", "casper"),
            Node::symlink("abs", "/usr/../lib/./x"),
            Node::sized("big.wim", 5 * GIB),
        ],
    );
    fs.rock_ridge = true;
    fs.joliet = Some("Rock".into());
    fs.boot = vec![
        BootEntry {
            platform: 0,
            lsn: 20,
            sectors: 4,
        },
        BootEntry {
            platform: 0xef,
            lsn: 21,
            sectors: 1,
        },
    ];
    fs.build()
}

#[test]
fn plain_iso_names_are_raw_identifiers() {
    let image = plain();
    let iso = Iso::open(&image, Extensions::for_scan(true, true))
        .unwrap()
        .unwrap();
    assert_eq!(
        iso.joliet_level(),
        0,
        "the scan disables Joliet when Rock Ridge is enabled"
    );
    let root = iso.read_root().unwrap().unwrap();
    assert_eq!(names(&root), [".", "..", "BOOT", "README.TXT;1"]);
    assert!(root.iter().all(|e| !e.rock));
    let readme = find(&root, "README.TXT;1");
    assert_eq!(readme.size, 5);
    assert_eq!(translate_name(&readme.name, 0), "readme.txt");
    let boot = iso.read_dir(find(&root, "BOOT")).unwrap().unwrap();
    assert_eq!(names(&boot), [".", "..", "GRUB.CFG;1"]);
}

#[test]
fn name_translation_follows_libcdio() {
    assert_eq!(translate_name("ISOLINUX.BIN;1", 0), "isolinux.bin");
    assert_eq!(translate_name("README.;1", 0), "readme");
    assert_eq!(translate_name("A;B;1", 0), "a.b");
    assert_eq!(translate_name("MixedCase;1", 1), "MixedCase");
    assert_eq!(translate_name("NOVERSION", 0), "noversion");
}

#[test]
fn joliet_is_used_with_all_extensions() {
    let image = plain();
    let iso = Iso::open(&image, Extensions::ALL).unwrap().unwrap();
    assert_eq!(iso.joliet_level(), 3);
    let root = iso.read_root().unwrap().unwrap();
    assert_eq!(names(&root), [".", "..", "BOOT", "README.TXT"]);
}

#[test]
fn rock_ridge_names_symlinks_and_multi_extent_files() {
    let image = rock();
    let iso = Iso::open(&image, Extensions::for_scan(true, true))
        .unwrap()
        .unwrap();
    let root = iso.read_root().unwrap().unwrap();
    // The virtual El Torito directory comes first.
    assert_eq!(
        names(&root),
        ["[BOOT]", ".", "..", "boot", "EFI", "live", "abs", "big.wim"]
    );
    assert!(find(&root, "boot").rock && find(&root, "boot").is_dir);
    assert_eq!(find(&root, "live").symlink.as_deref(), Some("casper"));
    assert_eq!(
        find(&root, "abs").symlink.as_deref(),
        Some("/usr/../lib/./x")
    );
    let big = find(&root, "big.wim");
    assert_eq!(big.size, 5 * GIB);
    assert!(!big.is_dir);

    let boot = iso.read_dir(find(&root, "boot")).unwrap().unwrap();
    let grub = iso.read_dir(find(&boot, "grub")).unwrap().unwrap();
    // The 150-character name is stored through a continuation area.
    assert_eq!(names(&grub), [".", "..", "grub.cfg", &"x".repeat(150)]);

    let images = iso.read_dir(&root[0]).unwrap().unwrap();
    assert_eq!(names(&images), ["0-Boot-NoEmul.img", "1-Boot-NoEmul.img"]);
    assert_eq!(images[0].size, 4 * 512);
    // A one-sector entry is only expanded when 8 MB or more follow it.
    assert_eq!(images[1].size, 512);
}

#[test]
fn files_are_read_like_read_iso_file_to_buffer() {
    let image = rock();
    // All extensions: the Joliet tree, exact names.
    let read = |path| Volume::read_file(&image, path, 1024 * 1024).unwrap();
    assert_eq!(
        read("/boot/grub/grub.cfg").as_deref(),
        Some(b"set timeout=5\n".as_slice())
    );
    assert_eq!(read("/EFI/BOOT/BOOTX64.EFI").map(|d| d.len()), Some(5000));
    assert_eq!(
        read("/efi/boot/bootx64.efi"),
        None,
        "lookups are case-sensitive"
    );
    assert_eq!(read("/missing"), None);
    assert_eq!(
        read("/[BOOT]/0-Boot-NoEmul.img").map(|d| d.len()),
        Some(2048)
    );
    // Files over the limit are not read.
    assert_eq!(
        Volume::read_file(&image, "/big.wim", u64::MAX).unwrap(),
        None
    );

    // Without Joliet, plain names match their translation.
    let mut fs = IsoFs::new(
        "P",
        vec![Node::dir("BOOT", vec![Node::file("GRUB.CFG", b"cfg")])],
    );
    fs.rock_ridge = false;
    let image = fs.build();
    assert_eq!(
        Volume::read_file(&image, "/boot/grub.cfg", 64)
            .unwrap()
            .as_deref(),
        Some(b"cfg".as_slice())
    );
    assert_eq!(
        Volume::read_file(&image, "/BOOT/GRUB.CFG;1", 64)
            .unwrap()
            .as_deref(),
        Some(b"cfg".as_slice())
    );
}

#[test]
fn deep_directories_are_flagged() {
    let mut fs = IsoFs::new(
        "DEEP",
        vec![Node::DeepDir {
            name: "rr_moved".into(),
        }],
    );
    fs.rock_ridge = true;
    let image = fs.build();
    let iso = Iso::open(&image, Extensions::for_scan(true, true))
        .unwrap()
        .unwrap();
    let root = iso.read_root().unwrap().unwrap();
    assert!(find(&root, "rr_moved").is_deep_dir_link());
}

#[test]
fn udf_is_preferred_and_read() {
    let mut fs = IsoFs::new(
        "BRIDGE",
        vec![
            Node::dir(
                "sources",
                vec![
                    Node::file("boot.wim", b"wim data"),
                    Node::sized("install.wim", 5 * GIB),
                ],
            ),
            Node::file("bootmgr", b"bm"),
            Node::symlink("link", "bootmgr"),
            Node::file("unicodé.txt", b"u"),
        ],
    );
    fs.udf = Some("CCCOMA_X64FRE".into());
    let image = fs.build();
    let Some(Volume::Udf(udf)) = Volume::open(&image, Extensions::ALL).unwrap() else {
        panic!("UDF expected")
    };
    let root = udf.read_root().unwrap().unwrap();
    // The parent link has an empty name; Rufus skips it.
    assert_eq!(
        names(&root),
        ["", "sources", "bootmgr", "link", "unicodé.txt"]
    );
    assert!(find(&root, "sources").is_dir);
    assert_eq!(find(&root, "link").symlink.as_deref(), Some(""));
    let sources = udf.read_dir(find(&root, "sources")).unwrap().unwrap();
    assert_eq!(find(&sources, "install.wim").size, 5 * GIB);
    assert_eq!(
        Volume::read_file(&image, "/sources/boot.wim", 64)
            .unwrap()
            .as_deref(),
        Some(b"wim data".as_slice())
    );
    assert_eq!(
        Volume::read_file(&image, "sources/boot.wim", 64)
            .unwrap()
            .as_deref(),
        Some(b"wim data".as_slice())
    );
    // A file is not a directory.
    assert_eq!(udf.read_dir(find(&root, "bootmgr")).unwrap(), None);
}

#[test]
fn non_iso_data_is_rejected() {
    let zeros = vec![0u8; 64 * 2048];
    assert!(Volume::open(&zeros, Extensions::ALL).unwrap().is_none());
    assert!(Udf::open(&zeros).unwrap().is_none());
    assert_eq!(Volume::read_file(&zeros, "/a", 64).unwrap(), None);
}

/// Lists everything reachable, as a scan would, with a depth limit.
fn walk(volume: &Volume<'_>, entries: &[Entry], depth: usize, budget: &mut usize) {
    for entry in entries {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        if entry.is_dir
            && entry.name != "."
            && entry.name != ".."
            && !entry.name.is_empty()
            && depth < 16
            && let Some(children) = volume.read_dir(entry).unwrap()
        {
            walk(volume, &children, depth + 1, budget);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// Corrupted images never make the reader panic or read without bound.
    #[test]
    fn corrupted_images_are_handled(mutations in prop::collection::vec((any::<usize>(), any::<u8>()), 1..64), udf in any::<bool>()) {
        let mut image = if udf {
            let mut fs = IsoFs::new("U", vec![Node::dir("a", vec![Node::file("b", b"c")])]);
            fs.udf = Some("U".into());
            fs.build()
        } else {
            rock()
        };
        let len = image.len();
        // Aim most mutations at the metadata area.
        for (offset, value) in mutations {
            let limit = if offset % 4 == 0 { len } else { (40 * 2048).min(len) };
            image[offset % limit] = value;
        }
        for extensions in [Extensions::ALL, Extensions::for_scan(true, true)] {
            if let Ok(Some(volume)) = Volume::open(&image, extensions)
                && let Ok(Some(root)) = volume.read_root()
            {
                let mut budget = 10_000;
                walk(&volume, &root, 0, &mut budget);
            }
        }
        let _ = Volume::read_file(&image, "/boot/grub/grub.cfg", 1024 * 1024);
    }
}
