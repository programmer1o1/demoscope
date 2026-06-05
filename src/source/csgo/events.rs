// CS:GO protobuf game events.
//
// CS:GO wraps its game events in protobuf — `CSVCMsg_GameEventList` (id 30,
// the descriptor: eventid → name + ordered key names) and `CSVCMsg_GameEvent`
// (id 25, an instance carrying values in descriptor-key order). The bit-packed
// `svc_GameEvent` scanner used for older Source 1 games (`events::extract_*`)
// can't read these, so CS:GO demos otherwise surface no kills/bomb/rounds.
//
// These protos are byte-for-byte identical to Source 2's
// `CMsgSource1LegacyGameEvent*`, so this is a near-verbatim port of the decoder
// in `src/source2/parser.rs` — only the message ids differ.

use std::collections::HashMap;

use super::message::{scan_payload, MsgKind};
use super::super::events::{EventField, EventValue, GameEvent};
use super::super::super::protobuf::Reader;

/// Gameplay-significant events to keep (the stream also carries high-spam events
/// like weapon_fire / player_footstep that would bloat the timeline).
const KEEP_EVENTS: &[&str] = &[
    "player_death", "player_hurt", "player_blind", "player_spawn", "player_team",
    "player_connect", "player_connect_full", "player_disconnect",
    "bomb_planted", "bomb_defused", "bomb_exploded", "bomb_beginplant", "bomb_begindefuse",
    "bomb_dropped", "bomb_pickup", "bomb_abortplant", "bomb_abortdefuse",
    "round_start", "round_end", "round_mvp", "round_freeze_end", "round_officially_ended",
    "round_prestart", "round_poststart", "round_announce_match_start", "cs_win_panel_round",
    "hostage_rescued", "hostage_killed", "hostage_hurt", "hostage_follows",
    "smokegrenade_detonate", "flashbang_detonate", "hegrenade_detonate", "molotov_detonate",
    "inferno_startburn", "decoy_started", "item_pickup", "weapon_zoom",
];

/// Decode CS:GO game events from the signon + game-packet protobuf message
/// streams (the event-list descriptor usually rides in signon). The `packets`
/// are `(tick, start, end)` offsets into `data`, exactly as the entity path
/// consumes them.
pub fn decode_events(
    signon: &[Vec<u8>],
    packets: &[(i32, usize, usize)],
    data: &[u8],
) -> Vec<GameEvent> {
    let mut descriptors: HashMap<i32, (String, Vec<String>)> = HashMap::new();
    let mut out: Vec<GameEvent> = Vec::new();
    for p in signon {
        process(p, 0, &mut descriptors, &mut out);
    }
    for &(tick, start, end) in packets {
        if let Some(p) = data.get(start..end) {
            process(p, tick, &mut descriptors, &mut out);
        }
    }
    out
}

fn process(
    payload: &[u8],
    tick: i32,
    descriptors: &mut HashMap<i32, (String, Vec<String>)>,
    out: &mut Vec<GameEvent>,
) {
    for m in scan_payload(payload) {
        match m.kind {
            MsgKind::SvcGameEventList => parse_list(m.body, descriptors),
            MsgKind::SvcGameEvent => {
                if let Some(ev) = parse_event(m.body, tick, descriptors) {
                    out.push(ev);
                }
            }
            _ => {}
        }
    }
}

/// CSVCMsg_GameEventList: descriptors[](1) { eventid(1), name(2), keys[](3){name(2)} }.
fn parse_list(body: &[u8], descriptors: &mut HashMap<i32, (String, Vec<String>)>) {
    let mut r = Reader::new(body);
    while let Ok(Some(f)) = r.next_field() {
        if f.number == 1 {
            if let Some(d) = f.value.as_bytes() {
                let mut eventid = 0i32;
                let mut name = String::new();
                let mut keys: Vec<String> = Vec::new();
                let mut dr = Reader::new(d);
                while let Ok(Some(ff)) = dr.next_field() {
                    match ff.number {
                        1 => eventid = ff.value.as_i32().unwrap_or(0),
                        2 => name = ff.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
                        3 => {
                            if let Some(kb) = ff.value.as_bytes() {
                                let mut kr = Reader::new(kb);
                                let mut kn = String::new();
                                while let Ok(Some(kf)) = kr.next_field() {
                                    if kf.number == 2 {
                                        kn = kf.value.as_str().map(|s| s.into_owned()).unwrap_or_default();
                                    }
                                }
                                keys.push(kn);
                            }
                        }
                        _ => {}
                    }
                }
                descriptors.insert(eventid, (name, keys));
            }
        }
    }
}

/// CSVCMsg_GameEvent: event_name(1), eventid(2), keys[](3). Values are zipped
/// against the descriptor key names; kept only if in KEEP_EVENTS.
fn parse_event(
    body: &[u8],
    tick: i32,
    descriptors: &HashMap<i32, (String, Vec<String>)>,
) -> Option<GameEvent> {
    let mut eventid = 0i32;
    let mut event_name = String::new();
    let mut values: Vec<EventValue> = Vec::new();
    let mut r = Reader::new(body);
    while let Ok(Some(f)) = r.next_field() {
        match f.number {
            1 => event_name = f.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
            2 => eventid = f.value.as_i32().unwrap_or(0),
            3 => {
                if let Some(kb) = f.value.as_bytes() {
                    values.push(decode_value(kb));
                }
            }
            _ => {}
        }
    }

    let (name, keys) = match descriptors.get(&eventid) {
        Some((n, k)) => (n.clone(), k.clone()),
        None => (event_name, Vec::new()),
    };
    if !KEEP_EVENTS.contains(&name.as_str()) {
        return None;
    }
    let fields = keys
        .into_iter()
        .zip(values.into_iter())
        .map(|(name, value)| EventField { name, value })
        .collect();
    Some(GameEvent { event: name, tick, fields })
}

/// One CSVCMsg_GameEvent.key_t → EventValue (only the populated val_* is sent).
fn decode_value(body: &[u8]) -> EventValue {
    let mut r = Reader::new(body);
    let mut v = EventValue::Null;
    while let Ok(Some(f)) = r.next_field() {
        match f.number {
            2 => v = EventValue::Str(f.value.as_str().map(|s| s.into_owned()).unwrap_or_default()),
            3 => v = EventValue::Float(f.value.as_f32().unwrap_or(0.0)),
            4 | 5 | 6 => v = EventValue::Int(f.value.as_i32().unwrap_or(0)),
            7 => v = EventValue::Bool(f.value.as_bool().unwrap_or(false)),
            8 => v = EventValue::Int(f.value.as_u64().unwrap_or(0) as i32),
            _ => {}
        }
    }
    v
}
