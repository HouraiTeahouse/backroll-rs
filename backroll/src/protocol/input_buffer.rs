use super::compression;
use crate::{input::FrameInput, Frame};
use parking_lot::RwLock;
use std::collections::VecDeque;
use std::sync::Arc;

struct InputEncoderRef<T>
where
    T: bytemuck::Zeroable,
{
    pending: VecDeque<FrameInput<T>>,

    last_acked: FrameInput<T>,
    last_encoded: FrameInput<T>,
}

/// A buffer of all inputs that have not been yet acknowledged by a connected remote peer.
///
/// This struct wraps an Arc, so it's safe to make clones and pass it around.
#[derive(Clone)]
pub(super) struct InputEncoder<T>(Arc<RwLock<InputEncoderRef<T>>>)
where
    T: bytemuck::Zeroable + bytemuck::Pod;

impl<T: bytemuck::Zeroable + bytemuck::Pod> Default for InputEncoder<T> {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(InputEncoderRef::<T> {
            pending: VecDeque::new(),

            last_acked: Default::default(),
            last_encoded: Default::default(),
        })))
    }
}

impl<T: bytemuck::Zeroable + bytemuck::Pod> InputEncoder<T> {
    /// Adds an input to as the latest element in the queue.
    pub fn push(&self, input: FrameInput<T>) {
        self.0.write().pending.push_back(input);
    }

    /// Gets the frame of the last input that was encoded via `[encode]`.
    pub fn last_encoded_frame(&self) -> Frame {
        self.0.read().last_encoded.frame
    }
}

impl<T: bytemuck::Zeroable + bytemuck::Pod + Clone> InputEncoder<T> {
    /// Acknowledges a given frame. All inputs with of a prior frame will be dropped.
    ///
    /// This will update the reference input that is used to delta-encode.
    pub fn acknowledge_frame(&self, ack_frame: Frame) {
        let mut queue = self.0.write();
        // Get rid of our buffered input
        let last = queue.pending.iter().filter(|i| i.frame < ack_frame).last();
        if let Some(last) = last {
            queue.last_acked = last.clone();
            queue.pending.retain(|i| i.frame >= ack_frame);
        }
    }

    /// Encodes all pending output as a byte buffer.
    ///
    /// To minimize the size of the produced buffer, the sequence of is delta
    /// encoded by `[compression::encode]` relative to the last acknowledged
    /// input, which is updated via `[acknowledge_frame]`.
    ///
    /// This will not remove any of the inputs in the queue, but will update
    /// the value returned by `[last_encoded_frame]` to reflect the highest
    /// frame that has been encoded.
    pub fn encode(&self) -> Result<(Frame, Vec<u8>), compression::EncodeError> {
        let mut queue = self.0.write();
        let pending = &queue.pending;
        if !pending.is_empty() {
            let start_frame = pending.front().unwrap().frame;
            let inputs = pending.iter().map(|f| &f.input);
            let bits = compression::encode(&queue.last_acked.input, inputs)?;
            queue.last_encoded = queue.pending.back().unwrap().clone();
            Ok((start_frame, bits))
        } else {
            Ok((queue.last_acked.frame, Vec::new()))
        }
    }
}

struct InputDecoderRef<T>
where
    T: bytemuck::Zeroable,
{
    last_decoded: FrameInput<T>,
}

/// A stateful decoder that decodes delta patches created by `[InputEncoder]`.
///
/// This struct wraps an Arc, so it's safe to make clones and pass it around.
#[derive(Clone)]
pub(super) struct InputDecoder<T>(Arc<RwLock<InputDecoderRef<T>>>)
where
    T: bytemuck::Zeroable + bytemuck::Pod;

impl<T: bytemuck::Zeroable + bytemuck::Pod> Default for InputDecoder<T> {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(InputDecoderRef::<T> {
            last_decoded: Default::default(),
        })))
    }
}

impl<T: bytemuck::Zeroable + bytemuck::Pod> InputDecoder<T> {
    /// Gets the frame of the most recently decoded input if available.
    ///
    /// If no input has been decoded yet, this will be the NULL_FRAME.
    pub fn last_decoded_frame(&self) -> Frame {
        self.0.read().last_decoded.frame
    }
}

impl<T: bytemuck::Zeroable + bytemuck::Pod + Clone> InputDecoder<T> {
    /// Resets the internal state of the decoder to it's default.
    pub fn reset(&self) {
        self.0.write().last_decoded = Default::default();
    }

