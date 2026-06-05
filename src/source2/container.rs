// PBDEMS2 outer container walk.
//
// After the 8-byte `PBDEMS2\0` magic come two little-endian i32s — the byte
// offset of the trailing `CDemoFileInfo` frame and of the spawn-groups frame —
// so the fixed header is 16 bytes. The body is then a flat sequence of frames:
//   varint(command)  varint(tick)  varint(size)  <size bytes of body>
// The command's high bit (`DEM_IsCompressed`, value 64) means the body is
// Snappy-compressed; mask it off to get the real command id.
//
// EDemoCommands we care about for positions:
//   DEM_FileHeader   (1)  -> CDemoFileHeader  (map/server/build metadata)  [done]
//   DEM_FileInfo     (2)  -> CDemoFileInfo    (playback time/ticks)        [done]
//   DEM_SendTables   (4)  -> serializer::parse (CSVCMsg_FlattenedSerializer) [todo]
//   DEM_ClassInfo    (5)  -> class id -> serializer name map                 [todo]
//   DEM_StringTables (6)  -> instancebaseline bootstrap (entities)           [todo]
//   DEM_Packet       (7)  -> embedded net messages incl. PacketEntities      [todo]
//   DEM_FullPacket  (13)  -> periodic keyframe, also carries a Packet        [todo]
//
// Bodies are protobuf `CDemo*` messages — decoded with `crate::protobuf::Reader`.

use super::super::protobuf::Reader;
use super::snappy;

/// The compression flag OR'd into the command varint.
pub const DEM_IS_COMPRESSED: u32 = 64;

const CMD_FILE_HEADER: u32 = 1;
const CMD_FILE_INFO: u32 = 2;

/// Demo-level metadata we can surface without the full entity pipeline. Drawn
/// from `CDemoFileHeader` (map/server/build) and `CDemoFileInfo` (duration).
#[derive(Debug, Default, Clone)]
pub struct Source2Meta {
    pub map_name: String,
    pub server_name: String,
    pub client_name: String,
    pub game_directory: String,
    pub demo_version_name: String,
    pub build_num: i32,
    pub network_protocol: i32,
    pub playback_time: f32,
    pub playback_ticks: i32,
    pub playback_frames: i32,
}

/// Parse the fixed header + the `CDemoFileHeader` frame + the trailing
/// `CDemoFileInfo` frame. Returns `None` if the magic/header is malformed; the
/// two protobuf reads degrade field-by-field (missing fields stay default).
pub fn parse_meta(data: &[u8]) -> Option<Source2Meta> {
    if !super::is_source2(data) || data.len() < 16 {
        return None;
    }
    let fileinfo_offset =
        i32::from_le_bytes(data.get(8..12)?.try_into().ok()?) as usize;

    let mut meta = Source2Meta::default();

    // The very first frame (at offset 16) is DEM_FileHeader, uncompressed.
    if let Some((kind, body)) = read_frame_at(data, 16) {
        if kind == CMD_FILE_HEADER {
            decode_file_header(&body, &mut meta);
        }
    }

    // CDemoFileInfo sits at the offset named in the header (end of file).
    if fileinfo_offset >= 16 && fileinfo_offset < data.len() {
        if let Some((kind, body)) = read_frame_at(data, fileinfo_offset) {
            if kind == CMD_FILE_INFO {
                decode_file_info(&body, &mut meta);
            }
        }
    }

    Some(meta)
}

/// Read one frame at `offset`: `(command_without_compress_bit, decoded_body)`.
/// Decompresses the body when the compression bit is set.
fn read_frame_at(data: &[u8], offset: usize) -> Option<(u32, Vec<u8>)> {
    let mut r = Reader::new(data.get(offset..)?);
    let cmd = r.read_varint().ok()? as u32;
    let _tick = r.read_varint().ok()?;
    let size = r.read_varint().ok()? as usize;
    let raw = r.read_bytes(size).ok()?;
    let compressed = cmd & DEM_IS_COMPRESSED != 0;
    let kind = cmd & !DEM_IS_COMPRESSED;
    let body = if compressed { snappy::decompress(raw)? } else { raw.to_vec() };
    Some((kind, body))
}

/// CDemoFileHeader: 2 net_proto, 3 server, 4 client, 5 map, 6 game_dir,
/// 11 demo_version_name, 13 build_num.
fn decode_file_header(body: &[u8], meta: &mut Source2Meta) {
    let mut r = Reader::new(body);
    while let Ok(Some(f)) = r.next_field() {
        match f.number {
            2 => meta.network_protocol = f.value.as_i32().unwrap_or(0),
            3 => meta.server_name = f.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
            4 => meta.client_name = f.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
            5 => meta.map_name = f.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
            6 => meta.game_directory = f.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
            11 => meta.demo_version_name = f.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
            13 => meta.build_num = f.value.as_i32().unwrap_or(0),
            _ => {}
        }
    }
}

