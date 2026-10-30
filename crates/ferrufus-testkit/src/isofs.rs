// SPDX-License-Identifier: GPL-3.0-or-later
//! ISO 9660 images with real directory trees: plain ISO 9660 names, Rock
//! Ridge (`SP`, `PX`, `NM`, `SL`, `CE`, `PL`), Joliet, El Torito and
//! multi-extent files, plus a UDF file system for bridge discs.
//!
//! Large files can be declared with a size bigger than their data: their
//! extents then point past the end of the image, which is enough for code
//! that only lists directories.

use crate::images::{BLOCK, BootEntry};

/// A file system node.
#[derive(Debug, Clone)]
pub enum Node {
    /// A regular file.
    File {
        /// Name (the Rock Ridge/Joliet/UDF name; see [`IsoFs::rock_ridge`]).
        name: String,
        /// Contents written to the image.
        data: Vec<u8>,
        /// Size recorded in the directory (at least `data.len()`).
        size: u64,
    },
    /// A directory.
    Dir {
        /// Name.
        name: String,
        /// Entries.
        children: Vec<Node>,
    },
    /// A symbolic link (Rock Ridge `SL`, UDF file type 12).
    Symlink {
        /// Name.
        name: String,
        /// Target.
        target: String,
    },
    /// A Rock Ridge deep directory placeholder: a directory record carrying
    /// a `PL` entry, as found in relocated directories.
    DeepDir {
        /// Name.
        name: String,
    },
}

impl Node {
    /// A file with the given contents.
    pub fn file(name: &str, data: &[u8]) -> Self {
        Self::File {
            name: name.to_string(),
            data: data.to_vec(),
            size: data.len() as u64,
        }
    }

    /// A file whose recorded size is `size` but whose data is not stored.
    pub fn sized(name: &str, size: u64) -> Self {
        Self::File {
            name: name.to_string(),
            data: Vec::new(),
            size,
        }
    }

    /// A directory.
    pub fn dir(name: &str, children: Vec<Node>) -> Self {
        Self::Dir {
            name: name.to_string(),
            children,
        }
    }

    /// A symbolic link.
    pub fn symlink(name: &str, target: &str) -> Self {
        Self::Symlink {
            name: name.to_string(),
            target: target.to_string(),
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::File { name, .. }
            | Self::Dir { name, .. }
            | Self::Symlink { name, .. }
            | Self::DeepDir { name } => name,
        }
    }

    fn is_dir(&self) -> bool {
        matches!(self, Self::Dir { .. } | Self::DeepDir { .. })
    }
}

/// An ISO image description.
#[derive(Debug, Clone)]
pub struct IsoFs {
    /// PVD volume identifier.
    pub volume_id: String,
    /// Joliet volume identifier; adds a Joliet tree when set.
    pub joliet: Option<String>,
    /// Whether to add Rock Ridge entries. Primary names are then upper-case
    /// d-characters and the real name is in `NM`; without Rock Ridge, the
    /// given names are used as primary names (files get `;1`).
    pub rock_ridge: bool,
    /// Root directory entries.
    pub root: Vec<Node>,
    /// El Torito entries (their data is not written).
    pub boot: Vec<BootEntry>,
    /// UDF logical volume identifier; adds a UDF file system when set.
    pub udf: Option<String>,
}

impl IsoFs {
    /// An image with the given volume identifier and root entries.
    pub fn new(volume_id: &str, root: Vec<Node>) -> Self {
        Self {
            volume_id: volume_id.to_string(),
            joliet: None,
            rock_ridge: false,
            root,
            boot: Vec::new(),
            udf: None,
        }
    }
}

/// Largest extent of a multi-extent file (a multiple of the block size).
const MAX_EXTENT: u64 = 0xFFFF_F800;

struct Writer {
    image: Vec<u8>,
    next: u32,
}

impl Writer {
    fn alloc(&mut self, blocks: u32) -> u32 {
        let lba = self.next;
        self.next += blocks.max(1);
        let needed = self.next as usize * BLOCK;
        if self.image.len() < needed {
            self.image.resize(needed, 0);
        }
        lba
    }

