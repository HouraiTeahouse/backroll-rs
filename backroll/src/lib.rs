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

pub use backend::Session;
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

pub enum BackrollPlayer<T>
where
    T: BackrollConfig,
{
    Local,
    Spectator(BackrollPeer<T>),
    Remote(BackrollPeer<T>),
}

impl<T: BackrollConfig> BackrollPlayer<T> {
    pub fn peer(&self) -> Option<&BackrollPeer<T>> {
        match self {
            Self::Local => None,
            Self::Remote(ref peer) => Some(peer),
            Self::Spectator(ref peer) => Some(peer),
        }
    }

    pub fn peer_mut(&mut self) -> Option<&mut BackrollPeer<T>> {
        match self {
            Self::Local => None,
            Self::Remote(ref mut peer) => Some(peer),
            Self::Spectator(ref mut peer) => Some(peer),
        }
    }

    pub fn is_local(&self) -> bool {
        self.peer().is_none()
    }

    pub fn is_remote_player(&self) -> bool {
        if let Self::Remote(peer) = self {
            true
        } else {
            false
        }
    }

    pub fn is_spectator(&self) -> bool {
        if let Self::Spectator(peer) = self {
            true
        } else {
            false
        }
    }

    pub fn is_synchronized(&self) -> bool {
        if let Some(peer) = self.peer() {
            peer.state().is_running()
        } else {
            true
        }
    }

    pub fn send_input(&mut self, input: FrameInput<T::Input>) {
        if let Some(peer) = self.peer_mut() {
            peer.send_input(input);
        }
    }

    pub fn disconnect(&mut self) {
        if let Some(peer) = self.peer_mut() {
            peer.disconnect();
        }
    }

    pub fn set_disconnect_timeout(&mut self, timeout: Option<Duration>) {
        if let Some(peer) = self.peer_mut() {
            peer.set_disconnect_timeout(timeout);
        }
    }

    pub fn set_disconnect_notify_start(&mut self, timeout: Option<Duration>) {
        if let Some(peer) = self.peer_mut() {
            peer.set_disconnect_notify_start(timeout);
        }
    }

    pub fn get_network_stats(&self) -> Option<NetworkStats> {
        self.peer().map(|peer| peer.get_network_stats())
    }
}

pub trait BackrollConfig {
    type Input: Default + Eq + Clone + bytemuck::Pod;
    type State;

    const MAX_PLAYERS_PER_MATCH: usize;
    const RECOMMENDATION_INTERVAL: u32;
    const DEFAULT_DISCONNECT_TIMEOUT: Option<Duration>;
    const DEFAULT_DISCONNECT_NOTIFY_START: Option<Duration>;
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
