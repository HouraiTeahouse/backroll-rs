use std::time::Duration;

mod protocol;

mod backend;
mod input;
mod sync;
mod time_sync;

pub use backend::*;
pub use backroll_transport as transport;
pub use input::GameInput;

// TODO(james7132): Generalize the executor for these.
pub(crate) use bevy_tasks::TaskPool;

pub const MAX_PLAYERS_PER_MATCH: usize = 8;
// Approximately 2 seconds of frames.
const MAX_ROLLBACK_FRAMES: usize = 120;

type Frame = i32;
const NULL_FRAME: Frame = -1;

fn is_null(frame: Frame) -> bool {
    frame < 0
}

#[derive(Copy, Clone, Debug)]
pub struct BackrollPlayerHandle(pub usize);

pub enum BackrollPlayer {
    Local,
    Spectator(transport::connection::Peer),
    Remote(transport::connection::Peer),
}

pub trait BackrollConfig: 'static {
    type Input: Default + Eq + Clone + bytemuck::Pod + Send + Sync;

    /// The save state type for the session. This type must be safe to send across
    /// threads and have a 'static lifetime. This type is also responsible for
    /// dropping any internal linked state via the `[Drop]` trait.
    type State: 'static + Send + Sync;

    const MAX_PLAYERS_PER_MATCH: usize;
    const RECOMMENDATION_INTERVAL: u32;
}

pub trait SessionCallbacks<T>
where
    T: BackrollConfig,
{
    /// The client should copy the entire contents of the current game state into a
    ///  new state struct and return it.
    ///
    /// Optionally, the client can compute a 64-bit checksum of the data and return it.
    fn save_state(&mut self) -> (T::State, Option<u64>);

    /// Backroll will call this function at the beginning of a rollback. The argument
    /// provided will be a previously saved state returned from the save_state function.  
    /// The client should make the current game state match the state contained in the
    /// argument.
    fn load_state(&mut self, state: &T::State);

    /// Called during a rollback.  You should advance your game state by exactly one frame.  
    /// `inputs` will contain the inputs you should use for the given frame.
    fn advance_frame(&mut self, input: GameInput<T::Input>);

    ///  Notification that something has happened. See the `[BackcrollEvent]`
    /// struct for more information.
    fn handle_event(&mut self, event: BackrollEvent);
}

pub enum BackrollError {
    UnsupportedOperation,
    InRollback,
    NotSynchronized,
    ReachedPredictionBarrier,
    InvalidPlayer(BackrollPlayerHandle),
    PlayerDisconnected(BackrollPlayerHandle),
}

pub type BackrollResult<T> = Result<T, BackrollError>;

#[derive(Clone, Debug, Default)]
pub struct NetworkStats {
    pub ping: Duration,
    pub send_queue_len: usize,
    pub recv_queue_len: usize,
    pub kbps_sent: u32,

    pub local_frames_behind: Frame,
    pub remote_frames_behind: Frame,
}

pub enum BackrollEvent {}
