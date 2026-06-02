// Quake 3 Arena demo decoder (.dm_68, protocol 68).
//
// Container: a sequence of [i32 sequence][i32 messageLength][message] blocks;
// messageLength == -1 marks EOF. Unlike Q1/Q2 the message body is a
// Huffman-compressed bitstream (id Tech 3's static-frequency adaptive tree),
// carrying delta-compressed snapshots. Positions come from:
//   - svc_snapshot playerState (the recorder's own origin + viewangles)
//   - svc_snapshot packet entities (other players' pos.trBase + apos.trBase)
//
// All wire details are ported verbatim from the GPL ioquake3 source
// (qcommon/msg.c field tables + delta logic, qcommon/huffman.c tree, and the
// msg_hData[256] static frequency table). Getting any of these even one bit
// wrong desyncs the whole stream, so they are reproduced exactly.

use std::collections::HashMap;
use std::error::Error;

use super::{QuakeDemo, QuakeMeta, TrackBuilder};

// ── Static Huffman (qcommon/huffman.c) ─────────────────────────────────────────
//
// Ported with an index arena replacing C pointers: node references are indices
// into `nodes`, `head` is an index into `ppnodes` (the C node** slots), and the
// ppnode freelist is a LIFO stack of slot indices (matching C's free/get order).

const HMAX: usize = 256;
const NYT: i32 = HMAX as i32; // not yet transmitted
const INTERNAL_NODE: i32 = HMAX as i32 + 1;
const NIL: usize = usize::MAX;

#[derive(Clone, Copy)]
struct Node {
    left: usize,
    right: usize,
    parent: usize,
    next: usize,
    prev: usize,
    head: usize, // index into ppnodes
    weight: i32,
    symbol: i32,
}

impl Default for Node {
    fn default() -> Self {
        Node { left: NIL, right: NIL, parent: NIL, next: NIL, prev: NIL, head: NIL, weight: 0, symbol: 0 }
    }
}

struct Huff {
    nodes: Vec<Node>,
    bloc_node: usize,
    ppnodes: Vec<usize>, // each slot holds a node index (C: node_t**)
    bloc_ptrs: usize,
    freelist: Vec<usize>,
    tree: usize,
    lhead: usize,
    loc: [usize; HMAX + 2], // indexed by symbol 0..255 and NYT(256)
}

impl Huff {
    fn build() -> Huff {
        let mut h = Huff {
            nodes: vec![Node::default(); 1024],
            bloc_node: 0,
            ppnodes: vec![NIL; 1024],
            bloc_ptrs: 0,
            freelist: Vec::new(),
            tree: NIL,
            lhead: NIL,
            loc: [NIL; HMAX + 2],
        };
        // Huff_Init (decompressor): seed with the NYT node.
        let n = h.bloc_node;
        h.bloc_node += 1;
        h.nodes[n].symbol = NYT;
        h.nodes[n].weight = 0;
        h.nodes[n].next = NIL;
        h.nodes[n].prev = NIL;
        h.nodes[n].parent = NIL;
        h.nodes[n].left = NIL;
        h.nodes[n].right = NIL;
        h.tree = n;
        h.lhead = n;
        h.loc[NYT as usize] = n;

        // Build the fixed tree by replaying the static frequencies.
        for (sym, &freq) in MSG_HDATA.iter().enumerate() {
            for _ in 0..freq {
                h.add_ref(sym as u8);
            }
        }
        h
    }

    fn get_ppnode(&mut self) -> usize {
        if let Some(idx) = self.freelist.pop() {
            idx
        } else {
            let idx = self.bloc_ptrs;
            self.bloc_ptrs += 1;
            if idx >= self.ppnodes.len() {
                self.ppnodes.resize(self.ppnodes.len() * 2, NIL);
            }
            idx
        }
    }
    fn free_ppnode(&mut self, idx: usize) {
        self.freelist.push(idx);
    }