/// CDemoFileInfo: 1 playback_time (float), 2 playback_ticks, 3 playback_frames.
fn decode_file_info(body: &[u8], meta: &mut Source2Meta) {
    let mut r = Reader::new(body);
    while let Ok(Some(f)) = r.next_field() {
        match f.number {
            1 => meta.playback_time = f.value.as_f32().unwrap_or(0.0),
            2 => meta.playback_ticks = f.value.as_i32().unwrap_or(0),
            3 => meta.playback_frames = f.value.as_i32().unwrap_or(0),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Entity-decode walk (positions) — not yet implemented. These are the frames
// that feed serializer/fieldpath/entities once those stages land.
// ---------------------------------------------------------------------------

/// Walk every frame, dispatching DEM_SendTables/ClassInfo/StringTables/Packet
/// to the entity pipeline. Currently a stub — `parse_meta` covers the
/// metadata-only path that the HTML viewer uses today.
pub fn walk(_data: &[u8]) {
    todo!("PBDEMS2 entity-decode frame walk")
}

/// Inside a DEM_Packet body: a bit-packed sequence of embedded net messages
/// (`ubitvar(msg_type)` + `varint32(size)` + body). Routes svc_PacketEntities.
pub fn walk_packet(_body: &[u8]) {
    todo!("DEM_Packet embedded message walk")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let mut b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            out.push(b);
            if v == 0 {
                break;
            }
        }
    }

    // One uncompressed frame: varint(cmd) varint(tick) varint(size) body.
    fn frame(cmd: u32, body: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        varint(cmd as u64, &mut f);
        varint(0, &mut f); // tick
        varint(body.len() as u64, &mut f);
        f.extend_from_slice(body);
        f
    }

    // Build a minimal but well-formed PBDEMS2 file and round-trip the metadata.
    #[test]
    fn parse_synthetic_meta() {
        // CDemoFileHeader { map_name(5)="de_dust2", build_num(13)=14000 }
        let mut fh = Vec::new();
        fh.push((5 << 3) | 2); // field 5, Len
        fh.push(8);
        fh.extend_from_slice(b"de_dust2");
        fh.push((13 << 3) | 0); // field 13, Varint
        varint(14000, &mut fh);

        // CDemoFileInfo { playback_time(1)=10.0, playback_ticks(2)=640 }
        let mut fi = Vec::new();
        fi.push((1 << 3) | 5); // field 1, Fixed32 (float)
        fi.extend_from_slice(&10.0f32.to_le_bytes());
        fi.push((2 << 3) | 0); // field 2, Varint
        varint(640, &mut fi);

        let header_frame = frame(CMD_FILE_HEADER, &fh);
        let info_frame = frame(CMD_FILE_INFO, &fi);

        // 16-byte fixed header: magic + fileinfo_offset + spawngroups_offset.
        let fileinfo_offset = (16 + header_frame.len()) as i32;
        let mut data = Vec::new();
        data.extend_from_slice(super::super::PBDEMS2_MAGIC);
        data.extend_from_slice(&fileinfo_offset.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes()); // spawngroups offset (unused)
        data.extend_from_slice(&header_frame);
        data.extend_from_slice(&info_frame);

        let meta = parse_meta(&data).expect("parse_meta");
        assert_eq!(meta.map_name, "de_dust2");
        assert_eq!(meta.build_num, 14000);
        assert_eq!(meta.playback_time, 10.0);
        assert_eq!(meta.playback_ticks, 640);
    }

    // A snappy-compressed FileHeader body still decodes (exercises the
    // compression bit + the snappy module on the container path).
    #[test]
    fn parse_compressed_file_header() {
        // CDemoFileHeader { map_name(5)="de_nuke" }, snappy-compressed as a
        // single literal element so read_frame_at takes the decompress branch.
        let mut fh = Vec::new();
        fh.push((5 << 3) | 2); // field 5, Len
        fh.push(7);
        fh.extend_from_slice(b"de_nuke");

        // Snappy literal block: varint(len) + tag + bytes.
        let mut snap = Vec::new();
        varint(fh.len() as u64, &mut snap);
        snap.push(((fh.len() as u8 - 1) << 2) | 0x00); // literal, len-1 in high bits
        snap.extend_from_slice(&fh);

        let header_frame = frame(CMD_FILE_HEADER | DEM_IS_COMPRESSED, &snap);
        let mut data = Vec::new();
        data.extend_from_slice(super::super::PBDEMS2_MAGIC);
        data.extend_from_slice(&0i32.to_le_bytes()); // no fileinfo
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&header_frame);

        let meta = parse_meta(&data).expect("parse_meta compressed");
        assert_eq!(meta.map_name, "de_nuke");
    }
}
