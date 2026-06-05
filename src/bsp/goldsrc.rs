// GoldSrc / Quake-1 BSP (version 30 / 29) world-geometry extraction.
//
// The Half-Life / CS 1.6 / DoD map format. Same lump-based world-geometry idea
// as Source VBSP (vertices + edges + surfedges + faces, fan-triangulated), but
// a much simpler header and smaller structs, and no LZMA / no displacements:
//
//   header: version(i32) then 15 lumps × (offset i32, length i32) at byte 4.
//   vertex  = 3×f32 (12 B)        edge     = 2×u16 (4 B)     surfedge = i32
//   dface_t = 20 B { planenum(u16) side(u16) firstedge(i32) numedges(u16)
//                    texinfo(u16) styles[4] lightofs(i32) }
//   texinfo = 40 B { vecs[2][4] f32 (32) miptex(i32) flags(i32) }
//   dmodel_t= 64 B { mins[3] maxs[3] origin[3] headnode[4] visleafs(i32)
//                    firstface(i32) numfaces(i32) }   -> firstface@56 numfaces@60
//   textures lump: nummiptex(i32) + offsets[nummiptex](i32) + miptex_t's, each
//                  starting with name[16] - used to drop sky / tool brushes.
//
// Quake 1 BSP (version 29) is byte-identical in these lumps, so this handles
// both. Returns the same (verts_b64, idx_b64, n_verts, n_tris, spawn) tuple the
// VBSP path does, so the viewer consumes it identically.

use std::collections::{HashMap, HashSet};

use base64::{engine::general_purpose::STANDARD, Engine as _};

use super::super::util::bytes::{le_f32, le_i32, le_u16};
use super::find_spawn_in_entities;