    fn swap(&mut self, n1: usize, n2: usize) {
        let par1 = self.nodes[n1].parent;
        let par2 = self.nodes[n2].parent;
        if par1 != NIL {
            if self.nodes[par1].left == n1 { self.nodes[par1].left = n2; } else { self.nodes[par1].right = n2; }
        } else {
            self.tree = n2;
        }
        if par2 != NIL {
            if self.nodes[par2].left == n2 { self.nodes[par2].left = n1; } else { self.nodes[par2].right = n1; }
        } else {
            self.tree = n1;
        }
        self.nodes[n1].parent = par2;
        self.nodes[n2].parent = par1;
    }

    fn swaplist(&mut self, n1: usize, n2: usize) {
        let t = self.nodes[n1].next;
        self.nodes[n1].next = self.nodes[n2].next;
        self.nodes[n2].next = t;
        let t = self.nodes[n1].prev;
        self.nodes[n1].prev = self.nodes[n2].prev;
        self.nodes[n2].prev = t;
        if self.nodes[n1].next == n1 { self.nodes[n1].next = n2; }
        if self.nodes[n2].next == n2 { self.nodes[n2].next = n1; }
        let nn = self.nodes[n1].next;
        if nn != NIL { self.nodes[nn].prev = n1; }
        let nn = self.nodes[n2].next;
        if nn != NIL { self.nodes[nn].prev = n2; }
        let pp = self.nodes[n1].prev;
        if pp != NIL { self.nodes[pp].next = n1; }
        let pp = self.nodes[n2].prev;
        if pp != NIL { self.nodes[pp].next = n2; }
    }

    fn increment(&mut self, node: usize) {
        if node == NIL { return; }
        let nxt = self.nodes[node].next;
        if nxt != NIL && self.nodes[nxt].weight == self.nodes[node].weight {
            let lnode = self.ppnodes[self.nodes[node].head];
            if lnode != self.nodes[node].parent {
                self.swap(lnode, node);
            }
            self.swaplist(lnode, node);
        }
        let prv = self.nodes[node].prev;
        if prv != NIL && self.nodes[prv].weight == self.nodes[node].weight {
            self.ppnodes[self.nodes[node].head] = prv;
        } else {
            let head = self.nodes[node].head;
            self.ppnodes[head] = NIL;
            self.free_ppnode(head);
        }
        self.nodes[node].weight += 1;
        let nxt = self.nodes[node].next;
        if nxt != NIL && self.nodes[nxt].weight == self.nodes[node].weight {
            self.nodes[node].head = self.nodes[nxt].head;
        } else {
            let pp = self.get_ppnode();
            self.nodes[node].head = pp;
            self.ppnodes[pp] = node;
        }
        let parent = self.nodes[node].parent;
        if parent != NIL {
            self.increment(parent);
            if self.nodes[node].prev == parent {
                self.swaplist(node, parent);
                if self.ppnodes[self.nodes[node].head] == node {
                    self.ppnodes[self.nodes[node].head] = parent;
                }
            }
        }
    }

