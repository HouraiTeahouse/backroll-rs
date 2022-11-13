use super::compression::DecodeError;
use varinteger as varint;

/// Encode a bitfield.
pub fn encode(buf: impl AsRef<[u8]>) -> Vec<u8> {
    let (enc, _) = encode_with_offset(&buf, 0);
    enc
}

/// Encode a bitfield at a specific offset
pub fn encode_with_offset(buf: impl AsRef<[u8]>, offset: usize) -> (Vec<u8>, usize) {
    let buf = buf.as_ref();
    let mut len = 0u64;
    let mut contiguous = false;
    let mut prev_bits = 0;
    let mut noncontiguous_bits = Vec::new();
    let mut enc = Vec::with_capacity(encode_len_with_offset(buf, offset));

    for (i, byte) in buf[offset..].iter().enumerate() {
        if contiguous && *byte == prev_bits {
            len += 1;
            continue;
        } else if contiguous {
            write_contiguous(&mut enc, len, prev_bits);
        }

        if *byte == 0 || *byte == 255 {
            if !contiguous && i > offset {
                write_noncontiguous(&mut enc, &mut noncontiguous_bits);
            }
            len = 1;
            prev_bits = *byte;
            contiguous = true;
        } else if !contiguous {
            noncontiguous_bits.push(*byte);
        } else {
            contiguous = false;
            noncontiguous_bits.push(*byte);
        }
    }

    if contiguous {
        write_contiguous(&mut enc, len, prev_bits);
    } else {
        write_noncontiguous(&mut enc, &mut noncontiguous_bits);
    }

    (enc, buf.len() - offset)
}

/// Writes a value for contiguous data to the encoded bitfield
fn write_contiguous(enc: &mut Vec<u8>, mut len: u64, prev_bits: u8) {
    len <<= 2;
    len += 1;
    if prev_bits == 255 {
        len += 2;
    }
    let mut varint = vec![0u8; varint::length(len)];
    varint::encode(len, &mut varint);
    enc.append(&mut varint);
}

/// Writes a value for noncontiguous data to the encoded bitfield
fn write_noncontiguous(enc: &mut Vec<u8>, noncontiguous_bits: &mut Vec<u8>) {
    let mut len = noncontiguous_bits.len() as u64;
    len <<= 1;
    let mut varint = vec![0u8; varint::length(len)];
    varint::encode(len, &mut varint);
    enc.append(&mut varint);
    enc.append(noncontiguous_bits);
}

/// Returns how many bytes an encoded bitfield will use, starting at a specific offset.
pub fn encode_len_with_offset(buf: impl AsRef<[u8]>, offset: usize) -> usize {
    let buf = buf.as_ref();
    let mut len = 0u64;
    let mut partial_len = 0u64;
    let mut contiguous = false;
    let mut prev_bits = 0;

    for (i, byte) in buf[offset..].iter().enumerate() {
        if contiguous && *byte == prev_bits {
            partial_len += 1;
            continue;
        } else if contiguous {
            len += varint::length(partial_len << 2) as u64;
        }

        if *byte == 0 || *byte == 255 {
            if !contiguous && i > offset {
                len += partial_len;
                len += varint::length(partial_len << 1) as u64;
            }
            partial_len = 1;
            prev_bits = *byte;
            contiguous = true;
        } else if !contiguous {
            partial_len += 1;
        } else {
            partial_len = 1;
            contiguous = false;
        }
    }

    if contiguous {
        len += varint::length(partial_len << 2) as u64;
    } else {
        len += partial_len;
        len += varint::length(partial_len << 1) as u64;
    }

    len as usize
}

/// Decode an encoded bitfield.
pub fn decode(buf: impl AsRef<[u8]>) -> Result<Vec<u8>, DecodeError> {
    let (bitfield, _) = decode_with_offset(&buf, 0)?;
    Ok(bitfield)
}

/// Decode an encoded bitfield, starting at a specific offset.
pub fn decode_with_offset(
    buf: impl AsRef<[u8]>,
    mut offset: usize,
) -> Result<(Vec<u8>, usize), DecodeError> {
    let buf = buf.as_ref();
    let mut bitfield = vec![0; decode_len_with_offset(buf, offset)?];
    let mut next = 0u64;
    let mut ptr = 0;

    while offset < buf.len() {
        offset += varint::decode_with_offset(buf, offset, &mut next);
        let repeat = next & 1;
        let len = if repeat > 0 {
            (next >> 2) as usize
        } else {
            (next >> 1) as usize
        };

        if repeat > 0 {
            if next & 2 > 0 {
                for i in 0..len {
                    bitfield[ptr + i] = 255;
                }
            }
        } else {
            bitfield[ptr..(len + ptr)].clone_from_slice(&buf[offset..(len + offset)]);
            offset += len;
        }

        ptr += len;
    }

    Ok((bitfield, buf.len() - offset))
}

