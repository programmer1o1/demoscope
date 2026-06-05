// Quake 2 / Quake 3 IBSP world-geometry extraction. Both share the `IBSP`
// magic but differ by version (38 vs 46) and lump layout. Each builds a raw
// triangle list, then hands off to the shared `finish_bsp` compactor.

use super::super::util::bytes::{le_f32, le_i32, le_u16};
use super::finish_bsp;

// ── Quake 2 BSP (IBSP, version 38) ───────────────────────────────────────────
//
// Same edge/surfedge fan-triangulation as GoldSrc, but an `IBSP` header with 19
// lumps at byte 8, different lump indices, a 76-byte texinfo (surface flags at
// +32), and a 48-byte model. Sky/nodraw faces are dropped via the SURF flags.
pub(crate) fn extract_q2_bsp_from_bytes(
    data: &[u8],
) -> Option<(String, String, usize, usize, [f32; 3])> {
    if data.len() < 8 + 19 * 8 || &data[0..4] != b"IBSP" || le_i32(data, 4) != 38 {
        return None;
    }
    let lump = |i: usize| -> (usize, usize) {
        let o = 8 + i * 8;
        let off = le_i32(data, o);
        let len = le_i32(data, o + 4);
        if off < 0 || len < 0 || (off as usize) + (len as usize) > data.len() {
            (0, 0)
        } else {
            (off as usize, len as usize)
        }
    };
    let (en_off, en_len) = lump(0);
    let (ti_off, ti_len) = lump(5);
    let (f_off, f_len) = lump(6);
    let (e_off, e_len) = lump(11);
    let (se_off, se_len) = lump(12);
    let (m_off, m_len) = lump(13);
    let (v_off, v_len) = lump(2);

    let n_verts = v_len / 12;
    let n_faces = f_len / 20;
    let n_se = se_len / 4;
    let n_ti = ti_len / 76;
    if n_verts == 0 || n_faces == 0 || n_se == 0 {
        return None;
    }
    let verts_xyz: Vec<[f32; 3]> = (0..n_verts)
        .map(|i| {
            let o = v_off + i * 12;
            [le_f32(data, o), le_f32(data, o + 4), le_f32(data, o + 8)]
        })
        .collect();
    let edges: Vec<(u16, u16)> = (0..e_len / 4)
        .map(|i| (le_u16(data, e_off + i * 4), le_u16(data, e_off + i * 4 + 2)))
        .collect();
    let se: Vec<i32> = (0..n_se).map(|i| le_i32(data, se_off + i * 4)).collect();

    // World = model 0 (firstface @ 40, numfaces @ 44 in the 48-byte model).
    let (world_first, world_end) = if m_len >= 48 {
        let ff = le_i32(data, m_off + 40) as usize;
        let nf = le_i32(data, m_off + 44) as usize;
        (ff, (ff + nf).min(n_faces))
    } else {
        (0, n_faces)
    };

    const MAX_TRIS: usize = 600_000;
    const SURF_SKY: i32 = 0x4;
    const SURF_NODRAW: i32 = 0x80;
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
        if ti_idx < n_ti {
            let flags = le_i32(data, ti_off + ti_idx * 76 + 32);
            if flags & (SURF_SKY | SURF_NODRAW) != 0 {
                continue;
            }
        }
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
        for i in 1..fv.len().saturating_sub(1) {
            tris.push((fv[0], fv[i], fv[i + 1]));
        }
    }

    let entities = if en_len > 0 && en_off + en_len <= data.len() {
        String::from_utf8_lossy(&data[en_off..en_off + en_len]).into_owned()
    } else {
        String::new()
    };
    finish_bsp(&verts_xyz, &tris, &entities)
}

