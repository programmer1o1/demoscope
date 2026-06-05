// BSP map geometry extraction. Resolves the `.bsp` alongside a demo and
// dispatches to the right per-engine decoder, each of which walks the map's
// world geometry into a triangle mesh and base64-encodes the vertex/index
// buffers the viewer renders. Also pulls the spawn origin from the entity lump.
//
// Per-engine decoders live in submodules: `source` (VBSP, with LZMA lumps and
// displacements), `goldsrc` (HL1 / Quake 1 v30/v29), and `quake` (Quake 2/3
// IBSP). The shared entity-spawn scan and triangle-list compactor stay here so
// every decoder reuses them.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD, Engine as _};

use super::util::bytes::le_i32;

mod goldsrc;
mod quake;
mod source;

pub(crate) use goldsrc::extract_goldsrc_bsp_from_bytes;
pub(crate) use quake::{extract_q2_bsp_from_bytes, extract_q3_bsp_from_bytes};
pub(crate) use source::extract_bsp_from_bytes;

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

/// Locate a CS2 map pak (`<map>.vpk`) beside the demo (or next to the
/// executable). CS2 maps aren't VBSP — they're VPK-packed Source 2 resources —
/// so the Source 2 render path resolves them through here instead of a `.bsp`.
pub(crate) fn find_vpk_file(dem_path: &Path, map_name: &str) -> Option<PathBuf> {
    // The map name can arrive as `de_nuke` or `maps/de_nuke`; use just the stem.
    let stem = Path::new(map_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| map_name.to_string());
    if stem.is_empty() {
        return None;
    }
    let stem_lower = stem.to_lowercase();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(parent) = dem_path.parent() {
        candidates.push(parent.join(format!("{stem}.vpk")));
        candidates.push(parent.join(format!("{stem_lower}.vpk")));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            candidates.push(exe_dir.join(format!("{stem}.vpk")));
            candidates.push(exe_dir.join(format!("{stem_lower}.vpk")));
        }
    }
    candidates.into_iter().find(|c| c.exists())
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

// Scan the entity lump text for the player spawn origin. Shared by every
// per-engine decoder (and `finish_bsp`).
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

// Shared tail: compact a triangle list against a vertex pool, base64-encode the
// vertex/index buffers, and pull a spawn from the entity lump text. Used by the
// Quake 2/3 decoders (the VBSP/GoldSrc paths inline an equivalent compaction
// because they also append a separate displacement vertex pool).
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