    fn put(&mut self, offset: usize, bytes: &[u8]) {
        if self.image.len() < offset + bytes.len() {
            self.image.resize(offset + bytes.len(), 0);
        }
        self.image[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
}

fn both32(value: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&value.to_le_bytes());
    out[4..].copy_from_slice(&value.to_be_bytes());
    out
}

fn both16(value: u16) -> [u8; 4] {
    let mut out = [0u8; 4];
    out[..2].copy_from_slice(&value.to_le_bytes());
    out[2..].copy_from_slice(&value.to_be_bytes());
    out
}

fn ucs2(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// A directory record. `su` is the system use area.
fn record(lba: u32, size: u32, flags: u8, name: &[u8], su: &[u8]) -> Vec<u8> {
    let mut rec = vec![0u8; 33];
    rec[2..10].copy_from_slice(&both32(lba));
    rec[10..18].copy_from_slice(&both32(size));
    rec[18..25].copy_from_slice(&[126, 1, 1, 0, 0, 0, 0]);
    rec[25] = flags;
    rec[28..32].copy_from_slice(&both16(1));
    rec[32] = u8::try_from(name.len()).unwrap();
    rec.extend_from_slice(name);
    if rec.len() % 2 == 1 {
        rec.push(0);
    }
    rec.extend_from_slice(su);
    if rec.len() % 2 == 1 {
        rec.push(0);
    }
    rec[0] = u8::try_from(rec.len()).unwrap();
    rec
}

fn su_entry(sig: &[u8; 2], payload: &[u8]) -> Vec<u8> {
    let mut entry = vec![sig[0], sig[1], u8::try_from(payload.len() + 4).unwrap(), 1];
    entry.extend_from_slice(payload);
    entry
}

fn px(mode: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    for value in [mode, 1, 0, 0] {
        payload.extend_from_slice(&both32(value));
    }
    su_entry(b"PX", &payload)
}

fn nm(name: &str) -> Vec<u8> {
    let mut payload = vec![0u8];
    payload.extend_from_slice(name.as_bytes());
    su_entry(b"NM", &payload)
}

fn sl(target: &str) -> Vec<u8> {
    let mut payload = vec![0u8];
    let (rooted, rest) = match target.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, target),
    };
    if rooted {
        payload.extend_from_slice(&[8, 0]);
    }
    for part in rest.split('/').filter(|p| !p.is_empty()) {
        match part {
            "." => payload.extend_from_slice(&[2, 0]),
            ".." => payload.extend_from_slice(&[4, 0]),
            text => {
                payload.extend_from_slice(&[0, u8::try_from(text.len()).unwrap()]);
                payload.extend_from_slice(text.as_bytes());
            }
        }
    }
    su_entry(b"SL", &payload)
}

/// The primary (ISO 9660) name of a node.
fn primary_name(node: &Node, rock_ridge: bool, index: usize) -> Vec<u8> {
    let base = if rock_ridge {
        // Upper-case d-characters, unique thanks to the index suffix.
        let mangled: String = node
            .name()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .take(20)
            .collect();
        format!("{mangled}{index:03}")
    } else {
        node.name().to_string()
    };
    if node.is_dir() {
        base.into_bytes()
    } else {
        format!("{base};1").into_bytes()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tree {
    Primary,
    Joliet,
}

/// What a directory entry refers to in the flat tables.
#[derive(Clone, Copy)]
enum Slot {
    Dir(usize),
    File(usize),
    Symlink,
}

struct Entry<'a> {
    node: &'a Node,
    index: usize,
    slot: Slot,
    /// Rock Ridge continuation block, if any.
    ce: Option<u32>,
}

struct Dir<'a> {
    parent: usize,
    entries: Vec<Entry<'a>>,
}

/// Directories in depth-first order (root = 0) and files in visit order.
fn flatten<'a>(
    root: &'a [Node],
    dirs: &mut Vec<Dir<'a>>,
    files: &mut Vec<&'a Node>,
    parent: usize,
) -> usize {
    let me = dirs.len();
    dirs.push(Dir {
        parent,
        entries: Vec::new(),
    });
    let mut entries = Vec::new();
    for (index, node) in root.iter().enumerate() {
        let slot = match node {
            Node::Dir { children, .. } => Slot::Dir(flatten(children, dirs, files, me)),
            Node::DeepDir { .. } => Slot::Dir(flatten(&[], dirs, files, me)),
            Node::File { .. } => {
                files.push(node);
                Slot::File(files.len() - 1)
            }
            Node::Symlink { .. } => Slot::Symlink,
        };
        entries.push(Entry {
            node,
            index,
            slot,
            ce: None,
        });
    }
    dirs[me].entries = entries;
    me
}