/// Returns how many bytes a decoded bitfield will use.
pub fn decode_len(buf: impl AsRef<[u8]>) -> Result<usize, DecodeError> {
    decode_len_with_offset(&buf, 0)
}

/// Returns how many bytes a decoded bitfield will use, starting at a specific offset.
pub fn decode_len_with_offset(
    buf: impl AsRef<[u8]>,
    mut offset: usize,
) -> Result<usize, DecodeError> {
    let buf = buf.as_ref();
    let mut len = 0;
    let mut next = 0u64;

    while offset < buf.len() {
        offset += varint::decode_with_offset(buf, offset, &mut next);
        let repeat = next & 1;

        let slice = if repeat > 0 {
            (next >> 2) as usize
        } else {
            (next >> 1) as usize
        };

        len += slice;
        if repeat == 0 {
            offset += slice;
        }
    }

    if offset > buf.len() {
        return Err(DecodeError::InvalidRLEBitfield {
            offset,
            len: buf.len(),
        });
    }

    Ok(len)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_should_encode_decode() {
        let mut bits: Vec<u8> = vec![0; 16];
        bits[8] = 0b00000001;

        let enc = encode(&bits);
        assert_eq!(enc.len(), 4);

        let res = decode(enc).unwrap();

        assert_eq!(res[8], 0b00000001);
        assert_eq!(res, bits);
    }

    #[test]
    fn test_encode() {
        let bitfield = vec![255, 255, 85, 84, 0, 0, 0, 183];
        let enc = encode(&bitfield);
        let correct = vec![11, 4, 85, 84, 13, 2, 183];
        assert_eq!(enc.len(), correct.len());
        assert_eq!(enc, correct);
    }

    #[test]
    fn test_decode_len() {
        let enc = [11, 4, 85, 84, 13, 2, 183];
        assert_eq!(8, decode_len(enc).unwrap());
    }

    #[test]
    fn test_decode() {
        let enc = [11, 4, 85, 84, 13, 2, 183];
        let res = decode(enc).unwrap();
        let correct = vec![255, 255, 85, 84, 0, 0, 0, 183];
        assert_eq!(res, correct);
    }

    #[test]
    fn test_not_power_of_two() {
        let deflated = encode(vec![255, 255, 255, 240]);
        let inflated = decode(deflated).unwrap();
        assert_eq!(inflated, vec![255, 255, 255, 240]);
    }

    #[test]
    /// Differs on NodeJS: node trims final bits when 0 and returns a smaller payload
    /// Decoding returns the same result, but encoding the result is smaller
    /// Both are interoperable, with the different on the payload size when reading from node.
    ///
    /// ```js
    /// require('bitfield-rle').encode(Buffer.from([])) // => <Buffer >
    /// require('bitfield-rle').decode(Buffer.from([])) // => <Buffer >
    /// require('bitfield-rle').decode(Buffer.from([0])) // => <Buffer >
    /// ```
    fn test_encodes_empty_bitfield() {
        assert_eq!(decode(encode(vec![])).unwrap(), vec![]);
        assert_eq!(decode(vec![]).unwrap(), vec![]);
        assert_eq!(decode(vec![0]).unwrap(), vec![]);
        assert_eq!(encode(vec![]), vec![0]);
    }

    #[test]
    /// Differs on NodeJS: node trims final bits when 0 and returns a smaller payload
    /// Decoding returns the same result, but encoding the result is smaller.
    /// Both are interoperable, with the different on the payload size when reading from node.
    ///
    /// ```js
    /// var data = require('bitfield-rle').decode(Buffer.from([2, 64, 253, 31])) // => <Buffer 40 00...>
    /// var data = require('bitfield-rle').encode(data) // => <Buffer 02 40>
    /// var data = require('bitfield-rle').encode(data) // => <Buffer 40> skipping the last bits
    /// ```
    fn test_does_not_trims_remaining_bytes() {
        let mut bitfield = vec![0; 1024];
        bitfield[0] = 64;
        assert_eq!(encode(&bitfield), vec![2, 64, 253, 31]);
    }
}
