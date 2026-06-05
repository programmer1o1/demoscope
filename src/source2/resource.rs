// Source 2 compiled-resource container ("binary KV3 resource"): the outer
// wrapper shared by every `*_c` file (`.vmdl_c`, `.vwrld_c`, `.vmap_c`, …).
//
// Layout:
//   u32 file_size
//   u16 header_version          (12 for current files)
//   u16 resource_version
//   u32 block_offset            (relative to its own position, i.e. offset 8)
//   u32 block_count
//   block_count × {
//     [4] block_type 4CC        (e.g. "DATA", "PHYS", "RERL", "MVTX")
//     u32 offset                (relative to its own position)
//     u32 size
//   }
//
// We only need to fetch a block's raw bytes by 4CC; the inner block format
// (KV3, VBIB, …) is decoded elsewhere.

fn le_u16(d: &[u8], o: usize) -> u16 { u16::from_le_bytes([d[o], d[o + 1]]) }
fn le_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

pub struct Block {
    pub kind: [u8; 4],
    pub start: usize,
    pub size: usize,
}

pub struct Resource<'a> {
    data: &'a [u8],
    pub blocks: Vec<Block>,
}

impl<'a> Resource<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Resource<'a>> {
        if data.len() < 16 {
            return None;
        }
        let block_offset = le_u32(data, 8) as usize;
        let block_count = le_u32(data, 12) as usize;
        // Sanity: a 4-char type tag wouldn't fit if these are garbage.
        if block_count > 64 {
            return None;
        }
        let table = 8 + block_offset;
        let mut blocks = Vec::with_capacity(block_count);
        for i in 0..block_count {
            let p = table + i * 12;
            if p + 12 > data.len() {
                return None;
            }
            let mut kind = [0u8; 4];
            kind.copy_from_slice(&data[p..p + 4]);
            let off = le_u32(data, p + 4) as usize;
            let size = le_u32(data, p + 8) as usize;
            let start = (p + 4) + off; // offset is relative to the offset field
            if start + size > data.len() {
                return None;
            }
            blocks.push(Block { kind, start, size });
        }
        Some(Resource { data, blocks })
    }

    /// Raw bytes of the first block with this 4CC tag (e.g. `b"PHYS"`).
    pub fn block(&self, kind: &[u8; 4]) -> Option<&'a [u8]> {
        let b = self.blocks.iter().find(|b| &b.kind == kind)?;
        Some(&self.data[b.start..b.start + b.size])
    }

    /// Header version (offset 4) — handy for debugging/format gating.
    pub fn header_version(&self) -> u16 {
        le_u16(self.data, 4)
    }
}