    pub fn decode(
        &self,
        start_frame: Frame,
        bits: impl AsRef<[u8]>,
    ) -> Result<Vec<FrameInput<T>>, compression::DecodeError> {
        let mut decoder = self.0.write();
        let last_decoded_frame = decoder.last_decoded.frame;
        let current_frame = if crate::is_null(decoder.last_decoded.frame) {
            start_frame - 1
        } else {
            last_decoded_frame
        };
        let frame_inputs = compression::decode(&decoder.last_decoded.input, bits)?
            .into_iter()
            .enumerate()
            .map(|(i, input)| FrameInput::<T> {
                frame: start_frame + i as Frame,
                input,
            })
            .skip_while(|input| input.frame <= current_frame)
            .collect::<Vec<_>>();

        if let Some(latest) = frame_inputs.last() {
            decoder.last_decoded = latest.clone();
        }

        debug_assert!(decoder.last_decoded.frame >= last_decoded_frame);

        Ok(frame_inputs)
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
        let encoder = InputEncoder::<Input>::default();
        let decoder = InputDecoder::<Input>::default();
        let mut buf: Vec<Input> = Vec::new();
        for frame in 0..100 {
            let input = Input { x: 420, y: 1337 };
            buf.push(input);
            encoder.push(FrameInput::<Input> { frame, input });
        }

        let (start, encoded) = encoder.encode().unwrap();
        let decoded = decoder.decode(start, &encoded).unwrap();
        assert_eq!(start, 0);
        assert_eq!(encoded, vec![4, 164, 1, 9, 4, 57, 5, 233, 24]);
        assert_eq!(
            decoded.into_iter().map(|f| f.input).collect::<Vec<Input>>(),
            buf
        );
        assert_eq!(decoder.last_decoded_frame(), 99);
    }

    #[test]
    pub fn test_empty_buffer() {
        let encoder = InputEncoder::<Input>::default();
        let decoder = InputDecoder::<Input>::default();
        let buf: Vec<Input> = Vec::new();

        let (start, encoded) = encoder.encode().unwrap();
        let decoded = decoder.decode(start, encoded.clone()).unwrap();
        assert_eq!(start, -1);
        assert_eq!(
            decoded.into_iter().map(|f| f.input).collect::<Vec<Input>>(),
            buf
        );
        assert_eq!(decoder.last_decoded_frame(), -1);
    }

    #[test]
    pub fn test_encodes_the_same_until_acknowledged() {
        let mut rng = rand::thread_rng();
        let encoder = InputEncoder::<Input>::default();
        let mut buf: Vec<Input> = Vec::new();
        let base = Input {
            x: rng.next_u32() as i32,
            y: rng.next_u32() as i32,
        };
        for frame in 0..100 {
            let input = if rng.next_u32() > u32::MAX / 4 {
                base
            } else {
                Input {
                    x: rng.next_u32() as i32,
                    y: rng.next_u32() as i32,
                }
            };
            buf.push(input);
            encoder.push(FrameInput::<Input> { frame, input });
        }

        let (start_1, encoded_1) = encoder.encode().unwrap();
        let (start_2, encoded_2) = encoder.encode().unwrap();
        assert_eq!(start_1, start_2);
        assert_eq!(encoded_1, encoded_2);
        encoder.acknowledge_frame(53);
        let (start_3, encoded_3) = encoder.encode().unwrap();
        assert!(start_3 != start_1);
        assert!(encoded_3 != encoded_1);
        assert!(start_3 != start_2);
        assert!(encoded_3 != encoded_2);
    }

    #[test]
    pub fn test_random_data() {
        let mut rng = rand::thread_rng();
        for i in 0..100 {
            let encoder = InputEncoder::<Input>::default();
            let decoder = InputDecoder::<Input>::default();
            let mut buf: Vec<Input> = Vec::new();
            let base = Input {
                x: rng.next_u32() as i32,
                y: rng.next_u32() as i32,
            };
            for frame in 0..100 {
                let input = if rng.next_u32() > u32::MAX / 4 {
                    base
                } else {
                    Input {
                        x: rng.next_u32() as i32,
                        y: rng.next_u32() as i32,
                    }
                };
                buf.push(input);
                encoder.push(FrameInput::<Input> { frame, input });
            }

            let (start, encoded) = encoder.encode().unwrap();
            let decoded = decoder.decode(start, &encoded).unwrap();
            assert_eq!(start, 0);
            assert!(encoded.len() <= std::mem::size_of::<Input>() * buf.len());
            assert_eq!(decoded.len(), buf.len());
            assert_eq!(decoder.last_decoded_frame(), 99);
            assert_eq!(
                decoded.into_iter().map(|f| f.input).collect::<Vec<Input>>(),
                buf
            );
        }
    }
}
