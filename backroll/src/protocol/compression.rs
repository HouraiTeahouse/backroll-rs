use bytemuck::Pod;
use thiserror::Error;

/// The maximum supported size of the raw buffer.
const MAX_BUFFER_SIZE: usize = u16::MAX as usize;

/// Encodes a set of `[Pod]` values into a byte buffer relative to a reference snapshot.
///
/// # Security
/// This function fails if the delta encoded output is bigger than `[MAX_BUFFER_SIZE]` to prevent
/// memory exhaustion.
///
/// [Pod](bytemuck::Pod)
pub fn encode<'a, T: Pod>(
    base: &'a T,
    data: impl Iterator<Item = &'a T>,
) -> Result<Vec<u8>, EncodeError> {
    let mut base = base.clone();
    let bits = bytemuck::bytes_of_mut(&mut base);
    let (lower, upper) = data.size_hint();
    let capacity = std::cmp::min(MAX_BUFFER_SIZE, upper.unwrap_or(lower) * bits.len());
    let mut bytes = Vec::with_capacity(capacity);

    // Create buffer of delta encoded bytes via XOR.
    for datum in data {
        let datum_bytes = bytemuck::bytes_of(datum);
        debug_assert!(bits.len() == datum_bytes.len());
        for (b1, b2) in bits.iter_mut().zip(datum_bytes.iter()) {
            bytes.push(*b1 ^ *b2);
            *b1 = *b2;
        }

        if bytes.len() >= MAX_BUFFER_SIZE {
            return Err(EncodeError::TooBig { len: bytes.len() });
        }
    }

    // Bitfield RLE the result
    Ok(bitfield::encode(bytes))
}

/// Decodes a set of delta encoded bytes into a buffer of `[Pod]` values relative to a
/// reference snapshot.
///
/// # Security
/// This function fails if the delta encoded output is bigger than `[MAX_BUFFER_SIZE]` to prevent
/// memory exhaustion. Also fails if the provided bytes cannot be safely converted via
/// `[bytemuck::bytes_of]`.
///
/// [Pod](bytemuck::Pod)
pub fn decode<T: Pod + Clone>(base: &T, data: impl AsRef<[u8]>) -> Result<Vec<T>, DecodeError> {
    let mut base = base.clone();
    let bits = bytemuck::bytes_of_mut(&mut base);
    let stride = bits.len();
    debug_assert!(stride > 0);
    let delta_len = bitfield::decode_len_with_offset(data.as_ref(), 0)?;

    // Ensure that the size of the buffer is not too big.
    if delta_len > MAX_BUFFER_SIZE {
        return Err(DecodeError::TooBig { len: delta_len });
    }

    let delta = bitfield::decode(data)?;
    debug_assert!(delta.len() % stride == 0);
    let output_size = delta.len() / stride;
    let mut output = Vec::with_capacity(output_size);

    for idx in 0..output_size {
        for (local_idx, byte) in bits.iter_mut().enumerate() {
            *byte ^= delta[idx * stride + local_idx];
        }
        output.push(bytemuck::try_from_bytes::<T>(&bits)?.clone())
    }

    Ok(output)
}

#[derive(Error, Debug)]
pub enum EncodeError {
    #[error("Input buffer is too big: {}", .len)]
    TooBig { len: usize },
}

#[derive(Error, Debug)]
pub enum DecodeError {
    #[error("Cannot be cast {:?}", .0)]
    Cast(bytemuck::PodCastError),
    #[error("RLE decode error: offset: {}, len: {}", .offset, .len)]
    InvalidRLEBitfield { offset: usize, len: usize },
    #[error("Output buffer is too big: {}", .len)]
    TooBig { len: usize },
}

impl From<bytemuck::PodCastError> for DecodeError {
    fn from(value: bytemuck::PodCastError) -> Self {
        Self::Cast(value)
    }
}

mod bitfield {
    use super::DecodeError;
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
        let mut enc = Vec::with_capacity(encode_len_with_offset(&buf, offset));

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

    /// Returns how many bytes a decoded bitfield will use.
    pub fn encode_len(buf: impl AsRef<[u8]>) -> usize {
        encode_len_with_offset(&buf, 0)
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
        let mut bitfield = vec![0; decode_len_with_offset(&buf, offset)?];
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
}
