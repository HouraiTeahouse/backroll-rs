use crate::{
    command::Command,
    command::{Commands, LoadState, SaveState},
    input::{FrameInput, GameInput, InputQueue},
    protocol::ConnectionStatus,
    BackrollError, BackrollResult, Config, Frame, NULL_FRAME,
};
use parking_lot::{Mutex, RwLock};

use std::ops::Deref;
use std::sync::Arc;
use tracing::{debug, warn};

const MAX_PREDICTION_FRAMES: usize = 8;

pub struct PlayerConfig {
    pub player_count: usize,
    pub frame_delay: Frame,
}

#[derive(Clone)]
pub(crate) struct SavedFrame<T> {
    pub frame: super::Frame,
    pub data: Option<Box<T>>,
    pub checksum: Option<u64>,
}

impl<T> Default for SavedFrame<T> {
    fn default() -> Self {
        Self {
            frame: NULL_FRAME,
            data: None,
            checksum: None,
        }
    }
}

pub(crate) struct SavedCell<T>(Arc<Mutex<SavedFrame<T>>>);

impl<T> SavedCell<T> {
    pub fn reset(&self, frame: Frame) {
        *self.0.lock() = SavedFrame::<T> {
            frame,
            ..Default::default()
        };
    }

    pub fn save(&self, new_frame: SavedFrame<T>) {
        debug_assert!(new_frame.data.is_some());
        let mut saved_frame = self.0.lock();
        saved_frame.data = new_frame.data;
        saved_frame.checksum = new_frame.checksum;
    }

    pub fn is_valid(&self) -> bool {
        let frame = self.0.lock();
        frame.data.is_some() && !crate::is_null(frame.frame)
    }
}

impl<T: Clone> SavedCell<T> {
    pub fn load(&self) -> T {
        let frame = self.0.lock();
        debug!(
            "=== Loading frame info (checksum: {:08x}).",
            frame.checksum.unwrap_or(0)
        );
        if let Some(data) = &frame.data {
            data.deref().clone()
        } else {
            panic!("Trying to load data that wasn't saved to.")
        }
    }
}

impl<T> Default for SavedCell<T> {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Default::default())))
    }
}

impl<T> Clone for SavedCell<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

pub(crate) struct SavedState<T> {
    head: usize,
    frames: [SavedCell<T>; MAX_PREDICTION_FRAMES + 2],
}

impl<T: Clone> SavedState<T> {
    pub fn push(&mut self, frame: Frame) -> SavedCell<T> {
        let saved_frame = self.frames[self.head].clone();
        saved_frame.reset(frame);
        self.head = (self.head + 1) % self.frames.len();
        debug_assert!(self.head < self.frames.len());
        saved_frame
    }

    /// Finds a saved state for a frame.
    fn find_index(&self, frame: Frame) -> Option<usize> {
        self.frames
            .iter()
            .enumerate()
            .find(|(_, saved)| saved.0.lock().frame == frame)
            .map(|(i, _)| i)
    }

    pub fn reset_to(&mut self, frame: Frame) -> SavedCell<T> {
        self.head = self
            .find_index(frame)
            .unwrap_or_else(|| panic!("Could not find saved frame index for frame: {}", frame));
        self.frames[self.head].clone()
    }

    /// Peeks at the latest saved frame in the queue.
    pub fn latest(&self) -> Option<SavedCell<T>> {
        self.frames
            .iter()
            .max_by_key(|saved| saved.0.lock().frame)
            .cloned()
    }
}

impl<T> Default for SavedState<T> {
    fn default() -> Self {
        Self {
            head: 0,
            frames: Default::default(),
        }
    }
}

pub(crate) struct Sync<T>
where
    T: Config,
{
    saved_state: SavedState<T::State>,
    input_queues: Vec<InputQueue<T::Input>>,
    config: PlayerConfig,
    rolling_back: bool,

    last_confirmed_frame: Frame,
    frame_count: Frame,
    local_connect_status: Arc<[RwLock<ConnectionStatus>]>,
}

impl<T: Config> Sync<T> {
    pub fn new(
        config: PlayerConfig,
        local_connect_status: Arc<[RwLock<ConnectionStatus>]>,
    ) -> Self {
        let input_queues = Self::create_queues(&config);
        Self {
            saved_state: Default::default(),
            local_connect_status,
            input_queues,
            config,

            rolling_back: false,
            last_confirmed_frame: super::NULL_FRAME,
            frame_count: 0,
        }
    }

    pub fn player_count(&self) -> usize {
        self.config.player_count
    }

    pub fn frame_count(&self) -> Frame {
        self.frame_count
    }

    pub fn in_rollback(&self) -> bool {
        self.rolling_back
    }

    pub fn set_last_confirmed_frame(&mut self, frame: Frame) {
        self.last_confirmed_frame = frame;
        if frame > 0 {
            for queue in self.input_queues.iter_mut() {
                queue.discard_confirmed_frames(frame - 1);
            }
        }
    }

