use super::{
    input::{FrameInput, GameInput},
    BackrollConfig, BackrollError, BackrollPlayer, BackrollPlayerHandle, BackrollResult, Frame,
    NetworkStats,
};
use std::time::Duration;

mod p2p;
mod sync_test;

pub use p2p::P2PBackend;
pub use sync_test::SyncTestBackend;

pub struct Session<T>(Backend<T>)
where
    T: BackrollConfig;

enum Backend<T>
where
    T: BackrollConfig,
{
    P2P(P2PBackend<T>),
    SyncTest(SyncTestBackend),
}

impl<T: BackrollConfig> Session<T> {
    /// A periodic poll for network events. This should be called periodically
    /// (i.e. once a frame during Vsync) to ensure that events are being
    /// handled.
    pub fn do_poll(&mut self) {
        match &mut self.0 {
            Backend::P2P(p2p) => p2p.do_poll(),
            _ => {}
        }
    }

    pub fn add_player(
        &mut self,
        player: BackrollPlayer<T>,
    ) -> BackrollResult<BackrollPlayerHandle> {
        match &mut self.0 {
            Backend::P2P(p2p) => p2p.add_player(player),
            _ => Err(BackrollError::UnsupportedOperation),
        }
    }

    pub fn add_local_input(
        &mut self,
        player: BackrollPlayerHandle,
        input: FrameInput<T::Input>,
    ) -> BackrollResult<()> {
        match &mut self.0 {
            Backend::P2P(p2p) => p2p.add_local_input(player, input),
            _ => Err(BackrollError::UnsupportedOperation),
        }
    }

    pub fn sync_input(&self) -> BackrollResult<(GameInput<T::Input>, u32)> {
        match &self.0 {
            Backend::P2P(p2p) => p2p.sync_input(),
            _ => Err(BackrollError::UnsupportedOperation),
        }
    }

    pub fn increment_frame(&mut self) -> BackrollResult<()> {
        match &mut self.0 {
            Backend::P2P(p2p) => p2p.increment_frame(),
            _ => Ok(()),
        }
    }

    pub fn disconnect_player(&mut self, player: BackrollPlayerHandle) -> BackrollResult<()> {
        match &mut self.0 {
            Backend::P2P(ref mut p2p) => p2p.disconnect_player(player),
            _ => Err(BackrollError::UnsupportedOperation),
        }
    }

    pub fn get_network_stats(&self, handle: BackrollPlayerHandle) -> BackrollResult<NetworkStats> {
        match self.0 {
            _ => Err(BackrollError::UnsupportedOperation),
        }
    }

    pub fn set_frame_delay(
        &mut self,
        player: BackrollPlayerHandle,
        delay: Frame,
    ) -> BackrollResult<()> {
        match &mut self.0 {
            Backend::P2P(p2p) => p2p.set_frame_delay(player, delay),
            _ => Err(BackrollError::UnsupportedOperation),
        }
    }

    pub fn set_disconnect_timeout(&mut self, timeout: Option<Duration>) -> BackrollResult<()> {
        match &mut self.0 {
            Backend::P2P(p2p) => p2p.set_disconnect_timeout(timeout),
            _ => Err(BackrollError::UnsupportedOperation),
        }
    }

    pub fn set_disconnect_notify_start(&mut self, timeout: Option<Duration>) -> BackrollResult<()> {
        match &mut self.0 {
            Backend::P2P(p2p) => p2p.set_disconnect_notify_start(timeout),
            _ => Err(BackrollError::UnsupportedOperation),
        }
    }
}
