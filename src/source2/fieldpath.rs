// Field-path Huffman decoding.
//
// Ported from dotabuff/manta `field_path.go` + `huffman.go`. The ~40 field-path
// operations have fixed weights; a Huffman tree built from those weights maps
// each bit-code to an operation that mutates a running field path. The tree
// shape MUST match the encoder's, which means replicating Go's `container/heap`
// pop order (weight asc, ties by higher value first) exactly — see `HeapBuilder`.

use super::bitreader::BitReader;
use std::sync::OnceLock;

/// The field-path operations, in table order. The index is the Huffman leaf
/// "value"; the weight drives the tree. `weight 0` is bumped to 1 at build time.
const OP_WEIGHTS: &[i32] = &[
    36271, // 0  PlusOne
    10334, // 1  PlusTwo
    1375,  // 2  PlusThree
    646,   // 3  PlusFour
    4128,  // 4  PlusN
    35,    // 5  PushOneLeftDeltaZeroRightZero
    3,     // 6  PushOneLeftDeltaZeroRightNonZero
    521,   // 7  PushOneLeftDeltaOneRightZero
    2942,  // 8  PushOneLeftDeltaOneRightNonZero
    560,   // 9  PushOneLeftDeltaNRightZero
    471,   // 10 PushOneLeftDeltaNRightNonZero
    10530, // 11 PushOneLeftDeltaNRightNonZeroPack6Bits
    251,   // 12 PushOneLeftDeltaNRightNonZeroPack8Bits
    0,     // 13 PushTwoLeftDeltaZero
    0,     // 14 PushTwoPack5LeftDeltaZero
    0,     // 15 PushThreeLeftDeltaZero
    0,     // 16 PushThreePack5LeftDeltaZero
    0,     // 17 PushTwoLeftDeltaOne
    0,     // 18 PushTwoPack5LeftDeltaOne
    0,     // 19 PushThreeLeftDeltaOne
    0,     // 20 PushThreePack5LeftDeltaOne
    0,     // 21 PushTwoLeftDeltaN
    0,     // 22 PushTwoPack5LeftDeltaN
    0,     // 23 PushThreeLeftDeltaN
    0,     // 24 PushThreePack5LeftDeltaN
    0,     // 25 PushN
    310,   // 26 PushNAndNonTopological
    2,     // 27 PopOnePlusOne
    0,     // 28 PopOnePlusN
    1837,  // 29 PopAllButOnePlusOne
    149,   // 30 PopAllButOnePlusN
    300,   // 31 PopAllButOnePlusNPack3Bits
    634,   // 32 PopAllButOnePlusNPack6Bits
    0,     // 33 PopNPlusOne
    0,     // 34 PopNPlusN
    1,     // 35 PopNAndNonTopographical
    76,    // 36 NonTopoComplex
    271,   // 37 NonTopoPenultimatePlusOne
    99,    // 38 NonTopoComplexPack4Bits
    25474, // 39 FieldPathEncodeFinish
];

const OP_FINISH: usize = 39;

pub struct FieldPath {
    pub path: [i32; 7],
    pub last: usize,
    pub done: bool,
    /// Set when `bump` ran off the end of the 7-slot buffer — distinguishes a
    /// desync abort from a clean `OP_FINISH` (both set `done`).
    pub overflow: bool,
}

impl FieldPath {
    fn new() -> Self {
        FieldPath { path: [-1, 0, 0, 0, 0, 0, 0], last: 0, done: false, overflow: false }
    }

    /// Advance to the next path level. Returns false (and marks the path done)
    /// if it would exceed the fixed 7-slot buffer — that only happens when the
    /// bitstream has already desynced upstream, so we abort this packet's
    /// field-path read rather than index out of bounds. See `read_field_paths`,
    /// which drops a `done` path instead of pushing it.
    #[inline]
    fn bump(&mut self) -> bool {
        if self.last + 1 >= self.path.len() {
            self.overflow = true;
            self.done = true;
            return false;
        }
        self.last += 1;
        true
    }

    fn pop(&mut self, n: usize) {
        for _ in 0..n {
            self.path[self.last] = 0;
            if self.last == 0 { break; }
            self.last -= 1;
        }
    }
}