fn tree_name(node: &Node, tree: Tree, rock_ridge: bool, index: usize) -> Vec<u8> {
    match tree {
        Tree::Primary => primary_name(node, rock_ridge, index),
        Tree::Joliet => {
            // Like xorriso: no version suffix, at most 64 characters.
            let name: String = node.name().chars().take(64).collect();
            ucs2(&name)
        }
    }
}

/// Rock Ridge system use for a node; returns the inline area and, when a
/// continuation block is used, the continuation area contents.
fn rock_su(node: &Node, ce_lba: Option<u32>) -> (Vec<u8>, Vec<u8>) {
    let mode = match node {
        Node::Dir { .. } | Node::DeepDir { .. } => 0o040_755,
        Node::Symlink { .. } => 0o120_777,
        Node::File { .. } => 0o100_644,
    };
    let mut inline = px(mode);
    let mut tail = nm(node.name());
    match node {
        Node::Symlink { target, .. } => tail.extend(sl(target)),
        Node::DeepDir { .. } => tail.extend(su_entry(b"PL", &both32(0))),
        _ => {}
    }
    if let Some(lba) = ce_lba {
        let mut payload = Vec::new();
        payload.extend_from_slice(&both32(lba));
        payload.extend_from_slice(&both32(0));
        payload.extend_from_slice(&both32(u32::try_from(tail.len()).unwrap()));
        inline.extend(su_entry(b"CE", &payload));
        (inline, tail)
    } else {
        inline.extend(tail);
        (inline, Vec::new())
    }
}

/// Whether a node's Rock Ridge entries need a continuation area.
fn needs_ce(node: &Node, primary: &[u8]) -> bool {
    let (inline, _) = rock_su(node, None);
    33 + primary.len() + 1 + inline.len() > 250
}

/// Packs records so that none crosses a block boundary.
fn pack(records: &[Vec<u8>]) -> Vec<u8> {
    let mut data = Vec::new();
    for rec in records {
        let used = data.len() % BLOCK;
        if used + rec.len() > BLOCK {
            data.resize(data.len() + BLOCK - used, 0);
        }
        data.extend_from_slice(rec);
    }
    let rem = data.len() % BLOCK;
    if rem != 0 || data.is_empty() {
        data.resize(data.len() + if rem == 0 { BLOCK } else { BLOCK - rem }, 0);
    }
    data
}

