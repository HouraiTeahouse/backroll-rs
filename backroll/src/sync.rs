use super::{
    input::{FrameInput, GameInput, InputQueue},
    protocol::ConnectionStatus,
    BackrollConfig, BackrollError, BackrollResult, Frame, SessionCallbacks,
};
use parking_lot::RwLock;
use std::sync::Arc;
use tracing::{info, warn};

const MAX_PREDICTION_FRAMES: usize = 8;

pub struct Config<T>
where
    T: BackrollConfig,
{
    pub callbacks: Box<dyn SessionCallbacks<T>>,
    pub player_count: usize,
}

pub struct SavedFrame<T>
where
    T: BackrollConfig,
{
    frame: super::Frame,
    data: Option<Box<T::State>>,
    checksum: i32,
}

impl<T: BackrollConfig> Default for SavedFrame<T> {
    fn default() -> Self {
        Self {
            frame: super::NULL_FRAME,
            data: None,
            checksum: 0,
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

impl<T: BackrollConfig> Default for SavedState<T> {
    fn default() -> Self {
        Self {
            head: 0,
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
    config: Config<T>,
    rolling_back: bool,

    last_confirmed_frame: Frame,
    frame_count: Frame,
    local_connect_status: Arc<[RwLock<ConnectionStatus>]>,
}

impl<T: BackrollConfig> BackrollSync<T> {
    pub fn new(config: Config<T>, local_connect_status: Arc<[RwLock<ConnectionStatus>]>) -> Self {
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

    pub fn increment_frame(&mut self) {
        self.frame_count += 1;
        self.save_current_frame();
    }

    pub fn add_local_input(
        &mut self,
        queue: usize,
        mut input: FrameInput<T::Input>,
    ) -> BackrollResult<Frame> {
        let frames_behind = self.frame_count - self.last_confirmed_frame;
        if self.frame_count >= MAX_PREDICTION_FRAMES as i32
            && frames_behind >= MAX_PREDICTION_FRAMES as i32
        {
            warn!("Rejecting input from emulator: reached prediction barrier.");
            return Err(BackrollError::ReachedPredictionBarrier);
        }

        if self.frame_count == 0 {
            self.save_current_frame();
        }

        info!(
            "Sending undelayed local frame {} to queue {}.",
            self.frame_count, queue
        );
        input.frame = self.frame_count;
        self.input_queues[queue].add_input(input);

        Ok(self.frame_count)
    }

    pub fn add_remote_input(&mut self, queue: usize, input: FrameInput<T::Input>) {
        self.input_queues[queue].add_input(input);
    }

    pub fn get_confirmed_inputs(&mut self, frame: Frame) -> (GameInput<T::Input>, u32) {
        let mut disconnect_flags = 0;
        let mut output: GameInput<T::Input> = Default::default();
        for idx in 0..self.config.player_count {
            let input = if self.is_disconnected(idx) {
                disconnect_flags |= 1 << idx;
                Default::default()
            } else {
                self.input_queues[idx]
                    .get_confirmed_input(frame)
                    .unwrap()
                    .clone()
            };
            output.inputs[idx] = input.input.clone();
        }
        (output, disconnect_flags)
    }

    pub fn synchronize_inputs(&self) -> (GameInput<T::Input>, u32) {
        let mut disconnect_flags = 0;
        let mut output: GameInput<T::Input> = Default::default();
        for idx in 0..self.config.player_count {
            if self.is_disconnected(idx) {
                disconnect_flags |= 1 << idx;
            } else if let Some(confirmed) =
                self.input_queues[idx].get_confirmed_input(self.frame_count())
            {
                output.inputs[idx] = confirmed.input.clone();
            }
        }
        (output, disconnect_flags)
    }

    pub fn check_simulation(&mut self) {
        if let Some(seek_to) = self.check_simulation_consistency() {
            self.adjust_simulation(seek_to);
        }
    }

    fn find_saved_frame_index(&self, frame: Frame) -> usize {
        self.saved_state
            .frames
            .iter()
            .enumerate()
            .find(|(_, saved)| saved.frame == frame)
            .unwrap_or_else(|| panic!("Could not find saved frame index for frame: {}", frame))
            .0
    }

    pub fn get_last_saved_frame(&self) -> &SavedFrame<T> {
        let idx = match self.saved_state.head {
            0 => self.saved_state.frames.len() - 1,
            x => x - 1,
        };
        &self.saved_state.frames[idx]
    }

    pub fn load_frame(&mut self, frame: Frame) {
        // find the frame in question
        if frame == self.frame_count {
            info!("Skipping NOP.");
            return;
        }

        // Move the head pointer back and load it up
        self.saved_state.head = self.find_saved_frame_index(frame);
        let state = &self.saved_state.frames[self.saved_state.head];

        info!("=== Loading frame info (checksum: {:08x}).", state.checksum);
        debug_assert!(state.data.is_some());
        // self.config.callbacks.load_game_state(state);

        // Reset framecount and the head of the state ring-buffer to point in
        // advance of the current frame (as if we had just finished executing it).
        self.frame_count = state.frame;
        self.saved_state.head = (self.saved_state.head + 1) % self.saved_state.frames.len();
    }

    pub fn save_current_frame(&mut self) {
        {
            let state = &mut self.saved_state.frames[self.saved_state.head];
            state.frame = self.frame_count;
            // let (save, checksum) = self.config.callbacks.save_game_state(state->frame);
            // state.data = Some(save);
            // state.checksum = checksum;
            info!(
                "=== Saved frame info {} (checksum: {:08x}).",
                state.frame, state.checksum
            );
        };
        self.saved_state.head = (self.saved_state.head + 1) % self.saved_state.frames.len();
    }

    pub fn adjust_simulation(&mut self, seek_to: Frame) {
        let frame_count = self.frame_count;
        let count = self.frame_count - seek_to;

        info!("Catching up");
        self.rolling_back = true;

        //  Flush our input queue and load the last frame.
        self.load_frame(seek_to);
        debug_assert!(self.frame_count == seek_to);

        // Advance frame by frame (stuffing notifications back to
        // the master).
        self.reset_prediction(self.frame_count);
        for _ in 0..count {
            // _callbacks.advance_frame(0);
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

    fn create_queues(config: &Config<T>) -> Vec<InputQueue<T>> {
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