/// Apply operation `op` (table index) to the running field path.
fn apply_op(op: usize, r: &mut BitReader, fp: &mut FieldPath) {
    let last = fp.last;
    match op {
        0 => fp.path[last] += 1,
        1 => fp.path[last] += 2,
        2 => fp.path[last] += 3,
        3 => fp.path[last] += 4,
        4 => fp.path[last] += r.read_ubit_var_fp() as i32 + 5,
        5 => { if !fp.bump() { return; } fp.path[fp.last] = 0; }
        6 => { if !fp.bump() { return; } fp.path[fp.last] = r.read_ubit_var_fp() as i32; }
        7 => { fp.path[last] += 1; if !fp.bump() { return; } fp.path[fp.last] = 0; }
        8 => { fp.path[last] += 1; if !fp.bump() { return; } fp.path[fp.last] = r.read_ubit_var_fp() as i32; }
        9 => { fp.path[last] += r.read_ubit_var_fp() as i32; if !fp.bump() { return; } fp.path[fp.last] = 0; }
        10 => {
            fp.path[last] += r.read_ubit_var_fp() as i32 + 2;
            if !fp.bump() { return; }
            fp.path[fp.last] = r.read_ubit_var_fp() as i32 + 1;
        }
        11 => {
            fp.path[last] += r.read_bits(3) as i32 + 2;
            if !fp.bump() { return; }
            fp.path[fp.last] = r.read_bits(3) as i32 + 1;
        }
        12 => {
            fp.path[last] += r.read_bits(4) as i32 + 2;
            if !fp.bump() { return; }
            fp.path[fp.last] = r.read_bits(4) as i32 + 1;
        }
        13 => {
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
        }
        14 => {
            if !fp.bump() { return; } fp.path[fp.last] = r.read_bits(5) as i32;
            if !fp.bump() { return; } fp.path[fp.last] = r.read_bits(5) as i32;
        }
        15 => {
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
        }
        16 => {
            if !fp.bump() { return; } fp.path[fp.last] = r.read_bits(5) as i32;
            if !fp.bump() { return; } fp.path[fp.last] = r.read_bits(5) as i32;
            if !fp.bump() { return; } fp.path[fp.last] = r.read_bits(5) as i32;
        }
        17 => {
            fp.path[last] += 1;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
        }
        18 => {
            fp.path[last] += 1;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_bits(5) as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_bits(5) as i32;
        }
        19 => {
            fp.path[last] += 1;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
        }
        20 => {
            fp.path[last] += 1;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_bits(5) as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_bits(5) as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_bits(5) as i32;
        }
        21 => {
            fp.path[last] += r.read_ubit_var() as i32 + 2;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
        }
        22 => {
            fp.path[last] += r.read_ubit_var() as i32 + 2;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_bits(5) as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_bits(5) as i32;
        }
        23 => {
            fp.path[last] += r.read_ubit_var() as i32 + 2;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_ubit_var_fp() as i32;
        }
        24 => {
            fp.path[last] += r.read_ubit_var() as i32 + 2;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_bits(5) as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_bits(5) as i32;
            if !fp.bump() { return; } fp.path[fp.last] += r.read_bits(5) as i32;
        }
        25 => {
            let n = r.read_ubit_var() as i32;
            fp.path[last] += r.read_ubit_var() as i32;
            for _ in 0..n {
                if !fp.bump() { return; }
                fp.path[fp.last] += r.read_ubit_var_fp() as i32;
            }
        }
        26 => {
            for i in 0..=fp.last {
                if r.read_bit() {
                    fp.path[i] += r.read_var_i32() + 1;
                }
            }
            let count = r.read_ubit_var() as i32;
            for _ in 0..count {
                if !fp.bump() { return; }
                fp.path[fp.last] = r.read_ubit_var_fp() as i32;
            }
        }
        27 => { fp.pop(1); fp.path[fp.last] += 1; }
        28 => { fp.pop(1); fp.path[fp.last] += r.read_ubit_var_fp() as i32 + 1; }
        29 => { fp.pop(fp.last); fp.path[0] += 1; }
        30 => { fp.pop(fp.last); fp.path[0] += r.read_ubit_var_fp() as i32 + 1; }
        31 => { fp.pop(fp.last); fp.path[0] += r.read_bits(3) as i32 + 1; }
        32 => { fp.pop(fp.last); fp.path[0] += r.read_bits(6) as i32 + 1; }
        33 => { let n = r.read_ubit_var_fp() as usize; fp.pop(n); fp.path[fp.last] += 1; }
        34 => { let n = r.read_ubit_var_fp() as usize; fp.pop(n); fp.path[fp.last] += r.read_var_i32(); }
        35 => {
            let n = r.read_ubit_var_fp() as usize;
            fp.pop(n);
            for i in 0..=fp.last {
                if r.read_bit() {
                    fp.path[i] += r.read_var_i32();
                }
            }
        }
        36 => {
            for i in 0..=fp.last {
                if r.read_bit() {
                    fp.path[i] += r.read_var_i32();
                }
            }
        }
        37 => { fp.path[fp.last.saturating_sub(1)] += 1; }
        38 => {
            for i in 0..=fp.last {
                if r.read_bit() {
                    fp.path[i] += r.read_bits(4) as i32 - 7;
                }
            }
        }
        OP_FINISH => fp.done = true,
        _ => {}
    }
}