impl IsoFs {
    /// The records of directory `me` for `tree`, given directory and file
    /// locations (zeros while sizing).
    fn dir_records(
        &self,
        dirs: &[Dir<'_>],
        me: usize,
        tree: Tree,
        locs: &[[(u32, u32); 2]],
        files: &[u32],
    ) -> Vec<Vec<u8>> {
        let t = tree as usize;
        let rock = self.rock_ridge && tree == Tree::Primary;
        let mut dot_su = Vec::new();
        if rock && me == 0 {
            dot_su.extend(su_entry(b"SP", &[0xbe, 0xef, 0]));
        }
        let dotdot_su = if rock { px(0o040_755) } else { Vec::new() };
        if rock {
            dot_su.extend(px(0o040_755));
        }
        let here = locs[me][t];
        let parent = locs[dirs[me].parent][t];
        let mut records = vec![
            record(here.0, here.1, 2, &[0], &dot_su),
            record(parent.0, parent.1, 2, &[1], &dotdot_su),
        ];
        for entry in &dirs[me].entries {
            let name = tree_name(entry.node, tree, self.rock_ridge, entry.index);
            let su = if rock {
                rock_su(entry.node, entry.ce).0
            } else {
                Vec::new()
            };
            match (entry.slot, entry.node) {
                (Slot::Dir(index), _) => {
                    let (lba, size) = locs[index][t];
                    records.push(record(lba, size, 2, &name, &su));
                }
                (Slot::File(index), Node::File { size, .. }) => {
                    let mut remaining = *size;
                    let mut lba = files[index];
                    loop {
                        let part = remaining.min(MAX_EXTENT);
                        remaining -= part;
                        let flags = if remaining > 0 { 0x80 } else { 0 };
                        records.push(record(lba, u32::try_from(part).unwrap(), flags, &name, &su));
                        lba += u32::try_from(part / BLOCK as u64).unwrap();
                        if remaining == 0 {
                            break;
                        }
                    }
                }
                _ => records.push(record(0, 0, 0, &name, &su)),
            }
        }
        records
    }

    /// Builds the image.
    pub fn build(&self) -> Vec<u8> {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        flatten(&self.root, &mut dirs, &mut files, 0);

        let mut writer = Writer {
            image: vec![0u8; 20 * BLOCK],
            next: 20,
        };
        let catalog = if self.boot.is_empty() {
            None
        } else {
            Some(writer.alloc(1))
        };
        if self.rock_ridge {
            for dir in &mut dirs {
                for entry in &mut dir.entries {
                    if needs_ce(entry.node, &primary_name(entry.node, true, entry.index)) {
                        entry.ce = Some(writer.alloc(1));
                    }
                }
            }
        }
        let trees: &[Tree] = if self.joliet.is_some() {
            &[Tree::Primary, Tree::Joliet]
        } else {
            &[Tree::Primary]
        };
        let zero_files = vec![0u32; files.len()];
        let mut locs = vec![[(0u32, 0u32); 2]; dirs.len()];
        for &tree in trees {
            for me in 0..dirs.len() {
                let bytes = pack(&self.dir_records(
                    &dirs,
                    me,
                    tree,
                    &vec![[(0, 0); 2]; dirs.len()],
                    &zero_files,
                ))
                .len();
                let lba = writer.alloc(u32::try_from(bytes / BLOCK).unwrap());
                locs[me][tree as usize] = (lba, u32::try_from(bytes).unwrap());
            }
        }
        let file_lbas: Vec<u32> = files
            .iter()
            .map(|node| match node {
                Node::File { data, .. } => {
                    writer.alloc(u32::try_from(data.len().div_ceil(BLOCK)).unwrap())
                }
                _ => 0,
            })
            .collect();

        for (node, lba) in files.iter().zip(&file_lbas) {
            if let Node::File { data, .. } = node {
                writer.put(*lba as usize * BLOCK, data);
            }
        }
        for dir in &dirs {
            for entry in &dir.entries {
                if let Some(lba) = entry.ce {
                    let (_, tail) = rock_su(entry.node, Some(lba));
                    writer.put(lba as usize * BLOCK, &tail);
                }
            }
        }
        for &tree in trees {
            for me in 0..dirs.len() {
                let bytes = pack(&self.dir_records(&dirs, me, tree, &locs, &file_lbas));
                writer.put(locs[me][tree as usize].0 as usize * BLOCK, &bytes);
            }
        }

        let mut tables = Vec::new();
        for &tree in trees {
            let (little, big) = self.path_tables(&dirs, tree, &locs);
            let blocks = u32::try_from(little.len().div_ceil(BLOCK)).unwrap();
            let l_lba = writer.alloc(blocks);
            let m_lba = writer.alloc(blocks);
            writer.put(l_lba as usize * BLOCK, &little);
            writer.put(m_lba as usize * BLOCK, &big);
            tables.push((u32::try_from(little.len()).unwrap(), l_lba, m_lba));
        }

        if let Some(lvid) = &self.udf {
            udf::add(&mut writer, lvid, &self.root);
        }
        let total = writer.next;
        self.descriptors(&mut writer, total, &locs, catalog, &tables);
        writer.image.resize(total as usize * BLOCK, 0);
        writer.image
    }

    /// Little- and big-endian path tables, in breadth-first order.
    fn path_tables(
        &self,
        dirs: &[Dir<'_>],
        tree: Tree,
        locs: &[[(u32, u32); 2]],
    ) -> (Vec<u8>, Vec<u8>) {
        // (directory index, parent number, identifier)
        let mut order: Vec<(usize, u16, Vec<u8>)> = vec![(0, 1, vec![0])];
        let mut at = 0;
        while at < order.len() {
            let me = order[at].0;
            let number = u16::try_from(at + 1).unwrap();
            for entry in &dirs[me].entries {
                if let Slot::Dir(child) = entry.slot {
                    order.push((
                        child,
                        number,
                        tree_name(entry.node, tree, self.rock_ridge, entry.index),
                    ));
                }
            }
            at += 1;
        }
        let (mut little, mut big) = (Vec::new(), Vec::new());
        for (me, parent, ident) in order {
            let lba = locs[me][tree as usize].0;
            for (table, le) in [(&mut little, true), (&mut big, false)] {
                table.push(u8::try_from(ident.len()).unwrap());
                table.push(0);
                table.extend_from_slice(&if le {
                    lba.to_le_bytes()
                } else {
                    lba.to_be_bytes()
                });
                table.extend_from_slice(&if le {
                    parent.to_le_bytes()
                } else {
                    parent.to_be_bytes()
                });
                table.extend_from_slice(&ident);
                if ident.len() % 2 == 1 {
                    table.push(0);
                }
            }
        }
        (little, big)
    }

    fn descriptors(
        &self,
        writer: &mut Writer,
        total: u32,
        locs: &[[(u32, u32); 2]],
        catalog: Option<u32>,
        tables: &[(u32, u32, u32)],
    ) {
        let set_tables = |vd: &mut [u8], (size, l_lba, m_lba): (u32, u32, u32)| {
            vd[132..140].copy_from_slice(&both32(size));
            vd[140..144].copy_from_slice(&l_lba.to_le_bytes());
            vd[148..152].copy_from_slice(&m_lba.to_be_bytes());
            vd[881] = 1;
        };
        let mut next = 16usize;
        let root_record = |tree: Tree| {
            let (lba, size) = locs[0][tree as usize];
            record(lba, size, 2, &[0], &[])
        };
        {
            let mut pvd = vec![0u8; BLOCK];
            pvd[0] = 1;
            pvd[1..6].copy_from_slice(b"CD001");
            pvd[6] = 1;
            pvd[8..40].copy_from_slice(&padded("LINUX", 32));
            pvd[40..72].copy_from_slice(&padded(&self.volume_id, 32));
            pvd[80..88].copy_from_slice(&both32(total));
            pvd[120..124].copy_from_slice(&both16(1));
            pvd[124..128].copy_from_slice(&both16(1));
            pvd[128..132].copy_from_slice(&both16(2048));
            let root = root_record(Tree::Primary);
            pvd[156..156 + root.len()].copy_from_slice(&root);
            set_tables(&mut pvd, tables[0]);
            writer.put(next * BLOCK, &pvd);
            next += 1;
        }
        if let Some(catalog) = catalog {
            let mut brvd = vec![0u8; BLOCK];
            brvd[1..6].copy_from_slice(b"CD001");
            brvd[6] = 1;
            brvd[7..30].copy_from_slice(b"EL TORITO SPECIFICATION");
            brvd[0x47..0x4b].copy_from_slice(&catalog.to_le_bytes());
            writer.put(next * BLOCK, &brvd);
            next += 1;
            let mut cat = vec![0u8; BLOCK];
            cat[0] = 1;
            cat[1] = self.boot[0].platform;
            cat[30] = 0x55;
            cat[31] = 0xaa;
            // The 16-bit words of the validation entry sum to zero.
            let sum = cat[..32].chunks(2).fold(0u16, |sum, word| {
                sum.wrapping_add(u16::from_le_bytes([word[0], word[1]]))
            });
            cat[28..30].copy_from_slice(&0u16.wrapping_sub(sum).to_le_bytes());
            let mut offset = 32;
            for (index, entry) in self.boot.iter().enumerate() {
                if index > 0 {
                    cat[offset] = if index + 1 == self.boot.len() {
                        0x91
                    } else {
                        0x90
                    };
                    cat[offset + 1] = entry.platform;
                    cat[offset + 2..offset + 4].copy_from_slice(&1u16.to_le_bytes());
                    offset += 32;
                }
                cat[offset] = 0x88;
                cat[offset + 6..offset + 8].copy_from_slice(&entry.sectors.to_le_bytes());
                cat[offset + 8..offset + 12].copy_from_slice(&entry.lsn.to_le_bytes());
                offset += 32;
            }
            writer.put(catalog as usize * BLOCK, &cat);
        }
        if let Some(volume_id) = &self.joliet {
            let mut svd = vec![0u8; BLOCK];
            svd[0] = 2;
            svd[1..6].copy_from_slice(b"CD001");
            svd[6] = 1;
            let mut label = ucs2(volume_id);
            while label.len() < 32 {
                label.extend_from_slice(&[0, b' ']);
            }
            svd[40..72].copy_from_slice(&label[..32]);
            svd[80..88].copy_from_slice(&both32(total));
            svd[88..91].copy_from_slice(b"%/E");
            svd[128..132].copy_from_slice(&both16(2048));
            svd[120..124].copy_from_slice(&both16(1));
            svd[124..128].copy_from_slice(&both16(1));
            let root = root_record(Tree::Joliet);
            svd[156..156 + root.len()].copy_from_slice(&root);
            set_tables(&mut svd, tables[1]);
            writer.put(next * BLOCK, &svd);
            next += 1;
        }
        let mut terminator = vec![0u8; BLOCK];
        terminator[0] = 255;
        terminator[1..6].copy_from_slice(b"CD001");
        terminator[6] = 1;
        writer.put(next * BLOCK, &terminator);
    }
}

fn padded(text: &str, len: usize) -> Vec<u8> {
    let mut field = vec![b' '; len];
    field[..text.len()].copy_from_slice(text.as_bytes());
    field
}

/// UDF 1.02 file system for bridge discs.
mod udf {
    use super::{Node, Writer};
    use crate::images::BLOCK;

    fn tag(block: &mut [u8], id: u16, location: u32) {
        block[0..2].copy_from_slice(&id.to_le_bytes());
        block[2..4].copy_from_slice(&2u16.to_le_bytes());
        block[12..16].copy_from_slice(&location.to_le_bytes());
        let sum = block[..15]
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 4)
            .fold(0u8, |s, (_, &b)| s.wrapping_add(b));
        block[4] = sum;
    }

    /// A file entry (tag 261) with short allocation descriptors.
    fn file_entry(location: u32, file_type: u8, length: u64, extents: &[(u32, u32)]) -> Vec<u8> {
        let mut fe = vec![0u8; BLOCK];
        fe[16 + 4..16 + 6].copy_from_slice(&4u16.to_le_bytes()); // strategy 4
        fe[16 + 11] = file_type;
        fe[16 + 18..16 + 20].copy_from_slice(&0u16.to_le_bytes()); // short ADs
        fe[48..50].copy_from_slice(&1u16.to_le_bytes());
        fe[56..64].copy_from_slice(&length.to_le_bytes());
        let ad_len = u32::try_from(extents.len() * 8).unwrap();
        fe[172..176].copy_from_slice(&ad_len.to_le_bytes());
        for (index, (lbn, len)) in extents.iter().enumerate() {
            let at = 176 + index * 8;
            fe[at..at + 4].copy_from_slice(&len.to_le_bytes());
            fe[at + 4..at + 8].copy_from_slice(&lbn.to_le_bytes());
        }
        tag(&mut fe, 261, location);
        fe
    }

    fn fid(characteristics: u8, icb: u32, name: &str, location: u32) -> Vec<u8> {
        let mut ident = Vec::new();
        if !name.is_empty() {
            if name.is_ascii() {
                ident.push(8);
                ident.extend_from_slice(name.as_bytes());
            } else {
                ident.push(16);
                ident.extend(name.encode_utf16().flat_map(u16::to_be_bytes));
            }
        }
        let mut fid = vec![0u8; 38];
        fid[16..18].copy_from_slice(&1u16.to_le_bytes());
        fid[18] = characteristics;
        fid[19] = u8::try_from(ident.len()).unwrap();
        fid[20..24].copy_from_slice(&2048u32.to_le_bytes());
        fid[24..28].copy_from_slice(&icb.to_le_bytes());
        fid.extend_from_slice(&ident);
        while !fid.len().is_multiple_of(4) {
            fid.push(0);
        }
        tag(&mut fid, 257, location);
        fid
    }

    /// Partition-relative allocation: returns (fe_lbn) of the directory.
    fn dir(writer: &mut Writer, start: u32, children: &[Node], parent_fe: Option<u32>) -> u32 {
        let fe_lbn = writer.alloc(1) - start;
        let data_lbn = writer.alloc(2) - start;
        let mut entries = Vec::new();
        let mut data = fid(0x0a, parent_fe.unwrap_or(fe_lbn), "", data_lbn);
        for child in children {
            match child {
                Node::Dir {
                    name,
                    children: grand,
                } => entries.push((0x02, dir(writer, start, grand, Some(fe_lbn)), name)),
                Node::DeepDir { name } => {
                    entries.push((0x02, dir(writer, start, &[], Some(fe_lbn)), name))
                }
                Node::File {
                    name,
                    data: bytes,
                    size,
                } => {
                    let fe = writer.alloc(1) - start;
                    let blocks = u32::try_from(bytes.len().div_ceil(BLOCK)).unwrap();
                    let lbn = writer.alloc(blocks) - start;
                    writer.put((start + lbn) as usize * BLOCK, bytes);
                    let mut extents = Vec::new();
                    let mut remaining = *size;
                    let mut at = lbn;
                    while remaining > 0 {
                        let part = remaining.min(0x3FFF_F800);
                        extents.push((at, u32::try_from(part).unwrap()));
                        at += u32::try_from(part / BLOCK as u64).unwrap();
                        remaining -= part;
                    }
                    writer.put(
                        (start + fe) as usize * BLOCK,
                        &file_entry(fe, 5, *size, &extents),
                    );
                    entries.push((0x00, fe, name));
                }
                Node::Symlink { name, target } => {
                    let fe = writer.alloc(1) - start;
                    let lbn = writer.alloc(1) - start;
                    writer.put((start + lbn) as usize * BLOCK, target.as_bytes());
                    let len = u32::try_from(target.len()).unwrap();
                    writer.put(
                        (start + fe) as usize * BLOCK,
                        &file_entry(fe, 12, u64::from(len), &[(lbn, len)]),
                    );
                    entries.push((0x00, fe, name));
                }
            }
        }
        for (characteristics, icb, name) in entries {
            data.extend(fid(characteristics, icb, name, data_lbn));
        }
        assert!(data.len() <= 2 * BLOCK, "UDF fixture directory too large");
        writer.put((start + data_lbn) as usize * BLOCK, &data);
        let len = u32::try_from(data.len()).unwrap();
        writer.put(
            (start + fe_lbn) as usize * BLOCK,
            &file_entry(fe_lbn, 4, u64::from(len), &[(data_lbn, len)]),
        );
        fe_lbn
    }

    /// Adds the UDF volume structures (anchor at 256, descriptors at 32 to
    /// 34) and a partition holding `root`.
    pub(super) fn add(writer: &mut Writer, lvid: &str, root: &[Node]) {
        if writer.next < 260 {
            writer.alloc(260 - writer.next);
        }
        let start = writer.alloc(1);
        // File set descriptor at partition block 0.
        let root_fe = dir(writer, start, root, None);
        let mut fsd = vec![0u8; BLOCK];
        fsd[400..404].copy_from_slice(&2048u32.to_le_bytes());
        fsd[404..408].copy_from_slice(&root_fe.to_le_bytes());
        tag(&mut fsd, 256, 0);
        writer.put(start as usize * BLOCK, &fsd);
        let length = writer.next - start;

        let block = |n: u32| n as usize * BLOCK;
        let mut anchor = vec![0u8; BLOCK];
        anchor[16..20].copy_from_slice(&(16 * 2048u32).to_le_bytes());
        anchor[20..24].copy_from_slice(&32u32.to_le_bytes());
        tag(&mut anchor, 2, 256);
        writer.put(block(256), &anchor);
        let mut pvd = vec![0u8; BLOCK];
        tag(&mut pvd, 1, 32);
        writer.put(block(32), &pvd);
        let mut pd = vec![0u8; BLOCK];
        pd[188..192].copy_from_slice(&start.to_le_bytes());
        pd[192..196].copy_from_slice(&length.to_le_bytes());
        tag(&mut pd, 5, 33);
        writer.put(block(33), &pd);
        let mut lvd = vec![0u8; BLOCK];
        lvd[84] = 8;
        lvd[85..85 + lvid.len()].copy_from_slice(lvid.as_bytes());
        lvd[84 + 127] = u8::try_from(lvid.len() + 1).unwrap();
        lvd[212..216].copy_from_slice(&2048u32.to_le_bytes());
        lvd[248..252].copy_from_slice(&2048u32.to_le_bytes());
        lvd[252..256].copy_from_slice(&0u32.to_le_bytes());
        tag(&mut lvd, 6, 34);
        writer.put(block(34), &lvd);
        let mut terminator = vec![0u8; BLOCK];
        tag(&mut terminator, 8, 35);
        writer.put(block(35), &terminator);
    }
}
