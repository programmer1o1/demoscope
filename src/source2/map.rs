// CS2 map overlay: turn a map `.vpk` into the same base64 vertex/index buffers
// the BSP overlay path produces, so the viewer renders CS2 world geometry with
// zero template changes.
//
// Pipeline: VPK directory → `maps/<map>/world_physics.vmdl_c` → its PHYS block
// (KV3 v5) → decoded value tree → collision triangle mesh (`vphys`). The result
// is the world *collision* hull — chosen over the render meshes because it lives
// in one file as plain vertex/triangle arrays, sidestepping the meshopt codec
// and the per-instance scene-graph aggregation the render path would require.

use base64::{engine::general_purpose::STANDARD, Engine as _};

use super::kv3::Kv3;
use super::resource::Resource;
use super::vphys;
use super::vpk::Vpk;

/// Extract a CS2 map's world-collision geometry from its `.vpk`. Returns
/// `(verts_b64, idx_b64, n_verts, n_tris)` in the exact LE-f32 / LE-u32 layout
/// the viewer's `__BSP_VERTS__` / `__BSP_IDX__` decoder expects.
pub fn extract_map_geometry(vpk_bytes: &[u8]) -> Option<(String, String, usize, usize)> {
    let vpk = Vpk::parse(vpk_bytes)?;
    // A map pak holds exactly one world_physics resource.
    let entry = vpk.find("world_physics.vmdl_c")?;
    let file = vpk.read(entry)?;
    let res = Resource::parse(&file)?;
    let phys = res.block(b"PHYS")?;
    let kv = Kv3::parse(phys)?;
    let root = kv.root()?;
    let (verts, indices) = vphys::extract_collision_mesh(&root)?;

    let mut v_buf = Vec::with_capacity(verts.len() * 12);
    for &[x, y, z] in &verts {
        v_buf.extend_from_slice(&x.to_le_bytes());
        v_buf.extend_from_slice(&y.to_le_bytes());
        v_buf.extend_from_slice(&z.to_le_bytes());
    }
    let mut i_buf = Vec::with_capacity(indices.len() * 4);
    for &i in &indices {
        i_buf.extend_from_slice(&i.to_le_bytes());
    }
    Some((STANDARD.encode(&v_buf), STANDARD.encode(&i_buf), verts.len(), indices.len() / 3))
}