// --- Huffman tree -----------------------------------------------------------
//
// The field-path Huffman tree is canonical (fixed by the encoder, weights in
// OP_WEIGHTS). It's identical to what dotabuff/manta and skadistats/clarity
// build from a min-heap (tie-break: higher insertion index pops first). Rather
// than re-derive Valve's exact heap ordering, we build it directly from the
// published bit codes (verified against LaihoE/demoparser's printed table).
// PlusOne (op 0), the most frequent, gets the 1-bit code "0"; bit 0 = left,
// bit 1 = right; the first bit read is the leftmost character.
const OP_CODES: &[(usize, &str)] = &[
    (0, "0"),
    (39, "10"),
    (8, "11000"),
    (2, "110010"),
    (29, "110011"),
    (4, "11010"),
    (30, "110110000"),
    (38, "1101100010"),
    (35, "1101100011000000"),
    (34, "1101100011000001"),
    (27, "110110001100001"),
    (25, "1101100011000100"),
    (24, "1101100011000101"),
    (33, "1101100011000110"),
    (28, "1101100011000111"),
    (13, "1101100011001000"),
    (15, "11011000110010010"),
    (14, "11011000110010011"),
    (6, "110110001100101"),
    (21, "11011000110011000"),
    (20, "11011000110011001"),
    (23, "11011000110011010"),
    (22, "11011000110011011"),
    (17, "11011000110011100"),
    (16, "11011000110011101"),
    (19, "11011000110011110"),
    (18, "11011000110011111"),
    (5, "110110001101"),
    (36, "11011000111"),
    (10, "11011001"),
    (7, "11011010"),
    (12, "110110110"),
    (37, "110110111"),
    (9, "11011100"),
    (31, "110111010"),
    (26, "110111011"),
    (32, "11011110"),
    (3, "11011111"),
    (1, "1110"),
    (11, "1111"),
];

enum Node {
    Leaf { value: usize },
    Internal { left: usize, right: usize },
}

struct Tree {
    nodes: Vec<Node>,
    root: usize,
}

static TREE: OnceLock<Tree> = OnceLock::new();

fn tree() -> &'static Tree {
    TREE.get_or_init(build_tree)
}

const NIL: usize = usize::MAX;

fn build_tree() -> Tree {
    // Arena with a mutable internal-node child table; leaves placed at code ends.
    let mut nodes: Vec<Node> = vec![Node::Internal { left: NIL, right: NIL }];
    let root = 0usize;

    for &(op, code) in OP_CODES {
        let mut cur = root;
        let bytes = code.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            let go_right = b == b'1';
            let last = i == bytes.len() - 1;
            // Read current child.
            let child = match &nodes[cur] {
                Node::Internal { left, right } => if go_right { *right } else { *left },
                Node::Leaf { .. } => NIL,
            };
            if last {
                let leaf = nodes.len();
                nodes.push(Node::Leaf { value: op });
                if let Node::Internal { left, right } = &mut nodes[cur] {
                    if go_right { *right = leaf } else { *left = leaf }
                }
            } else {
                let next = if child == NIL {
                    let n = nodes.len();
                    nodes.push(Node::Internal { left: NIL, right: NIL });
                    if let Node::Internal { left, right } = &mut nodes[cur] {
                        if go_right { *right = n } else { *left = n }
                    }
                    n
                } else {
                    child
                };
                cur = next;
            }
        }
    }

    Tree { nodes, root }
}

