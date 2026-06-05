// Zero-dependency Snappy block-format decompressor.
//
// Source 2 frames whose command byte has the compression bit set (see
// `container`) carry a Snappy-compressed body. Snappy's *block* format (not the
// framed/streaming format) is what the demo uses: a varint uncompressed length,
// then a sequence of literal / copy elements. Spec:
//   https://github.com/google/snappy/blob/main/format_description.txt
//
// This is the only compression Source 2 demos need; fully specified, no
// heuristics. Kept dependency-free to preserve demoscope's single-binary build.

/// Read a little-endian base-128 varint at `*pos`, advancing it. Returns `None`
/// on truncation or a varint wider than 32 bits (Snappy lengths are u32).
fn read_varint(src: &[u8], pos: &mut usize) -> Option<u32> {
    let mut result: u32 = 0;
    let mut shift: u32 = 0;
    loop {
        let byte = *src.get(*pos)?;
        *pos += 1;
        if shift >= 32 {
            return None; // wider than a u32 — malformed
        }
        result |= ((byte & 0x7f) as u32).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
    }
}

/// Decompress one Snappy block. Returns `None` on any malformed/overrunning
/// tag (caller skips the frame rather than aborting the demo).
pub fn decompress(src: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 0usize;
    let expected = read_varint(src, &mut pos)? as usize;
    let mut out: Vec<u8> = Vec::with_capacity(expected.min(1 << 26)); // cap preallocation

    while pos < src.len() {
        let tag = src[pos];
        pos += 1;
        match tag & 0x03 {
            // Literal: length-1 in the upper 6 bits, or in 1-4 trailing bytes.
            0x00 => {
                let mut len = (tag >> 2) as usize;
                if len >= 60 {
                    // 60..=63 → (len-59) little-endian bytes hold the real length-1.
                    let extra = len - 59;
                    let mut real = 0usize;
                    for i in 0..extra {
                        real |= (*src.get(pos + i)? as usize) << (8 * i);
                    }
                    pos += extra;
                    len = real;
                }
                len += 1;
                let end = pos.checked_add(len)?;
                out.extend_from_slice(src.get(pos..end)?);
                pos = end;
            }
            // Copy, 1-byte offset: 3-bit (length-4) + 11-bit offset.
            0x01 => {
                let len = (((tag >> 2) & 0x07) as usize) + 4;
                let off_hi = (tag >> 5) as usize;
                let off_lo = *src.get(pos)? as usize;
                pos += 1;
                copy(&mut out, (off_hi << 8) | off_lo, len)?;
            }
            // Copy, 2-byte offset: (length-1) + 16-bit LE offset.
            0x02 => {
                let len = ((tag >> 2) as usize) + 1;
                let off = u16::from_le_bytes(src.get(pos..pos + 2)?.try_into().ok()?) as usize;
                pos += 2;
                copy(&mut out, off, len)?;
            }
            // Copy, 4-byte offset: (length-1) + 32-bit LE offset.
            _ => {
                let len = ((tag >> 2) as usize) + 1;
                let off = u32::from_le_bytes(src.get(pos..pos + 4)?.try_into().ok()?) as usize;
                pos += 4;
                copy(&mut out, off, len)?;
            }
        }
    }

    // A correct stream reproduces exactly the promised length.
    if out.len() == expected {
        Some(out)
    } else {
        None
    }
}

/// Append `len` bytes copied from `offset` back in the already-emitted output.
/// Byte-by-byte so overlapping copies (offset < len, run-length style) work.
fn copy(out: &mut Vec<u8>, offset: usize, len: usize) -> Option<()> {
    if offset == 0 || offset > out.len() {
        return None;
    }
    let start = out.len() - offset;
    for i in 0..len {
        let b = out[start + i];
        out.push(b);
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A bare literal: varint(len) + tag(literal, len-1 in high bits) + bytes.
    #[test]
    fn single_literal() {
        // 5 bytes "hello": uncompressed len 5; literal tag = (4 << 2) | 0 = 0x10.
        let block = [0x05, 0x10, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(decompress(&block).unwrap(), b"hello");
    }

    // A copy that references earlier output, including an overlapping run.
    #[test]
    fn literal_then_overlapping_copy() {
        // Want "ababab" (6 bytes). Emit literal "ab", then copy len=4 off=2.
        // literal: tag (1<<2)|0 = 0x04, bytes "ab".
        // copy 1-byte off: len-4=0 → tag bits (0<<2)|1 = 0x01, offset 2 → byte 0x02.
        let block = [0x06, 0x04, b'a', b'b', 0x01, 0x02];
        assert_eq!(decompress(&block).unwrap(), b"ababab");
    }

    // Length-1 spilled into a trailing byte (literal of 64 bytes needs the
    // 60-escape: tag high bits = 60 → one extra byte holds length-1 = 63).
    #[test]
    fn long_literal_escape() {
        let payload: Vec<u8> = (0..64u8).collect();
        let mut block = vec![64u8, (60 << 2) | 0x00, 63];
        block.extend_from_slice(&payload);
        assert_eq!(decompress(&block).unwrap(), payload);
    }

    // Truncated / malformed inputs degrade to None, never panic.
    #[test]
    fn malformed_returns_none() {
        assert_eq!(decompress(&[0x05, 0x10, b'h', b'i']), None); // literal overruns
        assert_eq!(decompress(&[0x04, 0x01, 0x09]), None); // copy offset past start
    }
}
