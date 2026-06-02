// VBSP map geometry extraction: resolves the .bsp alongside a demo, LZMA-
// decompresses lumps, walks faces + displacements into a triangle mesh, and
// base64-encodes the vertex/index buffers the viewer renders. Also pulls the
// spawn origin out of the entity lump.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use lzma_rs::lzma_decompress;

use super::bytes::{le_f32, le_i16_bytes, le_i32, le_u16};

// Decompress a VBSP lump that may be LZMA-compressed.
// TF2/OrangBox BSP lumps start with b"LZMA" when compressed.
// Format: magic(4) + actual_size(u32) + lzma_size(u32) + properties(5) + data
fn decompress_lzma_lump(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 4 { return None; }
    if &data[0..4] != b"LZMA" {
        return Some(data.to_vec());
    }
    if data.len() < 17 { return None; }
    let actual_size = u32::from_le_bytes(data[4..8].try_into().ok()?) as u64;
    let props = &data[12..17];  // 5 bytes: [lc/lp/pb, dict_size LE u32]
    let body = &data[17..];

    // Build LZMA "alone" format: props(5) + uncompressed_size(8 LE) + body
    let mut stream: Vec<u8> = Vec::with_capacity(13 + body.len());
    stream.extend_from_slice(props);
    stream.extend_from_slice(&actual_size.to_le_bytes());
    stream.extend_from_slice(body);

    let mut out = Vec::with_capacity(actual_size as usize);
    lzma_decompress(&mut stream.as_slice(), &mut out).ok()?;
    Some(out)
}

pub(crate) fn find_bsp_file(dem_path: &Path, map_name: &str) -> Option<PathBuf> {
    let map_lower = map_name.to_lowercase();

    let candidates: Vec<PathBuf> = {
        let mut v = Vec::new();
        if let Some(parent) = dem_path.parent() {
            v.push(parent.join(format!("{}.bsp", map_name)));
            v.push(parent.join(format!("{}.bsp", map_lower)));
        }
        // executable dir
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                v.push(exe_dir.join(format!("{}.bsp", map_name)));
                v.push(exe_dir.join(format!("{}.bsp", map_lower)));
            }
        }
        v
    };

    for c in candidates {
        if c.exists() {
            return Some(c);
        }
    }
    None
}

// Path wrapper - opens the file and delegates to the byte-slice core. Keeps
// the existing CLI flow working unchanged; WASM goes straight through the
// `_from_bytes` variant since there's no filesystem in the browser. The
// wrapper itself is unused now that generate_html reads bytes up front and
// hands them in, but it stays as a convenience for any future direct callers.
#[allow(dead_code)]
pub(crate) fn extract_bsp(bsp_path: &Path) -> Option<(String, String, usize, usize, [f32; 3])> {
    let mut f = File::open(bsp_path).ok()?;
    let mut data = Vec::new();
    f.read_to_end(&mut data).ok()?;
    extract_bsp_from_bytes(&data)
}

// Standard VBSP puts the 64-entry lump directory at offset 8 (right after
// ident + version). Some v21 files (seen on an L4D2 c8m2_subway.bsp) carry an
// extra 4-byte field after `version`, shifting the directory to offset 12 -
// reading it at 8 yields pure garbage (huge versions, offset-0 lumps) and the
// downstream face walk then trips on noise. Score each candidate base by how
// many of the 64 entries look sane and prefer the standard 8 on a tie, so no
// existing v20 BSP regresses.
fn detect_lump_base(data: &[u8]) -> usize {
    let filelen = data.len() as i64;
    let score = |base: usize| -> usize {
        let mut ok = 0usize;
        for i in 0..64 {
            let o = base + i * 16;
            if o + 16 > data.len() { break; }
            let ofs = le_i32(data, o) as i64;
            let len = le_i32(data, o + 4) as i64;
            let ver = le_i32(data, o + 8);
            let sane_ver = (0..=255).contains(&ver);
            let sane_span = len == 0 || (ofs > 0 && ofs + len <= filelen);
            if sane_ver && sane_span { ok += 1; }
        }
        ok
    };
    if score(12) > score(8) { 12 } else { 8 }
}

