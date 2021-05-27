use super::{input::FrameInput, Frame};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::ops::{Add, Sub};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::info;

const FRAME_WINDOW_SIZE: usize = 40;
const MIN_UNIQUE_FRAMES: usize = 10;
const MIN_FRAME_ADVANTAGE: super::Frame = 3;
const MAX_FRAME_ADVANTAGE: super::Frame = 9;

struct TimeSyncRef<T> {
    local: [Frame; FRAME_WINDOW_SIZE],
    remote: [Frame; FRAME_WINDOW_SIZE],
    last_inputs: [FrameInput<T>; MIN_UNIQUE_FRAMES],
    iteration: u32,
}

#[derive(Clone)]
pub struct TimeSync<T>(Arc<Mutex<TimeSyncRef<T>>>);

impl<T: Default> Default for TimeSync<T> {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(TimeSyncRef {
            local: [0; FRAME_WINDOW_SIZE],
            remote: [0; FRAME_WINDOW_SIZE],
            last_inputs: Default::default(),
            iteration: 0,
        })))
    }
}

impl<T: PartialEq> TimeSync<T> {
    pub fn advance_frame(&self, input: FrameInput<T>, advantage: Frame, radvantage: Frame) {
        let frame = usize::try_from(input.frame).unwrap();
        let mut sync = self.0.lock();
        // Remember the last frame and frame advantage
        sync.last_inputs[frame % MIN_UNIQUE_FRAMES] = input;
        sync.local[frame % FRAME_WINDOW_SIZE] = advantage;
        sync.remote[frame % FRAME_WINDOW_SIZE] = radvantage;
    }

    pub fn recommend_frame_wait_duration(&self, require_idle_input: bool) -> super::Frame {
        let mut sync = self.0.lock();

        // Average our local and remote frame advantages
        let sum = sync.local.iter().sum::<Frame>() as f32;
        let advantage = sum / (sync.local.len() as f32);

        let sum = sync.remote.iter().sum::<Frame>() as f32;
        let radvantage = sum / (sync.remote.len() as f32);

        sync.iteration += 1;

        // See if someone should take action.  The person furthest ahead
        // needs to slow down so the other user can catch up.
        // Only do this if both clients agree on who's ahead!!
        if advantage >= radvantage {
            return 0;
        }

        // Both clients agree that we're the one ahead.  Split
        // the difference between the two to figure out how long to
        // sleep for.
        let sleep_frames = (((radvantage - advantage) / 2.0) + 0.5) as Frame;

        info!(
            "iteration {}:  sleep frames is {}",
            sync.iteration, sleep_frames
        );

        // Some things just aren't worth correcting for.  Make sure
        // the difference is relevant before proceeding.
        if sleep_frames < MIN_FRAME_ADVANTAGE {
            return 0;
        }

        // Make sure our input had been "idle enough" before recommending
        // a sleep.  This tries to make the emulator sleep while the
        // user's input isn't sweeping in arcs (e.g. fireball motions in
        // Street Fighter), which could cause the player to miss moves.
        if require_idle_input {
            for idx in 0..sync.last_inputs.len() {
                if sync.last_inputs[idx] != sync.last_inputs[0] {
                    info!(
                        "iteration {}: rejecting due to input stuff at position {}...!!!",
                        sync.iteration, idx
                    );
                    return 0;
                }
            }
        }

        // Success!!! Recommend the number of frames to sleep and adjust
        std::cmp::min(sleep_frames, MAX_FRAME_ADVANTAGE)
    }
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnixMillis(u64);

impl UnixMillis {
    pub fn now() -> Self {
        Self(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        )
    }

    pub const fn from_millis(millis: u64) -> Self {
        Self(millis)
    }

    pub const fn as_millis(&self) -> u64 {
        self.0
    }
}

impl Add<Duration> for UnixMillis {
    type Output = UnixMillis;
    fn add(self, other: Duration) -> Self::Output {
        Self(self.0 + other.as_millis() as u64)
    }
}

impl Sub<Duration> for UnixMillis {
    type Output = UnixMillis;
    fn sub(self, other: Duration) -> Self::Output {
        Self(self.0 - other.as_millis() as u64)
    }
}

impl Sub<UnixMillis> for UnixMillis {
    type Output = Duration;
    fn sub(self, other: Self) -> Self::Output {
        Duration::from_millis(self.0 - other.0)
    }
}
