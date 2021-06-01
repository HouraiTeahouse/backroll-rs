use super::bitfield;
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
    let bytes = delta_encode(base, data)?;
    // Bitfield RLE the result
    Ok(bitfield::encode(bytes))
}

fn delta_encode<'a, T: bytemuck::Pod>(
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

    Ok(bytes)
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
pub fn decode<T: Pod>(base: &T, data: impl AsRef<[u8]>) -> Result<Vec<T>, DecodeError> {
    let mut base = *base;
    let bits = bytemuck::bytes_of_mut(&mut base);
    let stride = bits.len();
    debug_assert!(stride > 0);

    let delta_len = bitfield::decode_len(data.as_ref())?;

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

#[cfg(test)]
mod test {
    use super::*;
    use bytemuck::{Pod, Zeroable};
    use rand::RngCore;

    #[repr(C)]
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    struct Input {
        x: i32,
        y: i32,
    }

    unsafe impl Pod for Input {}
    unsafe impl Zeroable for Input {}

    #[test]
    pub fn test_same_input_compresses_down() {
        let mut buf: Vec<Input> = Vec::new();
        let base = Input { x: 120, y: 120 };
        for _ in 0..100 {
            buf.push(Input { x: 420, y: 1337 });
        }

        let encoded = encode(&base, buf.iter()).unwrap();
        let decoded = decode(&base, encoded.iter()).unwrap();
        assert_eq!(encoded, vec![4, 220, 1, 9, 4, 65, 5, 233, 24]);
        assert_eq!(decoded, buf);
    }

    #[test]
    pub fn test_empty_buffer() {
        let buf: Vec<Input> = Vec::new();
        let base = Input { x: 120, y: 120 };

        let encoded = encode(&base, buf.iter()).unwrap();
        let decoded = decode(&base, encoded.iter()).unwrap();
        assert_eq!(encoded, vec![0]);
        assert_eq!(decoded, buf);
    }

    #[test]
    pub fn test_random_data() {
        let mut rng = rand::thread_rng();
        for _ in 0..100 {
            let mut buf: Vec<Input> = Vec::new();
            let base = Input {
                x: rng.next_u32() as i32,
                y: rng.next_u32() as i32,
            };
            for _ in 0..100 {
                if rng.next_u32() > u32::MAX / 4 {
                    buf.push(base);
                } else {
                    buf.push(Input {
                        x: rng.next_u32() as i32,
                        y: rng.next_u32() as i32,
                    });
                }
            }

            let encoded = encode(&base, buf.iter()).unwrap();
            let decoded = decode(&base, encoded.iter()).unwrap();
            assert!(encoded.len() <= std::mem::size_of::<Input>() * buf.len());
            assert_eq!(decoded, buf);
        }
    }
}