pub(crate) fn extract_goldsrc_bsp_from_bytes(
    data: &[u8],
) -> Option<(String, String, usize, usize, [f32; 3])> {
    if data.len() < 4 + 15 * 8 {
        return None;
    }
    let version = le_i32(data, 0);
    if version != 30 && version != 29 {
        return None; // not a GoldSrc (30) / Quake 1 (29) BSP
    }
    // 15 lumps, each (offset, length) at byte 4.
    let lump = |i: usize| -> (usize, usize) {
        let o = 4 + i * 8;
        let off = le_i32(data, o);
        let len = le_i32(data, o + 4);
        if off < 0 || len < 0 || (off as usize) + (len as usize) > data.len() {
            (0, 0)
        } else {
            (off as usize, len as usize)
        }
    };
    const L_ENTITIES: usize = 0;
    const L_TEXTURES: usize = 2;
    const L_VERTICES: usize = 3;
    const L_TEXINFO: usize = 6;
    const L_FACES: usize = 7;
    const L_EDGES: usize = 12;
    const L_SURFEDGES: usize = 13;
    const L_MODELS: usize = 14;

    let (v_off, v_len) = lump(L_VERTICES);
    let (f_off, f_len) = lump(L_FACES);
    let (e_off, e_len) = lump(L_EDGES);
    let (se_off, se_len) = lump(L_SURFEDGES);
    let (ti_off, ti_len) = lump(L_TEXINFO);
    let (m_off, m_len) = lump(L_MODELS);
    let (tx_off, tx_len) = lump(L_TEXTURES);
    let (en_off, en_len) = lump(L_ENTITIES);

    let n_verts = v_len / 12;
    let n_faces = f_len / 20;
    let n_edges = e_len / 4;
    let n_se = se_len / 4;
    let n_ti = ti_len / 40;
    if n_verts == 0 || n_faces == 0 || n_se == 0 {
        return None;
    }

    // Vertices.
    let verts_xyz: Vec<[f32; 3]> = (0..n_verts)
        .map(|i| {
            let o = v_off + i * 12;
            [le_f32(data, o), le_f32(data, o + 4), le_f32(data, o + 8)]
        })
        .collect();
    // Edges (pair of u16 vertex indices).
    let edges: Vec<(u16, u16)> = (0..n_edges)
        .map(|i| {
            let o = e_off + i * 4;
            (le_u16(data, o), le_u16(data, o + 2))
        })
        .collect();
    // Surfedges (signed: sign = direction).
    let se: Vec<i32> = (0..n_se).map(|i| le_i32(data, se_off + i * 4)).collect();

    // Texture-name lookup: miptex index -> lowercased name, to drop sky / tools.
    let tex_name = |miptex: i32| -> String {
        if miptex < 0 || tx_len < 4 {
            return String::new();
        }
        let nummiptex = le_i32(data, tx_off);
        if miptex >= nummiptex || nummiptex <= 0 {
            return String::new();
        }
        let off_tbl = tx_off + 4 + miptex as usize * 4;
        if off_tbl + 4 > data.len() {
            return String::new();
        }
        let rel = le_i32(data, off_tbl);
        if rel < 0 {
            return String::new();
        }
        let name_off = tx_off + rel as usize;
        if name_off + 16 > data.len() {
            return String::new();
        }
        let raw = &data[name_off..name_off + 16];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(16);
        String::from_utf8_lossy(&raw[..end]).to_lowercase()
    };
    // Tool / sky textures whose brushes shouldn't render as solid geometry.
    let is_skip_tex = |name: &str| -> bool {
        name.is_empty()
            || name.starts_with("sky")
            || name.starts_with("aaatrigger")
            || name == "clip"
            || name == "null"
            || name == "hint"
            || name == "skip"
            || name == "origin"
            || name.starts_with("trigger")
    };

    // World geometry = model 0 (mins/maxs/origin/headnode[4]/visleafs = 56,
    // then firstface@56, numfaces@60). Models 1+ are brush entities - skip.
    let (world_first, world_end) = if m_len >= 64 {
        let ff = le_i32(data, m_off + 56) as usize;
        let nf = le_i32(data, m_off + 60) as usize;
        (ff, (ff + nf).min(n_faces))
    } else {
        (0, n_faces)
    };

    const MAX_TRIS: usize = 600_000;
    let mut tris: Vec<(u32, u32, u32)> = Vec::new();
    for fi in world_first..world_end {
        if tris.len() >= MAX_TRIS {
            break;
        }
        let b = f_off + fi * 20;
        if b + 20 > data.len() {
            continue;
        }
        let firstedge = le_i32(data, b + 4);
        let numedges = le_u16(data, b + 8) as i32;
        let ti_idx = le_u16(data, b + 10) as usize;
        if numedges < 3 {
            continue;
        }
        // Drop sky / trigger faces by their texture name.
        if ti_idx < n_ti {
            let miptex = le_i32(data, ti_off + ti_idx * 40 + 32);
            if is_skip_tex(&tex_name(miptex)) {
                continue;
            }
        }
        // Resolve the face's corner vertices through the surfedge -> edge table.
        let mut fv: Vec<u32> = Vec::with_capacity(numedges as usize);
        for i in 0..numedges {
            let se_idx = (firstedge + i) as usize;
            if se_idx >= se.len() {
                break;
            }
            let s = se[se_idx];
            let vi = if s >= 0 {
                let idx = s as usize;
                if idx < edges.len() { edges[idx].0 as u32 } else { continue }
            } else {
                let idx = s.unsigned_abs() as usize;
                if idx < edges.len() { edges[idx].1 as u32 } else { continue }
            };
            fv.push(vi);
        }
        // Fan-triangulate the polygon.
        for i in 1..fv.len().saturating_sub(1) {
            tris.push((fv[0], fv[i], fv[i + 1]));
        }
    }

    if tris.is_empty() {
        return None;
    }

    // Compact to used vertices and remap indices.
    let used: Vec<u32> = {
        let mut set: HashSet<u32> = HashSet::new();
        for &(a, b, c) in &tris {
            set.insert(a);
            set.insert(b);
            set.insert(c);
        }
        let mut v: Vec<u32> = set.into_iter().collect();
        v.sort_unstable();
        v
    };
    let mut remap: HashMap<u32, u32> = HashMap::with_capacity(used.len());
    for (ni, &oi) in used.iter().enumerate() {
        remap.insert(oi, ni as u32);
    }
    let compact_v: Vec<[f32; 3]> = used
        .iter()
        .map(|&i| if (i as usize) < verts_xyz.len() { verts_xyz[i as usize] } else { [0.0; 3] })
        .collect();
    let compact_t: Vec<(u32, u32, u32)> = tris
        .iter()
        .filter_map(|&(a, b, c)| Some((*remap.get(&a)?, *remap.get(&b)?, *remap.get(&c)?)))
        .collect();

    // Encode to base64 (same little-endian f32/u32 layout as the VBSP path).
    let mut v_buf = Vec::with_capacity(compact_v.len() * 12);
    for &[x, y, z] in &compact_v {
        v_buf.extend_from_slice(&x.to_le_bytes());
        v_buf.extend_from_slice(&y.to_le_bytes());
        v_buf.extend_from_slice(&z.to_le_bytes());
    }
    let mut i_buf = Vec::with_capacity(compact_t.len() * 12);
    for &(a, b, c) in &compact_t {
        i_buf.extend_from_slice(&a.to_le_bytes());
        i_buf.extend_from_slice(&b.to_le_bytes());
        i_buf.extend_from_slice(&c.to_le_bytes());
    }
    let verts_b64 = STANDARD.encode(&v_buf);
    let idx_b64 = STANDARD.encode(&i_buf);

    // Spawn origin from the entity lump.
    let spawn = if en_len > 0 && en_off + en_len <= data.len() {
        let en_str = String::from_utf8_lossy(&data[en_off..en_off + en_len]);
        find_spawn_in_entities(&en_str).unwrap_or([0.0; 3])
    } else {
        [0.0; 3]
    };

    Some((verts_b64, idx_b64, compact_v.len(), compact_t.len(), spawn))
}
