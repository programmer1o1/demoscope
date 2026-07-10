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

use base64::{Engine as _, engine::general_purpose::STANDARD};

use super::super::util::bytes::{le_f32, le_i32, le_u16};
use super::find_spawn_in_entities;

/// Spatial metadata needed to stitch consecutive GoldSrc campaign maps.
/// `info_landmark` origins are the coordinate-space anchors used by the engine
/// itself during a changelevel; bounds let the renderer decide which map owns
/// a new post-changelevel track segment.
#[derive(Clone, Debug, Default)]
pub(crate) struct GoldSrcMapInfo {
    pub(crate) landmarks: HashMap<String, [f32; 3]>,
    pub(crate) mins: [f32; 3],
    pub(crate) maxs: [f32; 3],
}

fn goldsrc_lump(data: &[u8], index: usize) -> Option<(usize, usize)> {
    if data.len() < 4 + 15 * 8 || !matches!(le_i32(data, 0), 29 | 30) {
        return None;
    }
    let o = 4 + index * 8;
    let off = le_i32(data, o);
    let len = le_i32(data, o + 4);
    if off < 0 || len < 0 {
        return None;
    }
    let (off, len) = (off as usize, len as usize);
    off.checked_add(len)
        .filter(|end| *end <= data.len())
        .map(|_| (off, len))
}

fn entity_blocks(text: &str) -> Vec<HashMap<String, String>> {
    let bytes = text.as_bytes();
    let mut entities = Vec::new();
    let mut p = 0usize;
    while p < bytes.len() {
        while p < bytes.len() && bytes[p] != b'{' {
            p += 1;
        }
        if p == bytes.len() {
            break;
        }
        p += 1;
        let mut fields = HashMap::new();
        while p < bytes.len() && bytes[p] != b'}' {
            while p < bytes.len() && bytes[p].is_ascii_whitespace() {
                p += 1;
            }
            if p >= bytes.len() || bytes[p] == b'}' {
                break;
            }
            if p >= bytes.len() || bytes[p] != b'"' {
                p += 1;
                continue;
            }
            p += 1;
            let ks = p;
            while p < bytes.len() && bytes[p] != b'"' {
                p += 1;
            }
            let key = String::from_utf8_lossy(&bytes[ks..p]).into_owned();
            p = (p + 1).min(bytes.len());
            while p < bytes.len() && bytes[p].is_ascii_whitespace() {
                p += 1;
            }
            if p >= bytes.len() || bytes[p] != b'"' {
                continue;
            }
            p += 1;
            let vs = p;
            while p < bytes.len() && bytes[p] != b'"' {
                p += 1;
            }
            let value = String::from_utf8_lossy(&bytes[vs..p]).into_owned();
            p = (p + 1).min(bytes.len());
            fields.insert(key, value);
        }
        entities.push(fields);
        p = (p + 1).min(bytes.len());
    }
    entities
}

fn parse_origin(value: &str) -> Option<[f32; 3]> {
    let mut values = value.split_whitespace().map(str::parse::<f32>);
    let origin = [
        values.next()?.ok()?,
        values.next()?.ok()?,
        values.next()?.ok()?,
    ];
    if values.next().is_none() && origin.iter().all(|n| n.is_finite()) {
        Some(origin)
    } else {
        None
    }
}

/// Read the landmark anchors and raw coordinate bounds from a v29/v30 BSP.
pub(crate) fn goldsrc_map_info(data: &[u8]) -> Option<GoldSrcMapInfo> {
    const L_ENTITIES: usize = 0;
    const L_VERTICES: usize = 3;
    let (v_off, v_len) = goldsrc_lump(data, L_VERTICES)?;
    if v_len < 12 {
        return None;
    }
    let mut mins = [f32::INFINITY; 3];
    let mut maxs = [f32::NEG_INFINITY; 3];
    for o in (v_off..v_off + v_len - 11).step_by(12) {
        for axis in 0..3 {
            let value = le_f32(data, o + axis * 4);
            if value.is_finite() {
                mins[axis] = mins[axis].min(value);
                maxs[axis] = maxs[axis].max(value);
            }
        }
    }
    let mut landmarks = HashMap::new();
    if let Some((e_off, e_len)) = goldsrc_lump(data, L_ENTITIES) {
        let text = String::from_utf8_lossy(&data[e_off..e_off + e_len]);
        for entity in entity_blocks(&text) {
            if !entity
                .get("classname")
                .is_some_and(|v| v.eq_ignore_ascii_case("info_landmark"))
            {
                continue;
            }
            let Some(name) = entity.get("targetname").filter(|v| !v.trim().is_empty()) else {
                continue;
            };
            let Some(origin) = entity.get("origin").and_then(|v| parse_origin(v)) else {
                continue;
            };
            landmarks.insert(name.trim().to_ascii_lowercase(), origin);
        }
    }
    Some(GoldSrcMapInfo {
        landmarks,
        mins,
        maxs,
    })
}