    fn add_ref(&mut self, ch: u8) {
        if self.loc[ch as usize] == NIL {
            let tnode = self.bloc_node;
            self.bloc_node += 1;
            let tnode2 = self.bloc_node;
            self.bloc_node += 1;

            let lhead = self.lhead;
            let lhead_next = self.nodes[lhead].next;

            // tnode2 = new internal node
            self.nodes[tnode2].symbol = INTERNAL_NODE;
            self.nodes[tnode2].weight = 1;
            self.nodes[tnode2].next = lhead_next;
            if lhead_next != NIL {
                self.nodes[lhead_next].prev = tnode2;
                if self.nodes[lhead_next].weight == 1 {
                    self.nodes[tnode2].head = self.nodes[lhead_next].head;
                } else {
                    let pp = self.get_ppnode();
                    self.nodes[tnode2].head = pp;
                    self.ppnodes[pp] = tnode2;
                }
            } else {
                let pp = self.get_ppnode();
                self.nodes[tnode2].head = pp;
                self.ppnodes[pp] = tnode2;
            }
            self.nodes[lhead].next = tnode2;
            self.nodes[tnode2].prev = lhead;

            // tnode = new leaf for ch
            let lhead_next = self.nodes[lhead].next; // now tnode2
            self.nodes[tnode].symbol = ch as i32;
            self.nodes[tnode].weight = 1;
            self.nodes[tnode].next = lhead_next;
            if lhead_next != NIL {
                self.nodes[lhead_next].prev = tnode;
                if self.nodes[lhead_next].weight == 1 {
                    self.nodes[tnode].head = self.nodes[lhead_next].head;
                } else {
                    // "this should never happen"
                    let pp = self.get_ppnode();
                    self.nodes[tnode].head = pp;
                    self.ppnodes[pp] = tnode2;
                }
            } else {
                let pp = self.get_ppnode();
                self.nodes[tnode].head = pp;
                self.ppnodes[pp] = tnode;
            }
            self.nodes[lhead].next = tnode;
            self.nodes[tnode].prev = lhead;
            self.nodes[tnode].left = NIL;
            self.nodes[tnode].right = NIL;

            let lhead_parent = self.nodes[lhead].parent;
            if lhead_parent != NIL {
                if self.nodes[lhead_parent].left == lhead {
                    self.nodes[lhead_parent].left = tnode2;
                } else {
                    self.nodes[lhead_parent].right = tnode2;
                }
            } else {
                self.tree = tnode2;
            }

            self.nodes[tnode2].right = tnode;
            self.nodes[tnode2].left = lhead;
            self.nodes[tnode2].parent = lhead_parent;
            self.nodes[lhead].parent = tnode2;
            self.nodes[tnode].parent = tnode2;

            self.loc[ch as usize] = tnode;

            let p = self.nodes[tnode2].parent;
            self.increment(p);
        } else {
            let node = self.loc[ch as usize];
            self.increment(node);
        }
    }

    /// Decode one byte (Huff_offsetReceive): walk from root by raw bits to a leaf.
    fn offset_receive(&self, data: &[u8], bit: &mut usize, maxoffset: usize) -> i32 {
        let mut node = self.tree;
        while node != NIL && self.nodes[node].symbol == INTERNAL_NODE {
            if *bit >= maxoffset {
                *bit = maxoffset + 1;
                return 0;
            }
            let b = get_bit(data, bit);
            node = if b != 0 { self.nodes[node].right } else { self.nodes[node].left };
        }
        if node == NIL { return 0; }
        self.nodes[node].symbol
    }
}

#[inline]
fn get_bit(data: &[u8], bit: &mut usize) -> u32 {
    let idx = *bit >> 3;
    let t = if idx < data.len() { (data[idx] >> (*bit & 7)) & 1 } else { 0 };
    *bit += 1;
    t as u32
}

// ── Bit-level message reader (qcommon/msg.c, oob = false) ──────────────────────

struct Msg<'a> {
    data: &'a [u8],
    bit: usize,
    cursize_bits: usize,
    huff: &'a Huff,
    overflow: bool,
}

impl<'a> Msg<'a> {
    fn new(data: &'a [u8], huff: &'a Huff) -> Self {
        Msg { data, bit: 0, cursize_bits: data.len() * 8, huff, overflow: false }
    }

    fn read_bits(&mut self, bits: i32) -> i32 {
        let mut value: u32 = 0;
        let sgn = bits < 0;
        let mut bits = bits.unsigned_abs() as usize;

        let nbits = bits & 7;
        if nbits != 0 {
            if self.bit + nbits > self.cursize_bits {
                self.overflow = true;
                return 0;
            }
            for i in 0..nbits {
                value |= get_bit(self.data, &mut self.bit) << i;
            }
            bits -= nbits;
        }
        if bits != 0 {
            let mut i = 0;
            while i < bits {
                let get = self.huff.offset_receive(self.data, &mut self.bit, self.cursize_bits) as u32;
                value |= get << (i + nbits);
                i += 8;
                if self.bit > self.cursize_bits {
                    self.overflow = true;
                    return 0;
                }
            }
        }

        let mut value = value as i32;
        let total = nbits + bits;
        if sgn && total > 0 && total < 32 {
            if value & (1 << (total - 1)) != 0 {
                value |= -1i32 ^ ((1i32 << total) - 1);
            }
        }
        value
    }