    pub fn set_frame_delay(&mut self, queue: usize, delay: Frame) {
        self.input_queues[queue].set_frame_delay(delay);
    }

    pub fn increment_frame(&mut self, commands: &mut Commands<T>) {
        if self.frame_count == 0 {
            self.save_current_frame(commands);
        }
        let inputs = self.synchronize_inputs();
        commands.push(Command::AdvanceFrame(inputs));
        self.frame_count += 1;
        self.save_current_frame(commands);
    }

    pub fn add_local_input(&mut self, queue: usize, input: T::Input) -> BackrollResult<Frame> {
        let frames_behind = self.frame_count - self.last_confirmed_frame;
        if self.frame_count >= MAX_PREDICTION_FRAMES as i32
            && frames_behind >= MAX_PREDICTION_FRAMES as i32
        {
            warn!("Rejecting input: reached prediction barrier.");
            return Err(BackrollError::ReachedPredictionBarrier);
        }

        debug!(
            "Sending undelayed local frame {} to queue {}.",
            self.frame_count, queue
        );

        self.input_queues[queue].add_input(FrameInput::<T::Input> {
            frame: self.frame_count,
            input,
        });

        Ok(self.frame_count)
    }

    pub fn add_remote_input(&mut self, queue: usize, input: FrameInput<T::Input>) {
        self.input_queues[queue].add_input(input);
    }

    pub fn get_confirmed_inputs(&mut self, frame: Frame) -> GameInput<T::Input> {
        let mut output: GameInput<T::Input> = Default::default();
        for idx in 0..self.config.player_count {
            let input = if self.is_disconnected(idx) {
                output.disconnected |= 1 << idx;
                Default::default()
            } else {
                self.input_queues[idx]
                    .get_confirmed_input(frame)
                    .unwrap()
                    .clone()
            };
            output.inputs[idx] = input.input;
        }
        output
    }

    pub fn synchronize_inputs(&mut self) -> GameInput<T::Input> {
        let mut output = GameInput::<T::Input> {
            frame: self.frame_count,
            ..Default::default()
        };
        for idx in 0..self.config.player_count {
            if self.is_disconnected(idx) {
                output.disconnected |= 1 << idx;
            } else {
                output.inputs[idx] = self.input_queues[idx]
                    .get_input(self.frame_count)
                    .unwrap()
                    .input;
            }
        }
        output
    }

    pub fn check_simulation(&mut self, commands: &mut Commands<T>) {
        if let Some(seek_to) = self.check_simulation_consistency() {
            self.adjust_simulation(commands, seek_to);
        }
    }

    pub fn get_last_saved_frame(&self) -> SavedCell<T::State> {
        self.saved_state.latest().unwrap()
    }

    pub fn load_frame(&mut self, commands: &mut Commands<T>, frame: Frame) {
        // find the frame in question
        if frame == self.frame_count {
            debug!("Skipping NOP.");
            return;
        }

        let cell = self.saved_state.reset_to(frame);
        self.frame_count = cell.0.lock().frame;
        commands.push(Command::Load(LoadState::<T::State> { cell }));

        self.saved_state.head += 1;
        self.saved_state.head %= self.saved_state.frames.len();
    }

    pub fn save_current_frame(&mut self, commands: &mut Commands<T>) {
        let cell = self.saved_state.push(self.frame_count);
        commands.push(Command::Save(SaveState::<T::State> {
            cell,
            frame: self.frame_count,
        }));
    }

    pub fn adjust_simulation(&mut self, commands: &mut Commands<T>, seek_to: Frame) {
        let frame_count = self.frame_count;
        let count = self.frame_count - seek_to;

        debug!("Catching up");
        self.rolling_back = true;

        //  Flush our input queue and load the last frame.
        self.load_frame(commands, seek_to);
        debug_assert!(self.frame_count == seek_to);

        // Advance frame by frame (stuffing notifications back to
        // the master).
        self.reset_prediction(self.frame_count);
        for _ in 0..count {
            self.increment_frame(commands);
        }
        debug_assert!(self.frame_count == frame_count);

        self.rolling_back = false;
    }

    pub fn check_simulation_consistency(&self) -> Option<Frame> {
        self.input_queues
            .iter()
            .map(|queue| queue.first_incorrect_frame())
            .filter(|frame| !super::is_null(*frame))
            .min()
    }

    fn reset_prediction(&mut self, frame: Frame) {
        for queue in self.input_queues.iter_mut() {
            queue.reset_prediction(frame);
        }
    }

    fn is_disconnected(&self, player: usize) -> bool {
        let status = self.local_connect_status[player].read();
        status.disconnected && status.last_frame < self.frame_count()
    }

    fn create_queues(config: &PlayerConfig) -> Vec<InputQueue<T::Input>> {
        (0..config.player_count)
            .map(|_| InputQueue::new(config.frame_delay))
            .collect()
    }
}