/// Compute local→campaign translations for ordered map layers. Consecutive HL1
/// maps contain an `info_landmark` with the same targetname; placing those two
/// origins on top of one another reproduces GoldSrc's changelevel transform.
/// Looking backward also handles a supplied layer with no direct predecessor.
pub(crate) fn goldsrc_alignment_offsets(infos: &[GoldSrcMapInfo]) -> Vec<[f32; 3]> {
    let mut offsets = vec![[0.0; 3]; infos.len()];
    for i in 1..infos.len() {
        'previous: for j in (0..i).rev() {
            let mut common: Vec<&String> = infos[i]
                .landmarks
                .keys()
                .filter(|name| infos[j].landmarks.contains_key(*name))
                .collect();
            common.sort_unstable();
            if let Some(name) = common.first() {
                let from = infos[j].landmarks[*name];
                let to = infos[i].landmarks[*name];
                for axis in 0..3 {
                    offsets[i][axis] = offsets[j][axis] + from[axis] - to[axis];
                }
                break 'previous;
            }
        }
    }
    offsets
}

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
                if idx < edges.len() {
                    edges[idx].0 as u32
                } else {
                    continue;
                }
            } else {
                let idx = s.unsigned_abs() as usize;
                if idx < edges.len() {
                    edges[idx].1 as u32
                } else {
                    continue;
                }
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
        .map(|&i| {
            if (i as usize) < verts_xyz.len() {
                verts_xyz[i as usize]
            } else {
                [0.0; 3]
            }
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn map_with_landmarks(entries: &[(&str, [f32; 3])]) -> Vec<u8> {
        let mut entities = String::from("{\n\"classname\" \"worldspawn\"\n}\n");
        for (name, origin) in entries {
            entities.push_str(&format!(
                "{{\n\"classname\" \"info_landmark\"\n\"targetname\" \"{name}\"\n\"origin\" \"{} {} {}\"\n}}\n",
                origin[0], origin[1], origin[2]
            ));
        }
        entities.push('\0');
        let entity_offset = 4 + 15 * 8;
        let vertex_offset = entity_offset + entities.len();
        let mut data = vec![0u8; vertex_offset + 24];
        data[0..4].copy_from_slice(&30i32.to_le_bytes());
        data[4..8].copy_from_slice(&(entity_offset as i32).to_le_bytes());
        data[8..12].copy_from_slice(&(entities.len() as i32).to_le_bytes());
        let vertex_lump = 4 + 3 * 8;
        data[vertex_lump..vertex_lump + 4].copy_from_slice(&(vertex_offset as i32).to_le_bytes());
        data[vertex_lump + 4..vertex_lump + 8].copy_from_slice(&24i32.to_le_bytes());
        data[entity_offset..vertex_offset].copy_from_slice(entities.as_bytes());
        for (i, value) in [-64.0f32, -32.0, -8.0, 128.0, 96.0, 72.0]
            .into_iter()
            .enumerate()
        {
            let at = vertex_offset + i * 4;
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        data
    }

    #[test]
    fn reads_landmarks_and_map_bounds() {
        let data = map_with_landmarks(&[("LM_A", [10.0, 20.0, 30.0])]);
        let info = goldsrc_map_info(&data).unwrap();
        assert_eq!(info.landmarks["lm_a"], [10.0, 20.0, 30.0]);
        assert_eq!(info.mins, [-64.0, -32.0, -8.0]);
        assert_eq!(info.maxs, [128.0, 96.0, 72.0]);
    }

    #[test]
    fn chains_shared_landmark_transforms() {
        let maps = [
            map_with_landmarks(&[("a", [100.0, 0.0, 0.0])]),
            map_with_landmarks(&[("a", [10.0, 0.0, 0.0]), ("b", [500.0, 0.0, 0.0])]),
            map_with_landmarks(&[("b", [50.0, 0.0, 0.0])]),
        ];
        let infos: Vec<_> = maps
            .iter()
            .map(|map| goldsrc_map_info(map).unwrap())
            .collect();
        assert_eq!(infos[0].landmarks.get("a"), Some(&[100.0, 0.0, 0.0]));
        assert_eq!(infos[1].landmarks.get("a"), Some(&[10.0, 0.0, 0.0]));
        assert_eq!(
            goldsrc_alignment_offsets(&infos),
            vec![[0.0, 0.0, 0.0], [90.0, 0.0, 0.0], [540.0, 0.0, 0.0]]
        );
    }
}