pub(crate) fn extract_bsp_from_bytes(data: &[u8]) -> Option<(String, String, usize, usize, [f32; 3])> {
    if data.len() < 1036 { return None; }
    if &data[0..4] != b"VBSP" { return None; }

    // Read lump table: 64 lumps × 16 bytes each, at the auto-detected base.
    let lump_base = detect_lump_base(data);
    let lump_raw = |i: usize| -> (usize, usize) {
        let o = lump_base + i * 16;
        (le_i32(&data, o) as usize, le_i32(&data, o + 4) as usize)
    };

    // Decompress (or copy) a lump into an owned Vec
    let get_lump = |i: usize| -> Option<Vec<u8>> {
        let (off, len) = lump_raw(i);
        if off + len > data.len() { return None; }
        decompress_lzma_lump(&data[off..off + len])
    };

    let en_data = get_lump(0)?;   // entities
    let v_data  = get_lump(3)?;   // vertices
    let ti_data = get_lump(6)?;   // texinfo
    let f_data  = get_lump(7)?;   // faces
    let e_data  = get_lump(12)?;  // edges
    let se_data = get_lump(13)?;  // surfedges
    let m_data  = get_lump(14);   // models
    let di_data = get_lump(26).unwrap_or_default(); // dispinfo
    let dv_data = get_lump(33).unwrap_or_default(); // disp_verts

    let n_verts = v_data.len() / 12;
    let n_tinfo = ti_data.len() / 72;
    let n_faces = f_data.len() / 56;
    let n_edges = e_data.len() / 4;
    let n_se    = se_data.len() / 4;

    if n_verts == 0 || n_faces == 0 { return None; }

    // Parse texinfo flags (offset 64 in each 72-byte struct)
    let mut ti_flags: Vec<i32> = vec![0i32; n_tinfo];
    for i in 0..n_tinfo {
        let off = i * 72 + 64;
        if off + 4 <= ti_data.len() { ti_flags[i] = le_i32(&ti_data, off); }
    }

    // Edges: pair of u16 vertex indices
    let mut edges: Vec<(u16, u16)> = Vec::with_capacity(n_edges);
    for i in 0..n_edges {
        let o = i * 4;
        if o + 4 <= e_data.len() { edges.push((le_u16(&e_data, o), le_u16(&e_data, o + 2))); }
        else { edges.push((0, 0)); }
    }

    // Surfedges: i32 (sign encodes edge direction)
    let mut se: Vec<i32> = Vec::with_capacity(n_se);
    for i in 0..n_se {
        let o = i * 4;
        if o + 4 <= se_data.len() { se.push(le_i32(&se_data, o)); }
        else { se.push(0); }
    }

    // Vertices: float32 x3
    let mut verts_xyz: Vec<[f32; 3]> = Vec::with_capacity(n_verts);
    for i in 0..n_verts {
        let o = i * 12;
        if o + 12 <= v_data.len() {
            verts_xyz.push([le_f32(&v_data, o), le_f32(&v_data, o + 4), le_f32(&v_data, o + 8)]);
        } else {
            verts_xyz.push([0.0; 3]);
        }
    }

    // Surface flags to skip: sky2d(0x02), sky(0x04), trigger(0x40), nodraw(0x80), hint(0x100), skip(0x200)
    let skip_flags: i32 = 0x02 | 0x04 | 0x40 | 0x80 | 0x100 | 0x200;

    // Model 0 = worldspawn (static geometry + func_detail compiled in).
    // Models 1+ are brush entities - skip to avoid trigger boxes and floating origin brushes.
    // dmodel_t: mins(12) + maxs(12) + origin(12) + headnode(4) + firstface(4) + numfaces(4) = 48 bytes
    let (world_first, world_end) = match &m_data {
        Some(m) if m.len() >= 48 => {
            let ff = le_i32(m, 40) as usize;
            let nf = le_i32(m, 44) as usize;
            (ff, (ff + nf).min(n_faces))
        }
        _ => (0, n_faces),
    };

    // Collect triangles. Non-displacement faces use fan triangulation of
    // surfedge corners. Displacement faces are tessellated into a (2^power+1)²
    // grid via the algorithm from qbyte's SourceImporter
    // (~/Downloads/__init__.py): the dispinfo gives 4 face corners + a starting
    // corner; we bilinear-interp positions and offset each grid vert by its
    // DISPVERT direction × distance.
    //
    // DISPINFO (176 bytes per entry on Source v20):
    //    0..12  : startPosition (Vector)
    //   12..16  : DispVertStart (i32)
    //   16..20  : DispTriStart (i32)
    //   20..24  : power (i32)
    //  rest     : minTess / smoothingAngle / neighbors etc - ignored here.
    //
    // DISPVERT (20 bytes per entry):
    //    0..12  : vec (Vector - unit direction)
    //   12..16  : dist (f32)
    //   16..20  : alpha (f32, unused here)
    const DISPINFO_SIZE: usize = 176;
    const DISPVERT_SIZE: usize = 20;
    let n_disp = di_data.len() / DISPINFO_SIZE;

    // Decoded displacements (separate vertex pool - appended after compaction).
    let mut disp_verts_xyz: Vec<[f32; 3]> = Vec::new();
    let mut disp_tris: Vec<(u32, u32, u32)> = Vec::new();

    const MAX_TRIS: usize = 600_000;
    let mut tris: Vec<(u32, u32, u32)> = Vec::new();
    for fi in world_first..world_end {
        if tris.len() + disp_tris.len() >= MAX_TRIS { break; }
        let b = fi * 56;
        if b + 56 > f_data.len() { continue; }
        let firstedge = le_i32(&f_data, b + 4) as i32;
        let numedges  = le_i16_bytes(&f_data, b + 8) as i32;
        let ti_idx    = le_i16_bytes(&f_data, b + 10) as i32;
        let dispinfo  = le_i16_bytes(&f_data, b + 12) as i32;

        if ti_idx < 0 || ti_idx as usize >= n_tinfo { continue; }
        if ti_flags[ti_idx as usize] & skip_flags != 0 { continue; }
        if numedges < 3 { continue; }

        // Resolve corner verts via surfedges.
        let mut fv: Vec<u32> = Vec::with_capacity(numedges as usize);
        'edge: for i in 0..numedges {
            let se_idx = (firstedge + i) as usize;
            if se_idx >= se.len() { break; }
            let s = se[se_idx];
            let vi = if s >= 0 {
                let idx = s as usize;
                if idx < edges.len() { edges[idx].0 as u32 } else { continue 'edge; }
            } else {
                // unsigned_abs handles s == i32::MIN (negating it as i32 overflows);
                // garbage indices fall through the bounds check below.
                let idx = s.unsigned_abs() as usize;
                if idx < edges.len() { edges[idx].1 as u32 } else { continue 'edge; }
            };
            fv.push(vi);
        }

        if dispinfo == -1 {
            for i in 1..fv.len().saturating_sub(1) {
                tris.push((fv[0], fv[i], fv[i + 1]));
            }
            continue;
        }

        // ── Displacement face ──
        if numedges != 4 { continue; } // Source displacements are always quads.
        // Corner resolution above can skip edges on bad surfedge/edge indices,
        // leaving fewer than 4 corners; the bilinear basis below needs exactly 4.
        if fv.len() != 4 { continue; }
        let di = dispinfo as usize;
        if di >= n_disp { continue; }
        let di_off = di * DISPINFO_SIZE;
        let sx = le_f32(&di_data, di_off);
        let sy = le_f32(&di_data, di_off + 4);
        let sz = le_f32(&di_data, di_off + 8);
        let dv_start = le_i32(&di_data, di_off + 12) as usize;
        let power    = le_i32(&di_data, di_off + 20);
        if !(1..=4).contains(&power) { continue; }
        let n = (1usize << power) + 1; // verts per side
        let total_dv = n * n;
        if dv_start + total_dv > dv_data.len() / DISPVERT_SIZE { continue; }

        // Look up the 4 corner positions (in surfedge order).
        let c: Vec<[f32; 3]> = fv.iter().map(|&i| {
            if (i as usize) < verts_xyz.len() { verts_xyz[i as usize] } else { [0.0; 3] }
        }).collect();

        // Rotate corners so c[start_idx] is closest to startPosition.
        let d2 = |a: &[f32; 3], b: (f32, f32, f32)| {
            let dx = a[0] - b.0; let dy = a[1] - b.1; let dz = a[2] - b.2;
            dx*dx + dy*dy + dz*dz
        };
        let start = (sx, sy, sz);
        let mut start_idx = 0usize;
        let mut best = d2(&c[0], start);
        for i in 1..4 {
            let d = d2(&c[i], start);
            if d < best { best = d; start_idx = i; }
        }
        // Pick rotation per qbyte's mapping; the 4 corners then act as
        // v00, v10, v01, v11 (bilinear basis with start at v00).
        let idxs: [usize; 4] = match start_idx {
            0 => [0, 1, 3, 2],
            1 => [1, 2, 0, 3],
            2 => [2, 3, 1, 0],
            _ => [3, 0, 2, 1],
        };
        let v00 = c[idxs[0]];
        let v10 = c[idxs[1]];
        let v01 = c[idxs[2]];
        let v11 = c[idxs[3]];

        // Allocate grid: index (y, x) → flat index y + x*n (qbyte's layout).
        let base = disp_verts_xyz.len() as u32;
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| -> [f32; 3] {
            [a[0]*(1.0-t) + b[0]*t, a[1]*(1.0-t) + b[1]*t, a[2]*(1.0-t) + b[2]*t]
        };
        let denom = (n - 1) as f32;
        for y0 in 0..n {
            let ty = y0 as f32 / denom;
            let a = lerp(v00, v01, ty);
            let b = lerp(v10, v11, ty);
            for x0 in 0..n {
                let tx = x0 as f32 / denom;
                let p = lerp(a, b, tx);
                let dv_idx = (dv_start + y0 + x0 * n) * DISPVERT_SIZE;
                let dvx = le_f32(&dv_data, dv_idx);
                let dvy = le_f32(&dv_data, dv_idx + 4);
                let dvz = le_f32(&dv_data, dv_idx + 8);
                let dist = le_f32(&dv_data, dv_idx + 12);
                disp_verts_xyz.push([
                    p[0] + dvx * dist,
                    p[1] + dvy * dist,
                    p[2] + dvz * dist,
                ]);
            }
        }
        // Triangulate the grid. Two tris per quad in alternating pattern -
        // matches Source's runtime tessellation (and visually equivalent to
        // qbyte's 8-tri fan for wireframe rendering, at 1/4 the tri budget).
        let nu = n as u32;
        let idx_of = |x: u32, y: u32| -> u32 { base + y + x * nu };
        for y0 in 0..(nu - 1) {
            for x0 in 0..(nu - 1) {
                let i00 = idx_of(x0, y0);
                let i10 = idx_of(x0 + 1, y0);
                let i01 = idx_of(x0, y0 + 1);
                let i11 = idx_of(x0 + 1, y0 + 1);
                if (x0 + y0) & 1 == 0 {
                    disp_tris.push((i00, i10, i11));
                    disp_tris.push((i00, i11, i01));
                } else {
                    disp_tris.push((i00, i10, i01));
                    disp_tris.push((i10, i11, i01));
                }
            }
        }
    }

    if tris.is_empty() && disp_tris.is_empty() { return None; }

    // Compact non-displacement geometry: remap to only used vertex indices.
    let used: Vec<u32> = {
        let mut set: HashSet<u32> = HashSet::new();
        for &(a, b, c) in &tris { set.insert(a); set.insert(b); set.insert(c); }
        let mut v: Vec<u32> = set.into_iter().collect();
        v.sort_unstable();
        v
    };
    let mut remap: HashMap<u32, u32> = HashMap::with_capacity(used.len());
    for (ni, &oi) in used.iter().enumerate() { remap.insert(oi, ni as u32); }

    let mut compact_v: Vec<[f32; 3]> = used.iter()
        .map(|&i| if (i as usize) < verts_xyz.len() { verts_xyz[i as usize] } else { [0.0; 3] })
        .collect();
    let mut compact_t: Vec<(u32, u32, u32)> = tris.iter()
        .filter_map(|&(a, b, c)| Some((*remap.get(&a)?, *remap.get(&b)?, *remap.get(&c)?)))
        .collect();

    // Append displacement verts + tris. Displacement indices are already
    // self-consistent (they referenced disp_verts_xyz directly), so we just
    // shift them by the new base after extending compact_v.
    if !disp_tris.is_empty() {
        let shift = compact_v.len() as u32;
        compact_v.extend(disp_verts_xyz.iter().copied());
        for (a, b, c) in disp_tris {
            compact_t.push((a + shift, b + shift, c + shift));
        }
    }

    // Encode to base64
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
    let idx_b64   = STANDARD.encode(&i_buf);

    // Parse entity lump for spawn origin
    let en_str = String::from_utf8_lossy(&en_data);
    let spawn = find_spawn_in_entities(&en_str).unwrap_or([0.0; 3]);

    Some((verts_b64, idx_b64, compact_v.len(), compact_t.len(), spawn))
}

fn find_spawn_in_entities(entities: &str) -> Option<[f32; 3]> {
    // Parse entity blocks: { ... } and look for classname + origin keys
    let mut chars = entities.chars().peekable();

    loop {
        // Find opening brace
        while let Some(&c) = chars.peek() {
            if c == '{' {
                chars.next();
                break;
            }
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }

        // Read until closing brace
        let mut block = String::new();
        let mut depth = 1;
        for c in chars.by_ref() {
            if c == '{' { depth += 1; }
            else if c == '}' {
                depth -= 1;
                if depth == 0 { break; }
            }
            block.push(c);
        }

        // Parse key-value pairs from block
        let mut classname = String::new();
        let mut origin = String::new();

        let mut i = 0;
        let b = block.as_bytes();
        while i < b.len() {
            // Skip whitespace
            while i < b.len() && (b[i] == b' ' || b[i] == b'\n' || b[i] == b'\r' || b[i] == b'\t') {
                i += 1;
            }
            if i >= b.len() { break; }
            // Read quoted key
            if b[i] != b'"' { i += 1; continue; }
            i += 1;
            let key_start = i;
            while i < b.len() && b[i] != b'"' { i += 1; }
            let key = std::str::from_utf8(&b[key_start..i]).unwrap_or("").trim().to_string();
            if i < b.len() { i += 1; } // skip closing quote

            // Skip whitespace
            while i < b.len() && (b[i] == b' ' || b[i] == b'\n' || b[i] == b'\r' || b[i] == b'\t') {
                i += 1;
            }
            // Read quoted value
            if i >= b.len() || b[i] != b'"' { continue; }
            i += 1;
            let val_start = i;
            while i < b.len() && b[i] != b'"' { i += 1; }
            let val = std::str::from_utf8(&b[val_start..i]).unwrap_or("").to_string();
            if i < b.len() { i += 1; }

            match key.as_str() {
                "classname" => classname = val,
                "origin" => origin = val,
                _ => {}
            }
        }

        let cls_lower = classname.to_lowercase();
        if cls_lower.contains("teamspawn") || classname == "info_player_start" {
            let parts: Vec<&str> = origin.split_whitespace().collect();
            if parts.len() == 3 {
                if let (Ok(x), Ok(y), Ok(z)) = (
                    parts[0].parse::<f32>(),
                    parts[1].parse::<f32>(),
                    parts[2].parse::<f32>(),
                ) {
                    return Some([x, y, z]);
                }
            }
        }
    }
    None
}

// ── GoldSrc / Quake-1 BSP (version 30 / 29) ──────────────────────────────────
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

// Shared tail: compact a triangle list against a vertex pool, base64-encode the
// vertex/index buffers, and pull a spawn from the entity lump text.
fn finish_bsp(
    verts_xyz: &[[f32; 3]],
    tris: &[(u32, u32, u32)],
    entities: &str,
) -> Option<(String, String, usize, usize, [f32; 3])> {
    if tris.is_empty() {
        return None;
    }
    let used: Vec<u32> = {
        let mut set: HashSet<u32> = HashSet::new();
        for &(a, b, c) in tris {
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
    let spawn = find_spawn_in_entities(entities).unwrap_or([0.0; 3]);
    Some((STANDARD.encode(&v_buf), STANDARD.encode(&i_buf), compact_v.len(), compact_t.len(), spawn))
}

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

/// Dispatch a `.bsp` to the right decoder by magic + version. Covers Source
/// (VBSP), GoldSrc / Quake 1 (version 30 / 29), Quake 2 (IBSP 38) and Quake 3
/// (IBSP 46). Returns the shared (verts_b64, idx_b64, n_verts, n_tris, spawn).
pub(crate) fn extract_any_bsp(data: &[u8]) -> Option<(String, String, usize, usize, [f32; 3])> {
    if data.len() < 8 {
        return None;
    }
    if &data[0..4] == b"VBSP" {
        extract_bsp_from_bytes(data)
    } else if &data[0..4] == b"IBSP" {
        match le_i32(data, 4) {
            38 => extract_q2_bsp_from_bytes(data),
            46 => extract_q3_bsp_from_bytes(data),
            _ => None,
        }
    } else {
        match le_i32(data, 0) {
            29 | 30 => extract_goldsrc_bsp_from_bytes(data),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Decode a vertex buffer's X/Y/Z bounds from the base64 the extractor emits.
    fn bounds(v_b64: &str) -> ([f32; 3], [f32; 3]) {
        let buf = STANDARD.decode(v_b64).unwrap();
        let n = buf.len() / 12;
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for i in 0..n {
            for k in 0..3 {
                let c = f32::from_le_bytes(buf[i * 12 + k * 4..i * 12 + k * 4 + 4].try_into().unwrap());
                lo[k] = lo[k].min(c);
                hi[k] = hi[k].max(c);
            }
        }
        (lo, hi)
    }

    // These files are the local sample maps; skip cleanly if they're absent so
    // the test isn't a hard dependency on the demo corpus.
    fn try_bsp(rel: &str) -> Option<Vec<u8>> {
        std::fs::read(rel).ok()
    }

    #[test]
    fn quake_bsps_decode() {
        // (file, expected version-family, sanity floor on tri count)
        let cases = [
            ("DEMOS TESTING/dm6.bsp", "Q1 v29"),
            ("DEMOS TESTING/base64.bsp", "Q2 IBSP38"),
            ("DEMOS TESTING/cpm4.bsp", "Q3 IBSP46"),
        ];
        for (path, label) in cases {
            let Some(data) = try_bsp(path) else {
                eprintln!("skip {label}: {path} not present");
                continue;
            };
            let out = extract_any_bsp(&data)
                .unwrap_or_else(|| panic!("{label}: extract_any_bsp returned None"));
            let (vb64, _ib64, nv, nt, _spawn) = out;
            assert!(nv > 100 && nt > 100, "{label}: too few verts/tris ({nv}/{nt})");
            let (lo, hi) = bounds(&vb64);
            // A real map spans a meaningful, finite volume.
            for k in 0..3 {
                let span = hi[k] - lo[k];
                assert!(span > 50.0 && span < 100_000.0 && span.is_finite(),
                    "{label}: axis {k} span {span} implausible");
            }
            eprintln!("{label}: {nv} verts, {nt} tris, bounds X[{:.0},{:.0}] Y[{:.0},{:.0}] Z[{:.0},{:.0}]",
                lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]);
        }
    }
}
