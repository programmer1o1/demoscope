// Minimal Valve Pak (VPK) v2 directory reader — just enough to pull a single
// resource (e.g. `maps/<map>/world_physics.vmdl_c`) out of a CS2 map pak.
//
// CS2 ships each map as a self-contained `<map>.vpk` whose entries all live
// inline in the directory file (archive index 0x7fff). We only support that
// inline layout: split-archive paks (`_dir.vpk` + `_NNN.vpk`) would need the
// sibling archive files, which a map `.vpk` dropped next to a demo doesn't use.
//
// Format reference: ValveResourceFormat (ValvePak) + the public VDC VPK spec.

const VPK_SIGNATURE: u32 = 0x55aa_1234;
/// Archive index meaning "the bytes are inline in this directory file".
const INLINE_ARCHIVE: u16 = 0x7fff;

fn le_u16(d: &[u8], o: usize) -> u16 { u16::from_le_bytes([d[o], d[o + 1]]) }
fn le_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// One directory entry, with enough to locate its bytes in the inline section.
pub struct VpkEntry {
    pub full_path: String,
    pub archive_index: u16,
    pub entry_offset: u32,
    pub entry_length: u32,
    /// Inline preload bytes that precede the archive data (copied from the tree).
    pub preload: Vec<u8>,
    /// Absolute start of the inline data section in the directory file.
    data_section: usize,
}

pub struct Vpk<'a> {
    data: &'a [u8],
    pub entries: Vec<VpkEntry>,
}

/// Read a NUL-terminated string from `tree` at `*pos`, advancing past the NUL.
fn read_cstr(tree: &[u8], pos: &mut usize) -> Option<String> {
    let start = *pos;
    let end = tree[start..].iter().position(|&b| b == 0)? + start;
    let s = String::from_utf8_lossy(&tree[start..end]).into_owned();
    *pos = end + 1;
    Some(s)
}

impl<'a> Vpk<'a> {
    /// Parse a VPK v2 directory. Returns None if the magic/version don't match.
    pub fn parse(data: &'a [u8]) -> Option<Vpk<'a>> {
        if data.len() < 28 || le_u32(data, 0) != VPK_SIGNATURE {
            return None;
        }
        let version = le_u32(data, 4);
        if version != 2 {
            return None; // v1 has a 12-byte header; CS2 maps are always v2
        }
        let tree_size = le_u32(data, 8) as usize;
        let header_size = 28;
        let tree_end = header_size + tree_size;
        if tree_end > data.len() {
            return None;
        }
        let tree = &data[header_size..tree_end];
        // Inline file bytes follow the tree.
        let data_section = tree_end;

        let mut entries = Vec::new();
        let mut pos = 0usize;
        loop {
            let ext = read_cstr(tree, &mut pos)?;
            if ext.is_empty() {
                break;
            }
            loop {
                let path = read_cstr(tree, &mut pos)?;
                if path.is_empty() {
                    break;
                }
                loop {
                    let name = read_cstr(tree, &mut pos)?;
                    if name.is_empty() {
                        break;
                    }
                    if pos + 18 > tree.len() {
                        return None;
                    }
                    let _crc = le_u32(tree, pos);
                    let preload_len = le_u16(tree, pos + 4) as usize;
                    let archive_index = le_u16(tree, pos + 6);
                    let entry_offset = le_u32(tree, pos + 8);
                    let entry_length = le_u32(tree, pos + 12);
                    let _terminator = le_u16(tree, pos + 16);
                    pos += 18;
                    let preload = if preload_len > 0 {
                        if pos + preload_len > tree.len() {
                            return None;
                        }
                        let p = tree[pos..pos + preload_len].to_vec();
                        pos += preload_len;
                        p
                    } else {
                        Vec::new()
                    };
                    // VPK paths use a single space to mean "no directory".
                    let full_path = if path == " " {
                        format!("{name}.{ext}")
                    } else {
                        format!("{path}/{name}.{ext}")
                    };
                    entries.push(VpkEntry {
                        full_path,
                        archive_index,
                        entry_offset,
                        entry_length,
                        preload,
                        data_section,
                    });
                }
            }
        }
        Some(Vpk { data, entries })
    }

    /// First entry whose path ends with `suffix` (e.g. `"world_physics.vmdl_c"`).
    pub fn find(&self, suffix: &str) -> Option<&VpkEntry> {
        self.entries.iter().find(|e| e.full_path.ends_with(suffix))
    }

    /// Read an entry's full bytes (preload + inline archive data). Returns None
    /// for entries stored in a separate archive file (not the single-file map
    /// pak layout we support).
    pub fn read(&self, e: &VpkEntry) -> Option<Vec<u8>> {
        if e.archive_index != INLINE_ARCHIVE {
            return None;
        }
        let start = e.data_section + e.entry_offset as usize;
        let end = start + e.entry_length as usize;
        if end > self.data.len() {
            return None;
        }
        let mut out = Vec::with_capacity(e.preload.len() + e.entry_length as usize);
        out.extend_from_slice(&e.preload);
        out.extend_from_slice(&self.data[start..end]);
        Some(out)
    }
}
