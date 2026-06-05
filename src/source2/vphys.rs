// Walk a decoded Source 2 physics (PHYS) KV3 tree into a single triangle mesh:
// the world-collision geometry used as the CS2 map wireframe overlay.
//
// The shape data hangs off `m_parts[].m_rnShape`, which carries two geometry
// kinds, both already in world space for `world_physics.vmdl_c`:
//   • m_meshes[] — `RnMesh_t`: a plain vertex array (Vec3 f32) + triangle array
//     (3 × int32 indices). The bulk of the map surface.
//   • m_hulls[]  — `RnHull_t`: a convex hull as vertex positions (Vec3 f32) plus
//     a half-edge face structure we fan-triangulate. Brushwork / props.
//
// Output matches what the BSP path hands the viewer: a flat vertex list plus a
// u32 triangle-index list.

use std::collections::HashMap;

use super::kv3::Value;

// Interact-as tags that mark a collision shape as an *invisible* volume — clip
// brushes, tool brushes, skybox, sound/light blockers. A shape whose only tags
// are these has no visible surface, so it's skipped (CS2 maps are dense with
// player/grenade clips that would otherwise clutter the overlay).
const INVISIBLE_TAGS: &[&str] = &[
    "playerclip", "npcclip", "grenadeclip", "csgo_grenadeclip", "clip",
    "sky", "blocksound", "blocklight", "blocklos", "ladder", "trigger",
];

/// Does a collision attribute describe a *visible* world surface? An empty
/// interact-as list is the default solid world (visible); a non-empty list is
/// visible only if at least one tag isn't an invisible-volume tag (so
/// `[solid, blocksound]` and `[window]` stay, `[playerclip]` and `[sky]` drop).
fn attr_is_visible(attr: &Value) -> bool {
    let tags = match attr.get("m_InteractAsStrings").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return true,
    };
    if tags.is_empty() {
        return true;
    }
    tags.iter().any(|t| match t {
        Value::String(s) => !INVISIBLE_TAGS.contains(&s.to_lowercase().as_str()),
        _ => false,
    })
}

/// Whether a shape (the outer hull/mesh wrapper) should be drawn, by resolving
/// its `m_nCollisionAttributeIndex` against the part's collision-attribute table.
fn shape_visible(wrapper: &Value, attrs: Option<&[Value]>) -> bool {
    let attrs = match attrs {
        Some(a) => a,
        None => return true, // no table → don't filter
    };
    let idx = wrapper.get("m_nCollisionAttributeIndex").and_then(|v| v.as_i64());
    match idx.and_then(|i| usize::try_from(i).ok()).and_then(|i| attrs.get(i)) {
        Some(attr) => attr_is_visible(attr),
        None => true,
    }
}

/// Decode a blob of tightly-packed little-endian `Vec3<f32>` into points.
fn blob_to_vec3(blob: &[u8]) -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(blob.len() / 12);
    let mut o = 0;
    while o + 12 <= blob.len() {
        let f = |i: usize| f32::from_le_bytes(blob[o + i..o + i + 4].try_into().unwrap());
        out.push([f(0), f(4), f(8)]);
        o += 12;
    }
    out
}

/// Append a `RnMesh_t`'s vertices + triangles, re-basing indices onto `verts`.
fn add_mesh(mesh: &Value, verts: &mut Vec<[f32; 3]>, indices: &mut Vec<u32>) {
    let vblob = match mesh.get("m_Vertices").and_then(|v| v.as_blob()) {
        Some(b) => b,
        None => return,
    };
    let tblob = match mesh.get("m_Triangles").and_then(|v| v.as_blob()) {
        Some(b) => b,
        None => return,
    };
    let base = verts.len() as u32;
    let local = blob_to_vec3(vblob);
    let n = local.len() as u32;
    verts.extend_from_slice(&local);
    // Each RnTriangle_t is three 4-byte indices (12 bytes).
    let mut o = 0;
    while o + 12 <= tblob.len() {
        let idx = |i: usize| u32::from_le_bytes(tblob[o + i..o + i + 4].try_into().unwrap());
        let (a, b, c) = (idx(0), idx(4), idx(8));
        if a < n && b < n && c < n {
            indices.push(base + a);
            indices.push(base + b);
            indices.push(base + c);
        }
        o += 12;
    }
}

/// Pull every vertex of a convex hull toward the hull's centroid by a small,
/// distance-proportional amount (1%, capped at 0.6 units). Separates overlapping
/// hull surfaces in depth to stop z-fighting; the cap keeps big brushes from
/// developing visible seams at clean abutments.
fn inset_toward_centroid(pos: &mut [[f32; 3]]) {
    let n = pos.len() as f32;
    if n < 1.0 {
        return;
    }
    let mut c = [0.0f32; 3];
    for p in pos.iter() {
        c[0] += p[0]; c[1] += p[1]; c[2] += p[2];
    }
    c = [c[0] / n, c[1] / n, c[2] / n];
    const FRAC: f32 = 0.01;
    const MAX_INSET: f32 = 0.6;
    for p in pos.iter_mut() {
        let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if len <= 1e-4 {
            continue;
        }
        let inset = (len * FRAC).min(MAX_INSET);
        let s = (len - inset) / len;
        p[0] = c[0] + d[0] * s;
        p[1] = c[1] + d[1] * s;
        p[2] = c[2] + d[2] * s;
    }
}

