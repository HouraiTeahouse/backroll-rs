use std::{hash::Hash, time::Duration};
use thiserror::Error;

mod backend;
pub mod command;
mod input;
mod protocol;
mod sync;
mod time_sync;

pub use backend::*;
pub use backroll_transport as transport;
pub use input::GameInput;

// TODO(james7132): Generalize the executor for these.
pub(crate) use bevy_tasks::TaskPool;

/// The maximum number of players supported in a single game.
pub const MAX_PLAYERS: usize = 8;
// Approximately 2 seconds of frames.
const MAX_ROLLBACK_FRAMES: usize = 120;

type Frame = i32;
const NULL_FRAME: Frame = -1;

fn is_null(frame: Frame) -> bool {
    frame < 0
}

/// A handle for a player in a Backroll session.
#[derive(Copy, Clone, Debug)]
pub struct PlayerHandle(pub usize);

/// Players within a Backroll session.
#[derive(Clone)]
pub enum Player {
    /// The local player. Backroll currently only supports one local player per machine.
    Local,
    /// A remote player that is not on the local session.
    Remote(transport::Peer),
}

impl Player {
    pub(crate) fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

impl Default for Player {
    fn default() -> Self {
        Self::Local
    }

}

/// Compile time parameterization for Backroll sessions.
pub trait Config: 'static {
    /// The input type for a Backroll session. This is the only game-related data
    /// transmitted over the network.
    ///
    /// Reminder: Types implementing [Pod] may not have the same byte representation
    /// on platforms with different endianness. Backroll assumes that all players are
    /// running with the same endianness when encoding and decoding inputs. It may be
    /// worthwhile to ensure that all players are running with the same endianess.
    ///
    /// [Pod]: bytemuck::Pod
    type Input: PartialEq + bytemuck::Pod + bytemuck::Zeroable + Send + Sync;

    /// The save state type for the session. This type must be safe to send across
    /// threads and have a 'static lifetime. This type is also responsible for
    /// dropping any internal linked state via [Drop].
    ///
    /// [Drop]: std::ops::Drop
    type State: Clone + Hash + Send + Sync + 'static;
}

#[derive(Clone, Debug, Error)]
pub enum BackrollError {
    #[error("Multiple players ")]
    MultipleLocalPlayers,
    #[error("Action cannot be taken while in rollback.")]
    InRollback,
    #[error("The session has not been synchronized yet.")]
    NotSynchronized,
    #[error("The simulation has reached the prediction barrier.")]
    ReachedPredictionBarrier,
    #[error("Invalid player handle: {:?}", .0)]
    InvalidPlayer(PlayerHandle),
    #[error("Player already disconnected: {:?}", .0)]
    PlayerDisconnected(PlayerHandle),
}

pub type BackrollResult<T> = Result<T, BackrollError>;

#[derive(Clone, Debug, Default)]
/// Event that occurs during the course of a session.
pub struct NetworkStats {
    /// The round time trip duration between the local player and the
    /// remote.
    pub ping: Duration,
    /// The number of outgoing messages currently not sent.
    pub send_queue_len: usize,
    /// The number of incoming messages currently not processed.
    pub recv_queue_len: usize,
    /// The number of kilobytes sent per second, a rolling average.
    pub kbps_sent: u32,

    /// The local frame advantage relative to the associated peer.
    pub local_frames_behind: Frame,
    /// The remote frame advantage of the associated peer relative to the local player.
    pub remote_frames_behind: Frame,
}

#[derive(Clone, Debug)]
/// Event that occurs during the course of a session.
pub enum Event {
    /// A initial response packet from the remote player has been recieved.
    Connected(PlayerHandle),
    /// A response from a remote player has been recieved during the initial
    /// synchronization handshake.
    Synchronizing {
        player: PlayerHandle,
        count: u8,
        total: u8,
    },
    /// The initial synchronization handshake has been completed. The connection
    /// is considered live now.
    Synchronized(PlayerHandle),
    /// All remote peers are now synchronized, the session is can now start
    /// running.
    Running,
    /// The connection with a remote player has been disconnected.
    Disconnected(PlayerHandle),
    /// The local client is several frames ahead of all other peers. Might need
    /// to stall a few frames to allow others to catch up.
    TimeSync { frames_ahead: u8 },
    /// The connection with a remote player has been temporarily interrupted.
    ConnectionInterrupted {
        player: PlayerHandle,
        disconnect_timeout: Duration,
    },
    /// The connection with a remote player has been resumed after being interrupted.
    ConnectionResumed(PlayerHandle),
}
