# Roadmap

Forward-looking work, ranked roughly easy → hard. PRs welcome. For everything
already shipped, see [CHANGELOG.md](CHANGELOG.md).

## Medium (a weekend)

- **Jump / bhop detector** - flag segments where vertical velocity + `IN_JUMP` indicate rocket-jumps or bhops; expose as a filter.

## Hard (a real project)

- **GoldSrc full entity tracks** - the per-player dots for HL1. The recorder POV path already decodes (`src/goldsrc.rs` walks the NetMsg `RefParams`); this is the *other* players. Needs the `delta.lst` field-table decoder (`svc_deltadescription` → apply to `svc_packetentities`) over the NetMsg svc payloads the walker already isolates. Same shape as the Quake 3 netfield/delta logic already ported, except the table is sent on the wire. No protobuf, no SendTable priority-sort.
- **CS:GO / Source 2 entity decode** - CS:GO's DataTables and PacketEntities are protobuf-encoded (and Source 2 / CS2 / Dota 2 are protobuf throughout), so they need a protobuf message reader rather than the bit-packed SendTable path. CS:GO already parses inputs/names/camera; this is the entity-track layer.
- **Side-by-side demo diff** - overlay two demos in the same scene; useful for jump-map comparisons and speedrun analysis.
- **Streaming parse** - mmap or chunked I/O for 1 GB+ STV demos that currently get fully loaded into memory.
- **VPK / VMT material loading** - decode the materials referenced by each face so the BSP overlay shows the actual map textures, not just untextured wireframe (qbyte's SourceImporter shows this is doable without external deps).
