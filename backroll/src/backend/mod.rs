use super::{
    input::{FrameInput, GameInput},
    BackrollConfig, BackrollError, BackrollPlayer, BackrollPlayerHandle, BackrollResult, Frame,
    NetworkStats,
};
use std::time::Duration;

mod p2p;
mod sync_test;

pub use p2p::{P2PSession, P2PSessionBuilder};
pub use sync_test::SyncTestBackend;
