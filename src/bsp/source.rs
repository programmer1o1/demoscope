// Source VBSP (Half-Life 2 / TF2 / CS:S / L4D etc.) world-geometry extraction.
// Walks faces + displacements into a triangle mesh and base64-encodes the
// vertex/index buffers the viewer renders. Lumps may be LZMA-compressed.

use std::collections::{HashMap, HashSet};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use lzma_rs::lzma_decompress;

use super::super::util::bytes::{le_f32, le_i16_bytes, le_i32, le_u16};
use super::find_spawn_in_entities;

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

// Separate coplanar faces that overlap a face already kept on the same plane —
// the classic Source z-fight where a `func_detail` brush face lands exactly on a
// world brush face (identical plane → identical depth → strobing stripes that no
// depth buffer, polygon offset, or log-depth can resolve). Rather than DROP the
// duplicate (which would leave a hole wherever a partial overlap uniquely covered
// surface), we nudge it a fraction of a unit along its normal so both faces still
// render but at different depths — no strobe, no hole. New vertices are appended
// for the nudged faces (they can't share positions with their un-nudged
// neighbours), so the triangle list is rewritten to point at them.
//
// Overlap is a real 2D test (a corner strictly inside the other, or edges
// properly crossing) so partial/banded overlaps are caught; faces that only share
// an edge or vertex (adjacent tiles, the two halves of a quad) are not, so genuine
// flat surfaces are untouched. Huge coplanar buckets are skipped to bound cost.
fn remove_coplanar_overlaps(verts: &mut Vec<[f32; 3]>, tris: Vec<(u32, u32, u32)>) -> Vec<(u32, u32, u32)> {
    let mut tris = tris;

    // Phase 1 (read-only): find which faces overlap, and the normal to nudge
    // them along. Scoped so the immutable borrow of `verts` ends before phase 2.
    let to_offset: Vec<(usize, [f32; 3])> = {
        let v: &[[f32; 3]] = verts;
        let get = |i: u32| v[i as usize];
        let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        let cross = |a: [f32; 3], b: [f32; 3]| {
            [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
        };
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];

        struct TInfo { axis: usize, area: f32, key: (i32, i32, i32, i32), normal: [f32; 3] }
        let mut info: Vec<Option<TInfo>> = Vec::with_capacity(tris.len());
        for &(a, b, c) in &tris {
            let (pa, pb, pc) = (get(a), get(b), get(c));
            let n = cross(sub(pb, pa), sub(pc, pa));
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len < 1e-6 { info.push(None); continue; }
            let area = 0.5 * len;
            let rn = [n[0] / len, n[1] / len, n[2] / len]; // true (un-canonicalised) normal
            let (mut nn, mut d) = (rn, dot(rn, pa));
            // Canonicalise the sign so a face and its opposite-wound twin share a key.
            let s = if nn[0].abs() > 1e-4 { nn[0] } else if nn[1].abs() > 1e-4 { nn[1] } else { nn[2] };
            if s < 0.0 { nn = [-nn[0], -nn[1], -nn[2]]; d = -d; }
            let key = (
                (nn[0] * 64.0).round() as i32, (nn[1] * 64.0).round() as i32,
                (nn[2] * 64.0).round() as i32, (d * 2.0).round() as i32,
            );
            let axis = if nn[0].abs() >= nn[1].abs() && nn[0].abs() >= nn[2].abs() { 0 }
                else if nn[1].abs() >= nn[2].abs() { 1 } else { 2 };
            info.push(Some(TInfo { axis, area, key, normal: rn }));
        }
        let mut buckets: HashMap<(i32, i32, i32, i32), Vec<usize>> = HashMap::new();
        for (i, inf) in info.iter().enumerate() {
            if let Some(t) = inf { buckets.entry(t.key).or_default().push(i); }
        }
        let proj = |p: [f32; 3], axis: usize| -> [f32; 2] {
            match axis { 0 => [p[1], p[2]], 1 => [p[0], p[2]], _ => [p[0], p[1]] }
        };
        // STRICTLY-inside point-in-triangle: all three edge signs agree AND are
        // nonzero. A point on an edge or shared vertex gives a zero, so it isn't
        // counted inside — essential, since every adjacent face shares edges.
        let in_tri = |p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]| -> bool {
            let d1 = (p[0] - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (p[1] - b[1]);
            let d2 = (p[0] - c[0]) * (b[1] - c[1]) - (b[0] - c[0]) * (p[1] - c[1]);
            let d3 = (p[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (p[1] - a[1]);
            (d1 > 0.0 && d2 > 0.0 && d3 > 0.0) || (d1 < 0.0 && d2 < 0.0 && d3 < 0.0)
        };
        let orient = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| -> i32 {
            let val = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            if val > 1e-4 { 1 } else if val < -1e-4 { -1 } else { 0 }
        };
        let seg_cross = |p1, p2, p3, p4| -> bool {
            let (d1, d2, d3, d4) = (orient(p3, p4, p1), orient(p3, p4, p2), orient(p1, p2, p3), orient(p1, p2, p4));
            d1 != 0 && d2 != 0 && d3 != 0 && d4 != 0 && d1 != d2 && d3 != d4
        };
        let tri_overlap = |t1: &[[f32; 2]; 3], t2: &[[f32; 2]; 3]| -> bool {
            if t1.iter().any(|&p| in_tri(p, t2[0], t2[1], t2[2])) { return true; }
            if t2.iter().any(|&p| in_tri(p, t1[0], t1[1], t1[2])) { return true; }
            for i in 0..3 {
                for j in 0..3 {
                    if seg_cross(t1[i], t1[(i + 1) % 3], t2[j], t2[(j + 1) % 3]) { return true; }
                }
            }
            false
        };
        let mut out = Vec::new();
        for idxs in buckets.values() {
            if idxs.len() < 2 || idxs.len() > 4000 { continue; }
            let mut order = idxs.clone();
            order.sort_by(|&x, &y| {
                info[y].as_ref().unwrap().area
                    .partial_cmp(&info[x].as_ref().unwrap().area)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut kept: Vec<([[f32; 2]; 3], [f32; 4])> = Vec::new();
            for &i in &order {
                let inf = info[i].as_ref().unwrap();
                let (a, b, c) = tris[i];
                let tri = [proj(get(a), inf.axis), proj(get(b), inf.axis), proj(get(c), inf.axis)];
                let bb = [
                    tri[0][0].min(tri[1][0]).min(tri[2][0]), tri[0][1].min(tri[1][1]).min(tri[2][1]),
                    tri[0][0].max(tri[1][0]).max(tri[2][0]), tri[0][1].max(tri[1][1]).max(tri[2][1]),
                ];
                let overlaps = kept.iter().any(|(t, kbb)| {
                    bb[0] <= kbb[2] && bb[2] >= kbb[0] && bb[1] <= kbb[3] && bb[3] >= kbb[1]
                        && tri_overlap(&tri, t)
                });
                if overlaps {
                    out.push((i, inf.normal));
                } else {
                    kept.push((tri, bb));
                }
            }
        }
        out
    };

    // Phase 2 (mutate): nudge each overlapping face along its normal by a small,
    // per-face-varied amount (so stacked duplicates separate from each other too),
    // appending fresh vertices so neighbours aren't dragged along.
    for (i, n) in to_offset {
        let h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 28;
        let mag = 0.3 + 0.5 * ((h & 0xF) as f32 / 15.0); // 0.30 .. 0.80 units
        let (a, b, c) = tris[i];
        let mut nidx = [0u32; 3];
        for (k, &vi) in [a, b, c].iter().enumerate() {
            let p = verts[vi as usize];
            verts.push([p[0] + n[0] * mag, p[1] + n[1] * mag, p[2] + n[2] * mag]);
            nidx[k] = (verts.len() - 1) as u32;
        }
        tris[i] = (nidx[0], nidx[1], nidx[2]);
    }
    tris
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

    // Separate coplanar overlapping faces (a func_detail brush sitting exactly on
    // a world brush, etc.) - they share a plane, so they render at identical depth
    // and strobe with no depth buffer able to resolve them. Nudged apart, not
    // dropped, so no holes appear.
    let tris = remove_coplanar_overlaps(&mut verts_xyz, tris);

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