// ── Quake 3 BSP (IBSP, version 46) ───────────────────────────────────────────
//
// A different model from Q1/Q2: no edges/surfedges. Faces index into a meshvert
// list (per-face index offsets) which in turn indexes the 44-byte drawverts.
// Face types handled: 1 polygon + 3 mesh (both meshvert-indexed triangle soups).
// Type 2 (bezier patch) is counted and skipped - a wireframe overlay loses
// little without curve tessellation, and many maps (e.g. cpm4) have none; type 4
// (billboard) is skipped. Sky/tool shaders are dropped by shader name.
pub(crate) fn extract_q3_bsp_from_bytes(
    data: &[u8],
) -> Option<(String, String, usize, usize, [f32; 3])> {
    if data.len() < 8 + 17 * 8 || &data[0..4] != b"IBSP" || le_i32(data, 4) != 46 {
        return None;
    }
    let lump = |i: usize| -> (usize, usize) {
        let o = 8 + i * 8;
        let off = le_i32(data, o);
        let len = le_i32(data, o + 4);
        if off < 0 || len < 0 || (off as usize) + (len as usize) > data.len() {
            (0, 0)
        } else {
            (off as usize, len as usize)
        }
    };
    let (en_off, en_len) = lump(0);
    let (tx_off, tx_len) = lump(1); // shaders (textures)
    let (v_off, v_len) = lump(10); // drawverts
    let (mv_off, mv_len) = lump(11); // meshverts
    let (f_off, f_len) = lump(13); // faces

    let n_verts = v_len / 44;
    let n_mv = mv_len / 4;
    let n_faces = f_len / 104;
    let n_tex = tx_len / 72;
    if n_verts == 0 || n_faces == 0 {
        return None;
    }
    // drawVert position is the first 12 bytes of each 44-byte record.
    let verts_xyz: Vec<[f32; 3]> = (0..n_verts)
        .map(|i| {
            let o = v_off + i * 44;
            [le_f32(data, o), le_f32(data, o + 4), le_f32(data, o + 8)]
        })
        .collect();

    // Shader name (64-byte name at the start of each 72-byte shader record).
    let shader_name = |idx: i32| -> String {
        if idx < 0 || idx as usize >= n_tex {
            return String::new();
        }
        let o = tx_off + idx as usize * 72;
        if o + 64 > data.len() {
            return String::new();
        }
        let raw = &data[o..o + 64];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(64);
        String::from_utf8_lossy(&raw[..end]).to_lowercase()
    };
    let is_skip = |n: &str| -> bool {
        n.is_empty() || n == "noshader" || n.contains("common/") || n.contains("sky")
    };
    let meshvert = |i: usize| -> i32 {
        if i < n_mv { le_i32(data, mv_off + i * 4) } else { 0 }
    };

    const MAX_TRIS: usize = 600_000;
    let mut tris: Vec<(u32, u32, u32)> = Vec::new();
    let mut patches = 0usize;
    for fi in 0..n_faces {
        if tris.len() >= MAX_TRIS {
            break;
        }
        let b = f_off + fi * 104;
        if b + 104 > data.len() {
            continue;
        }
        let tex = le_i32(data, b);
        let ftype = le_i32(data, b + 8);
        let firstvert = le_i32(data, b + 12);
        let firstmv = le_i32(data, b + 20);
        let n_face_mv = le_i32(data, b + 24);
        if is_skip(&shader_name(tex)) {
            continue;
        }
        match ftype {
            1 | 3 => {
                // Polygon / mesh: meshverts are offsets relative to firstvert.
                if firstvert < 0 || firstmv < 0 || n_face_mv < 0 {
                    continue;
                }
                let fv = firstvert as u32;
                let mut k = 0i32;
                while k + 2 < n_face_mv {
                    let a = fv.wrapping_add(meshvert((firstmv + k) as usize) as u32);
                    let b2 = fv.wrapping_add(meshvert((firstmv + k + 1) as usize) as u32);
                    let c = fv.wrapping_add(meshvert((firstmv + k + 2) as usize) as u32);
                    if (a as usize) < n_verts && (b2 as usize) < n_verts && (c as usize) < n_verts {
                        tris.push((a, b2, c));
                    }
                    k += 3;
                }
            }
            2 => patches += 1, // bezier patch - not tessellated (overlay)
            _ => {}            // billboard / unknown - skip
        }
    }
    if patches > 0 {
        eprintln!("  Q3 BSP: {} bezier patch(es) skipped (curves not tessellated)", patches);
    }

    let entities = if en_len > 0 && en_off + en_len <= data.len() {
        String::from_utf8_lossy(&data[en_off..en_off + en_len]).into_owned()
    } else {
        String::new()
    };
    finish_bsp(&verts_xyz, &tris, &entities)
}
