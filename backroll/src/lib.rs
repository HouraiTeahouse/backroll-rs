use self::{
    input::{FrameInput, GameInput},
    protocol::BackrollPeer,
};
use std::time::Duration;

mod protocol;

mod backend;
pub mod input;
mod sync;
mod time_sync;

pub use backend::*;
pub use backroll_transport as transport;

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

pub trait BackrollConfig {
    type Input: Default + Eq + Clone + bytemuck::Pod;
    type State;

    const MAX_PLAYERS_PER_MATCH: usize;
    const RECOMMENDATION_INTERVAL: u32;
}

pub trait SessionCallbacks<T>
where
    T: BackrollConfig,
{
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