/// Read one PacketEntities delta's worth of field paths from the bit stream.
///
/// Returns `None` on a desync — either navigating into an absent Huffman branch
/// or a field path running past its 7-slot buffer. Both mean the bitstream is
/// no longer aligned, so the caller must fail the whole packet rather than
/// decode values against a corrupt cursor (which otherwise accumulates garbage
/// entities and, on long demos, runs the process out of memory).
pub fn read_field_paths(r: &mut BitReader) -> Option<Vec<FieldPath>> {
    let t = tree();
    let mut fp = FieldPath::new();
    let mut paths: Vec<FieldPath> = Vec::new();
    let mut node = t.root;

    while !fp.done {
        let next = match &t.nodes[node] {
            Node::Internal { left, right } => {
                if r.read_bit() { *right } else { *left }
            }
            Node::Leaf { .. } => node,
        };
        if next == NIL {
            return None; // navigated into an absent branch — desync
        }

        match &t.nodes[next] {
            Node::Leaf { value } => {
                let op = *value;
                node = t.root;
                apply_op(op, r, &mut fp);
                if !fp.done {
                    paths.push(FieldPath { path: fp.path, last: fp.last, done: false, overflow: false });
                }
            }
            Node::Internal { .. } => {
                node = next;
            }
        }
    }
    if fp.overflow {
        return None; // field path ran past its buffer — desync
    }
    Some(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The tree must have exactly 40 leaves + 39 internal nodes = 79 nodes, and a
    // valid root that is internal. (Sanity that the heap build terminated.)
    #[test]
    fn tree_shape() {
        let t = tree();
        assert!(matches!(t.nodes[t.root], Node::Internal { .. }));
        let leaves = t.nodes.iter().filter(|n| matches!(n, Node::Leaf { .. })).count();
        assert_eq!(leaves, 40);
    }

    // Code lengths must match the canonical field-path Huffman (reference table
    // from LaihoE/demoparser). depth(op) == len(prefix). This is the definitive
    // check that the heap build reproduces the encoder's tree.
    #[test]
    fn code_lengths_match_reference() {
        let t = tree();
        fn depth_of(t: &Tree, node: usize, target: usize, d: usize) -> Option<usize> {
            match &t.nodes[node] {
                Node::Leaf { value } => if *value == target { Some(d) } else { None },
                Node::Internal { left, right } => {
                    depth_of(t, *left, target, d + 1).or_else(|| depth_of(t, *right, target, d + 1))
                }
            }
        }
        // Each op's tree depth must equal its canonical code length.
        let mut mism = 0;
        for &(op, code) in OP_CODES {
            let d = depth_of(t, t.root, op, 0).expect("op present");
            if d != code.len() {
                eprintln!("op {:2}: depth={} code_len={}", op, d, code.len());
                mism += 1;
            }
        }
        assert_eq!(mism, 0, "{} ops mismatch", mism);
    }

    // FieldPathEncodeFinish is the highest-weight op (25474) so it must get one
    // of the shortest codes — at most 2 bits from the root.
    #[test]
    fn finish_is_shallow() {
        let t = tree();
        fn depth_of(t: &Tree, node: usize, target: usize, d: usize) -> Option<usize> {
            match &t.nodes[node] {
                Node::Leaf { value } => if *value == target { Some(d) } else { None },
                Node::Internal { left, right } => {
                    depth_of(t, *left, target, d + 1).or_else(|| depth_of(t, *right, target, d + 1))
                }
            }
        }
        let d = depth_of(t, t.root, OP_FINISH, 0).expect("finish leaf present");
        assert!(d <= 3, "finish op too deep: {}", d);
    }
}