/// Append a `RnHull_t`. Hull faces are polygons walked through a half-edge ring
/// (`RnHalfEdge_t` = {next, twin, origin, face} as 4 × u8); each is fan-split
/// into triangles over the hull's vertex positions.
fn add_hull(hull: &Value, verts: &mut Vec<[f32; 3]>, indices: &mut Vec<u32>) {
    let mut pos = match hull.get("m_VertexPositions").and_then(|v| v.as_blob()) {
        Some(b) => blob_to_vec3(b),
        None => return,
    };
    let edges = match hull.get("m_Edges").and_then(|v| v.as_blob()) {
        Some(b) => b,
        None => return,
    };
    let faces = match hull.get("m_Faces").and_then(|v| v.as_blob()) {
        Some(b) => b,
        None => return,
    };
    // Shrink the hull a hair toward its own centroid. The world solid is a union
    // of overlapping convex brush hulls, so adjacent hulls have near-coplanar
    // faces at almost-identical depth — opaque, same material, so nothing
    // (polygon offset, log-depth) separates them and they GPU-z-fight (the
    // water-like flicker). Pulling each hull's surface in by a tiny, distance-
    // proportional amount (capped so large floor/wall brushes barely move)
    // separates those overlapping surfaces in depth without visibly gapping
    // clean abutments.
    inset_toward_centroid(&mut pos);
    let base = verts.len() as u32;
    let nv = pos.len() as u32;
    verts.extend_from_slice(&pos);

    let edge_count = edges.len() / 4;
    let next = |e: usize| edges[e * 4] as usize;       // m_nNext
    let origin = |e: usize| edges[e * 4 + 2] as usize; // m_nOrigin

    // Each face starts at one half-edge (RnFace_t = { u8 m_nEdge }).
    for &start in faces.iter() {
        let start = start as usize;
        if start >= edge_count {
            continue;
        }
        // Collect the face's ordered vertex ring.
        let mut ring = Vec::new();
        let mut e = start;
        for _ in 0..edge_count {
            // guard against a malformed cycle
            let v = origin(e);
            if (v as u32) < nv {
                ring.push(base + v as u32);
            }
            e = next(e);
            if e == start || e >= edge_count {
                break;
            }
        }
        // Fan-triangulate the polygon.
        for i in 1..ring.len().saturating_sub(1) {
            indices.push(ring[0]);
            indices.push(ring[i]);
            indices.push(ring[i + 1]);
        }
    }
}

/// Extract the full world-collision triangle mesh from a decoded PHYS root.
/// Returns (vertices, triangle indices); None if no shapes were found.
pub fn extract_collision_mesh(root: &Value) -> Option<(Vec<[f32; 3]>, Vec<u32>)> {
    let parts = root.get("m_parts")?.as_array()?;
    // Collision-attribute table: indexed by each shape's
    // m_nCollisionAttributeIndex; tells visible world from clip/tool volumes.
    let attrs = root.get("m_collisionAttributes").and_then(|v| v.as_array());
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for part in parts {
        let shape = match part.get("m_rnShape") {
            Some(s) => s,
            None => continue,
        };
        if let Some(meshes) = shape.get("m_meshes").and_then(|v| v.as_array()) {
            for m in meshes {
                if !shape_visible(m, attrs) { continue; }
                if let Some(inner) = m.get("m_Mesh") {
                    add_mesh(inner, &mut verts, &mut indices);
                }
            }
        }
        if let Some(hulls) = shape.get("m_hulls").and_then(|v| v.as_array()) {
            for h in hulls {
                if !shape_visible(h, attrs) { continue; }
                if let Some(inner) = h.get("m_Hull") {
                    add_hull(inner, &mut verts, &mut indices);
                }
            }
        }
    }
    if verts.is_empty() || indices.is_empty() {
        return None;
    }
    let indices = drop_interior_faces(&verts, &indices);
    if indices.is_empty() {
        return None;
    }
    Some((verts, indices))
}

/// Remove faces shared by abutting/overlapping convex hulls. The world solid is
/// a union of brush hulls, so any face on the *interface* between two solids is
/// emitted twice (once per hull, opposite winding) at the exact same place. Those
/// interior faces are invisible in-game and, being coincident, z-fight forever —
/// no depth-buffer trick can separate them. Keying each triangle by its sorted,
/// lightly-quantized corner positions (winding-independent) lets us drop every
/// face that appears more than once, leaving just the exterior shell. Also drops
/// sub-grid-degenerate slivers.
fn drop_interior_faces(verts: &[[f32; 3]], indices: &[u32]) -> Vec<u32> {
    // ~0.25-unit grid: brush vertices snap to integers in Hammer so shared faces
    // match exactly; the quantum only absorbs convex-hull float noise.
    let q = |v: [f32; 3]| -> (i64, i64, i64) {
        ((v[0] * 4.0).round() as i64, (v[1] * 4.0).round() as i64, (v[2] * 4.0).round() as i64)
    };
    let key_of = |t: &[u32]| -> [(i64, i64, i64); 3] {
        let mut k = [q(verts[t[0] as usize]), q(verts[t[1] as usize]), q(verts[t[2] as usize])];
        k.sort_unstable();
        k
    };
    let mut counts: HashMap<[(i64, i64, i64); 3], u32> = HashMap::with_capacity(indices.len() / 3);
    for t in indices.chunks_exact(3) {
        *counts.entry(key_of(t)).or_insert(0) += 1;
    }
    let mut out = Vec::with_capacity(indices.len());
    for t in indices.chunks_exact(3) {
        let k = key_of(t);
        let degenerate = k[0] == k[1] || k[1] == k[2] || k[0] == k[2];
        if !degenerate && counts[&k] == 1 {
            out.extend_from_slice(t);
        }
    }
    out
}
