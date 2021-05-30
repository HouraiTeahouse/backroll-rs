use crate::{
    input::{FrameInput, GameInput, InputQueue},
    protocol::ConnectionStatus,
    BackrollConfig, BackrollError, BackrollResult, Frame, SessionCallbacks, NULL_FRAME,
};
use parking_lot::RwLock;
use std::sync::Arc;
use tracing::{debug, info, warn};

const MAX_PREDICTION_FRAMES: usize = 8;

pub struct Config {
    pub player_count: usize,
}

pub struct SavedFrame<T>
where
    T: BackrollConfig,
{
    frame: super::Frame,
    data: Option<Box<T::State>>,
    checksum: Option<u64>,
}

impl<T: BackrollConfig> Default for SavedFrame<T> {
    fn default() -> Self {
        Self {
            frame: NULL_FRAME,
            data: None,
            checksum: None,
        }
    }
}

pub struct SavedState<T>
where
    T: BackrollConfig,
{
    head: usize,
    frames: [SavedFrame<T>; MAX_PREDICTION_FRAMES + 2],
}

impl<T: BackrollConfig> SavedState<T> {
    pub fn push(&mut self, frame: SavedFrame<T>) {
        self.head += 1;
        self.head %= self.frames.len();
        self.frames[self.head] = frame;
        debug_assert!(self.head < self.frames.len());
    }

    /// Finds a saved state for a frame.
    pub fn find(&self, frame: Frame) -> Option<&SavedFrame<T>> {
        self.frames.iter().find(|saved| saved.frame == frame)
    }

    /// Peeks at the latest saved frame in the queue.
    pub fn latest(&self) -> &SavedFrame<T> {
        debug_assert!(self.head < self.frames.len());
        &self.frames[self.head]
    }
}

impl<T: BackrollConfig> Default for SavedState<T> {
    fn default() -> Self {
        Self {
            // This should lead the first one saved frame to be at
            // index 0.
            head: MAX_PREDICTION_FRAMES + 1,
            frames: Default::default(),
        }
    }
}

pub(crate) struct BackrollSync<T>
where
    T: BackrollConfig,
{
    saved_state: SavedState<T>,
    input_queues: Vec<InputQueue<T>>,
    config: Config,
    rolling_back: bool,

    last_confirmed_frame: Frame,
    frame_count: Frame,
    local_connect_status: Arc<[RwLock<ConnectionStatus>]>,
}

impl<T: BackrollConfig> BackrollSync<T> {
    pub fn new(config: Config, local_connect_status: Arc<[RwLock<ConnectionStatus>]>) -> Self {
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

    pub fn increment_frame(&mut self, callbacks: &mut impl SessionCallbacks<T>) {
        if self.frame_count == 0 {
            self.save_current_frame(callbacks);
        }
        let inputs = self.synchronize_inputs();
        callbacks.advance_frame(inputs);
        self.frame_count += 1;
        self.save_current_frame(callbacks);
    }

    pub fn add_local_input(&mut self, queue: usize, mut input: T::Input) -> BackrollResult<Frame> {
        let frames_behind = self.frame_count - self.last_confirmed_frame;
        if self.frame_count >= MAX_PREDICTION_FRAMES as i32
            && frames_behind >= MAX_PREDICTION_FRAMES as i32
        {
            warn!("Rejecting input: reached prediction barrier.");
            return Err(BackrollError::ReachedPredictionBarrier);
        }

        info!(
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
            output.inputs[idx] = input.input.clone();
        }
        output
    }

    pub fn synchronize_inputs(&self) -> GameInput<T::Input> {
        let mut output: GameInput<T::Input> = Default::default();
        for idx in 0..self.config.player_count {
            if self.is_disconnected(idx) {
                output.disconnected |= 1 << idx;
            } else if let Some(confirmed) =
                self.input_queues[idx].get_confirmed_input(self.frame_count())
            {
                output.inputs[idx] = confirmed.input.clone();
            }
        }
        output
    }

    pub fn check_simulation(&mut self, callbacks: &mut impl SessionCallbacks<T>) {
        if let Some(seek_to) = self.check_simulation_consistency() {
            self.adjust_simulation(callbacks, seek_to);
        }
    }

    pub fn get_last_saved_frame(&self) -> &SavedFrame<T> {
        self.saved_state.latest()
    }

    pub fn load_frame(&mut self, callbacks: &mut impl SessionCallbacks<T>, frame: Frame) {
        // find the frame in question
        if frame == self.frame_count {
            info!("Skipping NOP.");
            return;
        }

        // Move the head pointer back and load it up
        let state = self
            .saved_state
            .find(frame)
            .unwrap_or_else(|| panic!("Could not find saved frame index for frame: {}", frame));

        debug!(
            "=== Loading frame info (checksum: {:08x}).",
            state.checksum.unwrap_or(0)
        );
        callbacks.load_state(
            state
                .data
                .as_deref()
                .cloned()
                .expect("Should not be loading unsaved frames"),
        );

        // Reset framecount and the head of the state ring-buffer to point in
        // advance of the current frame (as if we had just finished executing it).
        self.frame_count = frame;
        self.saved_state.head = (self.saved_state.head + 1) % self.saved_state.frames.len();
    }

    pub fn save_current_frame(&mut self, callbacks: &mut impl SessionCallbacks<T>) {
        let (data, checksum) = callbacks.save_state();
        self.saved_state.push(SavedFrame::<T> {
            frame: self.frame_count,
            data: Some(Box::new(data)),
            checksum,
        });
        info!(
            "=== Saved frame info {} (checksum: {:08x}).",
            self.frame_count,
            checksum.unwrap_or(0)
        );
    }

    pub fn adjust_simulation(&mut self, callbacks: &mut impl SessionCallbacks<T>, seek_to: Frame) {
        let frame_count = self.frame_count;
        let count = self.frame_count - seek_to;

        info!("Catching up");
        self.rolling_back = true;

        //  Flush our input queue and load the last frame.
        self.load_frame(callbacks, seek_to);
        debug_assert!(self.frame_count == seek_to);

        // Advance frame by frame (stuffing notifications back to
        // the master).
        self.reset_prediction(self.frame_count);
        for _ in 0..count {
            let inputs = self.synchronize_inputs();
            callbacks.advance_frame(inputs);
        }
        debug_assert!(self.frame_count == frame_count);

        self.rolling_back = false;
        info!("---");
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

    fn create_queues(config: &Config) -> Vec<InputQueue<T>> {
        (0..config.player_count)
            .map(|_| InputQueue::new())
            .collect()
    }
}

//    void AdjustSimulation(int seek_to);

//    bool CheckSimulationConsistency(int *seekTo);

//    UdpMsg::connect_status *_local_connect_status;

// bool
// Sync::GetEvent(Event &e)
// {
//    if (_event_queue.size()) {
//       e = _event_queue.front();
//       _event_queue.pop();
//       return true;
//    }
//    return false;
// }