    fn read_byte(&mut self) -> i32 {
        let c = self.read_bits(8) & 0xff;
        if self.overflow { -1 } else { c }
    }
    fn read_short(&mut self) -> i32 {
        self.read_bits(16) as i16 as i32
    }
    fn read_long(&mut self) -> i32 {
        self.read_bits(32)
    }
    fn read_string(&mut self) -> String {
        let mut out = Vec::new();
        loop {
            let c = self.read_byte();
            if c == -1 || c == 0 {
                break;
            }
            // Q3 sanitises high-bit and '%' to '.'.
            let ch = if c > 127 || c == b'%' as i32 { b'.' } else { c as u8 };
            out.push(ch);
            if out.len() > 8192 {
                break;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

// ── svc ops + field tables ─────────────────────────────────────────────────────

const SVC_NOP: i32 = 1;
const SVC_GAMESTATE: i32 = 2;
const SVC_CONFIGSTRING: i32 = 3;
const SVC_BASELINE: i32 = 4;
const SVC_SERVERCOMMAND: i32 = 5;
const SVC_DOWNLOAD: i32 = 6;
const SVC_SNAPSHOT: i32 = 7;
const SVC_EOF: i32 = 8;

const GENTITYNUM_BITS: i32 = 10;
const ENTITYNUM_NONE: i32 = (1 << GENTITYNUM_BITS) - 1; // 1023
const FLOAT_INT_BITS: i32 = 13;
const FLOAT_INT_BIAS: i32 = 1 << (FLOAT_INT_BITS - 1); // 4096

const CS_SERVERINFO: usize = 0;
const CS_PLAYERS: usize = 544;
const MAX_CLIENTS: usize = 64;

const ET_PLAYER: u32 = 1;
const EF_DEAD: u32 = 0x0000_0001; // eFlags bit: player is dead
const PM_DEAD: u32 = 3; // pmtype_t: dead
const TEAM_SPECTATOR: i32 = 3; // team_t

// entityStateFields bit widths, in wire order (msg.c).
const ENT_BITS: [i32; 51] = [
    32, 0, 0, 0, 0, 0, 0, 0, 0, 10, 0, 8, 8, 8, 8, 10, 8, 19, 10, 8, 8, 0, 32, 8, 0, 0, 0, 24, 16,
    8, 10, 8, 8, 0, 0, 0, 8, 0, 32, 32, 32, 0, 0, 0, 0, 32, 0, 0, 0, 32, 16,
];
// Field indices we care about (entity).
const ENT_TRBASE0: usize = 1;
const ENT_TRBASE1: usize = 2;
const ENT_TRBASE2: usize = 5;
const ENT_APOS1: usize = 6; // yaw
const ENT_APOS0: usize = 8; // pitch
const ENT_ETYPE: usize = 11;
const ENT_EFLAGS: usize = 17;

// playerStateFields bit widths, in wire order (msg.c). Negative = signed.
const PS_BITS: [i32; 48] = [
    32, 0, 0, 8, 0, 0, 0, 0, -16, 0, 0, 8, -16, 16, 8, 4, 8, 8, 8, 16, 10, 4, 16, 10, 16, 16, 16,
    8, -8, 8, 8, 8, 8, 8, 8, 16, 16, 12, 8, 8, 8, 5, 0, 0, 0, 0, 10, 16,
];
const PS_ORIGIN0: usize = 1;
const PS_ORIGIN1: usize = 2;
const PS_ORIGIN2: usize = 9;
const PS_VIEWANGLE1: usize = 6; // yaw
const PS_VIEWANGLE0: usize = 7; // pitch
const PS_PM_TYPE: usize = 34;

// ── Delta decoders ─────────────────────────────────────────────────────────────

/// MSG_ReadDeltaEntity. Returns None if the entity was removed.
fn read_delta_entity(m: &mut Msg, from: &[u32; 51]) -> Option<[u32; 51]> {
    if m.read_bits(1) == 1 {
        return None; // remove
    }
    let mut to = *from;
    if m.read_bits(1) == 0 {
        return Some(to); // no delta
    }
    let lc = m.read_byte();
    if lc < 0 || lc as usize > ENT_BITS.len() {
        m.overflow = true;
        return Some(to);
    }
    for i in 0..lc as usize {
        if m.read_bits(1) == 0 {
            continue; // unchanged (already copied)
        }
        let bits = ENT_BITS[i];
        if bits == 0 {
            // float
            if m.read_bits(1) == 0 {
                to[i] = 0.0f32.to_bits();
            } else if m.read_bits(1) == 0 {
                let trunc = m.read_bits(FLOAT_INT_BITS) - FLOAT_INT_BIAS;
                to[i] = (trunc as f32).to_bits();
            } else {
                to[i] = m.read_bits(32) as u32;
            }
        } else {
            if m.read_bits(1) == 0 {
                to[i] = 0;
            } else {
                to[i] = m.read_bits(bits) as u32;
            }
        }
    }
    Some(to)
}

/// MSG_ReadDeltaPlayerstate (in place; `ps` is both from and to).
fn read_delta_playerstate(m: &mut Msg, ps: &mut [u32; 48]) {
    let lc = m.read_byte();
    if lc < 0 || lc as usize > PS_BITS.len() {
        m.overflow = true;
        return;
    }
    for i in 0..lc as usize {
        if m.read_bits(1) == 0 {
            continue; // unchanged
        }
        let bits = PS_BITS[i];
        if bits == 0 {
            // float (no zero-shortcut, unlike entities)
            if m.read_bits(1) == 0 {
                let trunc = m.read_bits(FLOAT_INT_BITS) - FLOAT_INT_BIAS;
                ps[i] = (trunc as f32).to_bits();
            } else {
                ps[i] = m.read_bits(32) as u32;
            }
        } else {
            ps[i] = m.read_bits(bits) as u32;
        }
    }
    // Arrays: stats / persistant / ammo (16-bit each) + powerups (32-bit each).
    if m.read_bits(1) != 0 {
        if m.read_bits(1) != 0 {
            let bits = m.read_bits(16);
            for i in 0..16 {
                if bits & (1 << i) != 0 { m.read_short(); }
            }
        }
        if m.read_bits(1) != 0 {
            let bits = m.read_bits(16);
            for i in 0..16 {
                if bits & (1 << i) != 0 { m.read_short(); }
            }
        }
        if m.read_bits(1) != 0 {
            let bits = m.read_bits(16);
            for i in 0..16 {
                if bits & (1 << i) != 0 { m.read_short(); }
            }
        }
        if m.read_bits(1) != 0 {
            let bits = m.read_bits(16);
            for i in 0..16 {
                if bits & (1 << i) != 0 { m.read_long(); }
            }
        }
    }
}

// ── Infostring helper ──────────────────────────────────────────────────────────

fn info_value(info: &str, key: &str) -> String {
    // "\k1\v1\k2\v2..." - keys and values are backslash-separated.
    let parts: Vec<&str> = info.trim_start_matches('\\').split('\\').collect();
    let mut i = 0;
    while i + 1 < parts.len() {
        if parts[i].eq_ignore_ascii_case(key) {
            return parts[i + 1].to_string();
        }
        i += 2;
    }
    String::new()
}

// ── Top-level parse ─────────────────────────────────────────────────────────────

pub fn parse(data: &[u8], name: &str) -> Result<QuakeDemo, Box<dyn Error>> {
    let dbg = std::env::var("DUMP_QUAKE").is_ok();
    let protocol = super::dm_protocol(name).unwrap_or(68);
    if !(66..=68).contains(&protocol) {
        eprintln!(
            "  [Quake3] WARNING: .dm_{protocol} is not Quake 3 1.32 (protocol 68). This is \
             likely Quake Live or a variant whose entityState/playerState field tables differ \
             from Q3 - the decode will probably desync (few/no samples). Only 66-68 are supported."
        );
    }
    let huff = Huff::build();

    let mut tb = TrackBuilder::default();
    let mut configstrings: HashMap<usize, String> = HashMap::new();
    let mut baselines: HashMap<u16, [u32; 51]> = HashMap::new();
    let mut ents: HashMap<u16, [u32; 51]> = HashMap::new();
    let mut playerstate = [0u32; 48];
    let mut client_num: i32 = -1;
    let mut base_time: Option<i32> = None;
    let mut max_tick: i32 = 0;
    let mut nsnaps: usize = 0;

    // Demo container walk.
    let mut pos = 0usize;
    loop {
        if pos + 8 > data.len() {
            break;
        }
        let _seq = i32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        let len = i32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]);
        pos += 8;
        if len < 0 || len == -1 {
            break;
        }
        let len = len as usize;
        if pos + len > data.len() {
            break;
        }
        let msg_bytes = &data[pos..pos + len];
        pos += len;

        let mut m = Msg::new(msg_bytes, &huff);
        let _reliable_ack = m.read_long();

        while !m.overflow {
            let cmd = m.read_byte();
            if cmd == -1 || cmd == SVC_EOF {
                break;
            }
            match cmd {
                SVC_NOP => {}
                SVC_SERVERCOMMAND => {
                    let _seq = m.read_long();
                    let s = m.read_string();
                    handle_server_command(&s, &mut configstrings);
                }
                SVC_GAMESTATE => {
                    parse_gamestate(&mut m, &mut configstrings, &mut baselines, &mut client_num, dbg);
                }
                SVC_SNAPSHOT => {
                    parse_snapshot(
                        &mut m,
                        &baselines,
                        &mut ents,
                        &mut playerstate,
                        client_num,
                        &mut base_time,
                        &mut max_tick,
                        &mut tb,
                    );
                    nsnaps += 1;
                }
                SVC_DOWNLOAD => {
                    let block = m.read_short();
                    if block != 0 {
                        let size = m.read_short();
                        if size > 0 {
                            for _ in 0..size {
                                m.read_byte();
                            }
                        }
                    }
                }
                _ => break, // unknown / illegible
            }
        }
    }

    // Map + player names from configstrings.
    let serverinfo = configstrings.get(&CS_SERVERINFO).cloned().unwrap_or_default();
    let map = info_value(&serverinfo, "mapname");
    let server = info_value(&serverinfo, "sv_hostname");
    for i in 0..MAX_CLIENTS {
        if let Some(cs) = configstrings.get(&(CS_PLAYERS + i)) {
            if cs.is_empty() {
                continue;
            }
            let name = info_value(cs, "n");
            if !name.is_empty() {
                tb.name(i as u32, name);
            }
            // Players on the spectator team are hidden (they have no body, but
            // some mods still send a free-fly entity that would otherwise show).
            if info_value(cs, "t").trim().parse::<i32>() == Ok(TEAM_SPECTATOR) {
                tb.observe(i as u32, 0, true);
            }
        }
    }
    if client_num >= 0 {
        tb.primary = Some(client_num as u32);
    }

    let tick_rate = 1000.0; // ticks are serverTime in ms
    let duration = max_tick as f32 / tick_rate;
    let total_samples: usize = tb.tracks.values().map(|v| v.len()).sum();
    eprintln!(
        "  [Quake3] map={} players={} entities={} samples={} snapshots={} primary={}",
        map,
        tb.names.len(),
        tb.tracks.len(),
        total_samples,
        nsnaps,
        client_num
    );

    let meta = QuakeMeta {
        map: map.clone(),
        server,
        client: tb.primary.and_then(|p| tb.names.get(&p).cloned()).unwrap_or_default(),
        game: "quake3".to_string(),
        protocol,
        duration,
        tick_rate,
        ncmds: nsnaps,
    };
    let mpd = tb.build(map, meta.server.clone(), duration, max_tick);
    Ok(QuakeDemo { meta, mpd })
}

fn handle_server_command(s: &str, configstrings: &mut HashMap<usize, String>) {
    // "cs <index> \"<infostring>\"" updates a configstring mid-game (renames etc.).
    let mut it = s.splitn(3, ' ');
    if it.next() != Some("cs") {
        return;
    }
    let idx: usize = match it.next().and_then(|t| t.parse().ok()) {
        Some(i) => i,
        None => return,
    };
    let rest = it.next().unwrap_or("");
    let val = rest.trim().trim_matches('"').to_string();
    configstrings.insert(idx, val);
}

fn parse_gamestate(
    m: &mut Msg,
    configstrings: &mut HashMap<usize, String>,
    baselines: &mut HashMap<u16, [u32; 51]>,
    client_num: &mut i32,
    dbg: bool,
) {
    let _server_command_seq = m.read_long();
    loop {
        if m.overflow {
            return;
        }
        let cmd = m.read_byte();
        if cmd == SVC_EOF || cmd == -1 {
            break;
        }
        match cmd {
            SVC_CONFIGSTRING => {
                let idx = m.read_short();
                let s = m.read_string();
                if idx >= 0 {
                    configstrings.insert(idx as usize, s);
                }
            }
            SVC_BASELINE => {
                let newnum = m.read_bits(GENTITYNUM_BITS);
                let null = [0u32; 51];
                if let Some(st) = read_delta_entity(m, &null) {
                    if (0..=ENTITYNUM_NONE).contains(&newnum) {
                        baselines.insert(newnum as u16, st);
                    }
                }
            }
            _ => break,
        }
    }
    *client_num = m.read_long();
    let _checksum_feed = m.read_long();
    if dbg {
        eprintln!("[q3] gamestate: clientNum={client_num} configstrings={} baselines={}", configstrings.len(), baselines.len());
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_snapshot(
    m: &mut Msg,
    baselines: &HashMap<u16, [u32; 51]>,
    ents: &mut HashMap<u16, [u32; 51]>,
    playerstate: &mut [u32; 48],
    client_num: i32,
    base_time: &mut Option<i32>,
    max_tick: &mut i32,
    tb: &mut TrackBuilder,
) {
    let server_time = m.read_long();
    let _delta_num = m.read_byte();
    let _snap_flags = m.read_byte();
    let arealen = m.read_byte();
    if arealen > 0 {
        for _ in 0..arealen {
            m.read_byte();
        }
    }

    let base = *base_time.get_or_insert(server_time);
    let tick = server_time.saturating_sub(base).max(0);
    *max_tick = (*max_tick).max(tick);

    // Player state (the recorder).
    read_delta_playerstate(m, playerstate);
    if client_num >= 0 {
        let x = f32::from_bits(playerstate[PS_ORIGIN0]);
        let y = f32::from_bits(playerstate[PS_ORIGIN1]);
        let z = f32::from_bits(playerstate[PS_ORIGIN2]);
        let yaw = f32::from_bits(playerstate[PS_VIEWANGLE1]);
        let pitch = f32::from_bits(playerstate[PS_VIEWANGLE0]);
        if x.is_finite() && y.is_finite() && z.is_finite() {
            tb.pos(client_num as u32, tick, x, y, z);
            tb.yaw(client_num as u32, tick, yaw, pitch);
            tb.view_angles.push((tick, pitch, yaw));
        }
        // Hide the recorder's own avatar while dead. Deliberately NOT keyed on
        // PM_SPECTATOR: in-eye/POV spectator demos sit at PM_SPECTATOR the whole
        // time and their camera is exactly what we want to follow, so marking it
        // would wrongly freeze the camera.
        tb.life(client_num as u32, tick, playerstate[PS_PM_TYPE] == PM_DEAD);
    }

    // Packet entities (delta against baseline-or-previous).
    loop {
        if m.overflow {
            break;
        }
        let newnum = m.read_bits(GENTITYNUM_BITS);
        if newnum == ENTITYNUM_NONE {
            break;
        }
        if newnum < 0 || newnum > ENTITYNUM_NONE {
            break;
        }
        let num = newnum as u16;
        let from = ents
            .get(&num)
            .copied()
            .or_else(|| baselines.get(&num).copied())
            .unwrap_or([0u32; 51]);
        match read_delta_entity(m, &from) {
            None => {
                ents.remove(&num);
            }
            Some(to) => {
                let etype = to[ENT_ETYPE];
                ents.insert(num, to);
                if etype == ET_PLAYER && (newnum as usize) < MAX_CLIENTS {
                    // Hide the avatar while EF_DEAD is set (death → respawn).
                    tb.life(newnum as u32, tick, to[ENT_EFLAGS] & EF_DEAD != 0);
                    let x = f32::from_bits(to[ENT_TRBASE0]);
                    let y = f32::from_bits(to[ENT_TRBASE1]);
                    let z = f32::from_bits(to[ENT_TRBASE2]);
                    let yaw = f32::from_bits(to[ENT_APOS1]);
                    let pitch = f32::from_bits(to[ENT_APOS0]);
                    if x.is_finite() && y.is_finite() && z.is_finite() {
                        tb.pos(newnum as u32, tick, x, y, z);
                        tb.yaw(newnum as u32, tick, yaw, pitch);
                    }
                }
            }
        }
    }
}

// The static Huffman frequency table (qcommon/msg.c msg_hData[256]).
#[rustfmt::skip]
const MSG_HDATA: [i32; 256] = [
    250315, 41193, 6292, 7106, 3730, 3750, 6110, 23283, 33317, 6950, 7838, 9714, 9257, 17259, 3949,
    1778, 8288, 1604, 1590, 1663, 1100, 1213, 1238, 1134, 1749, 1059, 1246, 1149, 1273, 4486, 2805,
    3472, 21819, 1159, 1670, 1066, 1043, 1012, 1053, 1070, 1726, 888, 1180, 850, 960, 780, 1752,
    3296, 10630, 4514, 5881, 2685, 4650, 3837, 2093, 1867, 2584, 1949, 1972, 940, 1134, 1788, 1670,
    1206, 5719, 6128, 7222, 6654, 3710, 3795, 1492, 1524, 2215, 1140, 1355, 971, 2180, 1248, 1328,
    1195, 1770, 1078, 1264, 1266, 1168, 965, 1155, 1186, 1347, 1228, 1529, 1600, 2617, 2048, 2546,
    3275, 2410, 3585, 2504, 2800, 2675, 6146, 3663, 2840, 14253, 3164, 2221, 1687, 3208, 2739, 3512,
    4796, 4091, 3515, 5288, 4016, 7937, 6031, 5360, 3924, 4892, 3743, 4566, 4807, 5852, 6400, 6225,
    8291, 23243, 7838, 7073, 8935, 5437, 4483, 3641, 5256, 5312, 5328, 5370, 3492, 2458, 1694, 1821,
    2121, 1916, 1149, 1516, 1367, 1236, 1029, 1258, 1104, 1245, 1006, 1149, 1025, 1241, 952, 1287,
    997, 1713, 1009, 1187, 879, 1099, 929, 1078, 951, 1656, 930, 1153, 1030, 1262, 1062, 1214, 1060,
    1621, 930, 1106, 912, 1034, 892, 1158, 990, 1175, 850, 1121, 903, 1087, 920, 1144, 1056, 3462,
    2240, 4397, 12136, 7758, 1345, 1307, 3278, 1950, 886, 1023, 1112, 1077, 1042, 1061, 1071, 1484,
    1001, 1096, 915, 1052, 995, 1070, 876, 1111, 851, 1059, 805, 1112, 923, 1103, 817, 1899, 1872,
    976, 841, 1127, 956, 1159, 950, 7791, 954, 1289, 933, 1127, 3207, 1020, 927, 1355, 768, 1040,
    745, 952, 805, 1073, 740, 1013, 805, 1008, 796, 996, 1057, 11457, 13504,
];
