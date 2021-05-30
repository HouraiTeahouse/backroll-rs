use super::{BackrollError, BackrollPlayer, BackrollPlayerHandle, BackrollResult};
use crate::{
    input::{FrameInput, GameInput},
    is_null,
    protocol::{BackrollPeer, BackrollPeerConfig, ConnectionStatus, Event},
    sync::{self, BackrollSync},
    transport::Peer,
    BackrollConfig, BackrollEvent, Frame, NetworkStats, SessionCallbacks, TaskPool,
};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

const RECOMMENDATION_INTERVAL: Frame = 240;
const DEFAULT_DISCONNECT_TIMEOUT: Duration = Duration::from_millis(5000);
const DEFAULT_DISCONNECT_NOTIFY_START: Duration = Duration::from_millis(750);

enum Player<T>
where
    T: BackrollConfig,
{
    Local,
    Remote {
        peer: BackrollPeer<T>,
        rx: async_channel::Receiver<Event<T::Input>>,
    },
    Spectator {
        peer: BackrollPeer<T>,
        rx: async_channel::Receiver<Event<T::Input>>,
    },
}

impl<T: BackrollConfig> Player<T> {
    pub fn new(
        queue: usize,
        player: &BackrollPlayer,
        builder: &P2PSessionBuilder<T>,
        connect: Arc<[RwLock<ConnectionStatus>]>,
        task_pool: TaskPool,
    ) -> Self {
        match player {
            BackrollPlayer::Local => Self::Local,
            BackrollPlayer::Remote(peer) => {
                let (peer, rx) = Self::make_peer(queue, peer, builder, connect, task_pool);
                Player::<T>::Remote { peer, rx }
            }
            BackrollPlayer::Spectator(peer) => {
                let (peer, rx) = Self::make_peer(queue, peer, builder, connect, task_pool);
                Player::<T>::Spectator { peer, rx }
            }
        }
    }

    fn make_peer(
        queue: usize,
        peer: &Peer,
        builder: &P2PSessionBuilder<T>,
        connect: Arc<[RwLock<ConnectionStatus>]>,
        pool: TaskPool,
    ) -> (BackrollPeer<T>, async_channel::Receiver<Event<T::Input>>) {
        let config = BackrollPeerConfig {
            peer: peer.clone(),
            disconnect_timeout: builder.disconnect_timeout,
            disconnect_notify_start: builder.disconnect_notify_start,
            task_pool: pool,
        };

        BackrollPeer::<T>::new(queue, config, connect)
    }

    pub fn peer(&self) -> Option<&BackrollPeer<T>> {
        match self {
            Self::Local => None,
            Self::Remote { ref peer, .. } => Some(peer),
            Self::Spectator { ref peer, .. } => Some(peer),
        }
    }

    pub fn is_local(&self) -> bool {
        self.peer().is_none()
    }

    pub fn is_remote_player(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    pub fn is_spectator(&self) -> bool {
        matches!(self, Self::Spectator { .. })
    }

    pub fn is_synchronized(&self) -> bool {
        if let Some(peer) = self.peer() {
            peer.is_running()
        } else {
            true
        }
    }

    pub fn send_input(&mut self, input: FrameInput<T::Input>) {
        if let Some(peer) = self.peer() {
            let _ = peer.send_input(input);
        }
    }

    pub fn disconnect(&mut self) {
        if let Some(peer) = self.peer() {
            peer.disconnect();
        }
    }

    pub fn get_network_stats(&self) -> Option<NetworkStats> {
        self.peer().map(|peer| peer.get_network_stats())
    }
}

pub struct P2PSessionBuilder<T>
where
    T: BackrollConfig,
{
    players: Vec<BackrollPlayer>,
    disconnect_timeout: Duration,
    disconnect_notify_start: Duration,
    marker_: std::marker::PhantomData<T>,
}

impl<T> P2PSessionBuilder<T>
where
    T: BackrollConfig,
{
    /// Creates a new builder. Identical to [P2PSession::build].
    ///
    /// [P2PSession]: self::P2PSession
    pub fn new() -> Self {
        Self {
            players: Vec::new(),
            disconnect_timeout: DEFAULT_DISCONNECT_TIMEOUT,
            disconnect_notify_start: DEFAULT_DISCONNECT_NOTIFY_START,
            marker_: Default::default(),
        }
    }

    /// Sets how long the client will wait for a packet from a remote player
    /// before considering the connection disconnected. Defaults to 5000ms.
    pub fn with_disconnect_timeout(mut self, timeout: Duration) -> Self {
        self.disconnect_timeout = timeout;
        self
    }

    /// Sets how long the client will wait for a packet from a remote player before
    /// before firing a [BackrollEvent::ConnectionInterrupted] event. Defaults to 750ms.
    ///
    /// [BackrollEvent]: crate::BackrollEvent
    pub fn with_disconnect_notify_start(mut self, timeout: Duration) -> Self {
        self.disconnect_timeout = timeout;
        self
    }

    /// Adds a player to the session and returns the corresponding handle.
    pub fn add_player(&mut self, player: BackrollPlayer) -> BackrollPlayerHandle {
        let id = self.players.len();
        self.players.push(player);
        BackrollPlayerHandle(id)
    }

    /// Constructs and starts the P2PSession. Consumes the builder.
    ///
    /// # Errors
    /// Returns [BackrollError::MultipleLocalPlayers] if there are multiple local players.
    /// Backroll currently only supports one local player.
    ///
    /// [BackrolLError]: crate::BackrolLError
    pub fn start(self, pool: TaskPool) -> BackrollResult<P2PSession<T>> {
        P2PSession::new_internal(self, pool)
    }
}

struct P2PSessionRef<T>
where
    T: BackrollConfig,
{
    sync: BackrollSync<T>,
    players: Vec<Player<T>>,

    synchronizing: bool,
    next_recommended_sleep: Frame,
    next_spectator_frame: Frame,

    local_connect_status: Arc<[RwLock<ConnectionStatus>]>,
}

impl<T: BackrollConfig> P2PSessionRef<T> {
    fn players(&self) -> impl Iterator<Item = &BackrollPeer<T>> {
        self.players
            .iter()
            .filter(|player| player.is_remote_player())
            .map(|player| player.peer())
            .flatten()
    }

    fn spectators(&self) -> impl Iterator<Item = &BackrollPeer<T>> {
        self.players
            .iter()
            .filter(|player| player.is_spectator())
            .map(|player| player.peer())
            .flatten()
    }

    fn player_handle_to_queue(&self, player: BackrollPlayerHandle) -> BackrollResult<usize> {
        let offset = player.0;
        if offset >= self.sync.player_count() {
            return Err(BackrollError::InvalidPlayer(player));
        }
        Ok(offset)
    }

    fn check_initial_sync(&mut self, callbacks: &mut impl SessionCallbacks<T>) {
        if self.synchronizing && self.is_synchronized() {
            callbacks.handle_event(BackrollEvent::Running);
            self.synchronizing = false;
        }
    }

    fn disconnect_player_queue(
        &mut self,
        callbacks: &mut impl SessionCallbacks<T>,
        queue: usize,
        syncto: Frame,
    ) {
        let frame_count = self.sync.frame_count();

        self.players[queue].disconnect();

        info!("Changing queue {} local connect status for last frame from {} to {} on disconnect request (current: {}).",
         queue, self.local_connect_status[queue].read().last_frame, syncto, frame_count);

        {
            let mut status = self.local_connect_status[queue].write();
            status.disconnected = true;
            status.last_frame = syncto;
        }

        if syncto < frame_count {
            info!(
                "Adjusting simulation to account for the fact that {} disconnected @ {}.",
                queue, syncto
            );
            self.sync.adjust_simulation(callbacks, syncto);
            info!("Finished adjusting simulation.");
        }

        callbacks.handle_event(BackrollEvent::Disconnected(BackrollPlayerHandle(queue)));

        self.check_initial_sync(callbacks);
    }

    fn do_poll(&mut self, callbacks: &mut impl SessionCallbacks<T>) {
        if self.sync.in_rollback() || self.synchronizing {
            return;
        }

        self.sync.check_simulation(callbacks);

        // notify all of our endpoints of their local frame number for their
        // next connection quality report
        let current_frame = self.sync.frame_count();
        for player in self.players() {
            player.set_local_frame_number(current_frame);
        }

        let min_frame = if self.players().count() <= 2 {
            self.poll_2_players(callbacks)
        } else {
            self.poll_n_players(callbacks)
        };

        info!("last confirmed frame in p2p backend is {}.", min_frame);
        if min_frame >= 0 {
            debug_assert!(min_frame != Frame::MAX);
            if self.spectators().next().is_some() {
                while self.next_spectator_frame <= min_frame {
                    info!("pushing frame {} to spectators.", self.next_spectator_frame);

                    // FIXME(james7132): Spectator input sending.
                    // let (input, _)= self.sync.get_confirmed_inputs(self.next_spectator_frame);
                    // for spectator in self.spectators() {
                    //     spectator.send_input(input);
                    // }
                    self.next_spectator_frame += 1;
                }
            }
            info!("setting confirmed frame in sync to {}.", min_frame);
            self.sync.set_last_confirmed_frame(min_frame);
        }

        // send timesync notifications if now is the proper time
        if current_frame > self.next_recommended_sleep {
            let interval = self
                .players()
                .map(|player| player.recommend_frame_delay())
                .max();
            if let Some(interval) = interval {
                callbacks.handle_event(BackrollEvent::TimeSync {
                    frames_ahead: interval as u8,
                });
                self.next_recommended_sleep = current_frame + RECOMMENDATION_INTERVAL;
            }
        }
    }

    fn poll_2_players(&mut self, callbacks: &mut impl SessionCallbacks<T>) -> Frame {
        // discard confirmed frames as appropriate
        let mut min_frame = Frame::MAX;
        for i in 0..self.players.len() {
            let player = &self.players[i];
            let mut queue_connected = true;
            if let Some(peer) = player.peer() {
                if peer.is_running() {
                    queue_connected = !peer.get_peer_connect_status(i).disconnected;
                }
            }
            let local_status = self.local_connect_status[i].read().clone();
            if !local_status.disconnected {
                min_frame = std::cmp::min(local_status.last_frame, min_frame);
            }
            info!(
                "local endp: connected = {}, last_received = {}, total_min_confirmed = {}.",
                !local_status.disconnected, local_status.last_frame, min_frame
            );
            if !queue_connected && !local_status.disconnected {
                info!("disconnecting player {} by remote request.", i);
                self.disconnect_player_queue(callbacks, i, min_frame);
            }
            info!("min_frame = {}.", min_frame);
        }
        min_frame
    }

    fn poll_n_players(&mut self, callbacks: &mut impl SessionCallbacks<T>) -> Frame {
        // discard confirmed frames as appropriate
        let mut min_frame = Frame::MAX;
        for queue in 0..self.players.len() {
            let mut queue_connected = true;
            let mut queue_min_confirmed = Frame::MAX;
            info!("considering queue {}.", queue);
            for (i, player) in self.players.iter().enumerate() {
                // we're going to do a lot of logic here in consideration of endpoint i.
                // keep accumulating the minimum confirmed point for all n*n packets and
                // throw away the rest.
                if player.peer().map(|peer| peer.is_running()).unwrap_or(false) {
                    let peer = player.peer().unwrap();
                    let status = peer.get_peer_connect_status(queue);
                    queue_connected = queue_connected && !status.disconnected;
                    queue_min_confirmed = std::cmp::min(status.last_frame, queue_min_confirmed);
                    info!("endpoint {}: connected = {}, last_received = {}, queue_min_confirmed = {}.", 
                          i, queue_connected, status.last_frame, queue_min_confirmed);
                } else {
                    info!("endpoint {}: ignoring... not running.", i);
                }
            }

            let local_status = self.local_connect_status[queue].read().clone();
            // merge in our local status only if we're still connected!
            if !local_status.disconnected {
                queue_min_confirmed = std::cmp::min(local_status.last_frame, queue_min_confirmed);
            }
            info!(
                "local endp: connected = {}, last_received = {}, queue_min_confirmed = {}.",
                !local_status.disconnected, local_status.last_frame, queue_min_confirmed
            );

            if queue_connected {
                min_frame = std::cmp::min(queue_min_confirmed, min_frame);
            } else {
                // check to see if this disconnect notification is further back than we've been before.  If
                // so, we need to re-adjust.  This can happen when we detect our own disconnect at frame n
                // and later receive a disconnect notification for frame n-1.
                if !local_status.disconnected || local_status.last_frame > queue_min_confirmed {
                    info!("disconnecting queue {} by remote request.", queue);
                    self.disconnect_player_queue(callbacks, queue, queue_min_confirmed);
                }
            }
            info!("min_frame = {}.", min_frame);
        }
        return min_frame;
    }

    fn is_synchronized(&self) -> bool {
        // Check to see if everyone is now synchronized.  If so,
        // go ahead and tell the client that we're ok to accept input.
        for (i, player) in self.players.iter().enumerate() {
            if !player.is_local()
                && !player.is_synchronized()
                && !self.local_connect_status[i].read().disconnected
            {
                return false;
            }
        }
        true
    }
}

pub struct P2PSession<T>(Arc<RwLock<P2PSessionRef<T>>>)
where
    T: BackrollConfig;

impl<T: BackrollConfig> Clone for P2PSession<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: BackrollConfig> P2PSession<T> {
    pub fn build() -> P2PSessionBuilder<T> {
        P2PSessionBuilder::new()
    }

    fn new_internal(builder: P2PSessionBuilder<T>, task_pool: TaskPool) -> BackrollResult<Self> {
        let local_player_count = builder
            .players
            .iter()
            .filter(|player| player.is_local())
            .count();
        if local_player_count > 1 {
            return Err(BackrollError::MultipleLocalPlayers);
        }
        let player_count = builder.players.len();
        let connect_status: Vec<RwLock<ConnectionStatus>> =
            (0..player_count).map(|_| Default::default()).collect();
        let connect_status: Arc<[RwLock<ConnectionStatus>]> = connect_status.into();

        let players: Vec<Player<T>> = builder
            .players
            .iter()
            .enumerate()
            .map(|(i, player)| {
                Player::<T>::new(
                    i,
                    player,
                    &builder,
                    connect_status.clone(),
                    task_pool.clone(),
                )
            })
            .collect();

        let synchronizing = players.iter().any(|player| !player.is_local());
        let config = sync::Config { player_count };
        let sync = BackrollSync::<T>::new(config, connect_status.clone());
        Ok(Self(Arc::new(RwLock::new(P2PSessionRef::<T> {
            sync,
            players,
            synchronizing,
            next_recommended_sleep: 0,
            next_spectator_frame: 0,
            local_connect_status: connect_status,
        }))))
    }

    /// Gets the number of players in the current session. This includes
    /// users that are already disconnected.
    pub fn player_count(&self) -> usize {
        self.0.read().sync.player_count()
    }

    /// Checks if the session currently in the middle of a rollback.
    pub fn in_rollback(&self) -> bool {
        self.0.read().sync.in_rollback()
    }

    /// Gets the current frame of the game.
    pub fn current_frame(&self) -> Frame {
        self.0.read().sync.frame_count()
    }

    pub fn local_players(&self) -> Vec<BackrollPlayerHandle> {
        self.0
            .read()
            .players
            .iter()
            .enumerate()
            .filter(|(_, player)| player.is_local())
            .map(|(i, _)| BackrollPlayerHandle(i))
            .collect()
    }

    pub fn remote_players(&self) -> Vec<BackrollPlayerHandle> {
        self.0
            .read()
            .players
            .iter()
            .enumerate()
            .filter(|(_, player)| player.is_remote_player())
            .map(|(i, _)| BackrollPlayerHandle(i))
            .collect()
    }

    /// Checks if all remote players are synchronized. If all players are
    /// local, this will always return true.
    pub fn is_synchronized(&self) -> bool {
        self.0.read().is_synchronized()
    }

    /// Adds a local input for the current frame. This will register the input in the local
    /// input queues, as well as queue the input to be sent to all remote players. If called multiple
    /// times for the same player without advancing the session with [advance_frame], the previously
    /// queued input for the frame will be overwritten.
    ///
    /// For a corrrect simulation, this must be called on all local players every frame before calling
    /// [advance_frame].
    ///
    /// # Errors
    /// Returns [BackrollError::InRollback] if the session is currently in the middle of a rollback.
    ///
    /// Returns [BackrollError::NotSynchronized] if the all of the remote peers have not yet
    /// synchornized.
    ///
    /// Returns [BackrollError::InvalidPlayer] if the provided player handle does not point a vali
    /// player.
    ///
    /// [BackrollError]: crate::BackrollError
    /// [advance_frame]: self::P2PSession::advance_frame
    pub fn add_local_input(
        &self,
        player: BackrollPlayerHandle,
        input: T::Input,
    ) -> BackrollResult<()> {
        let mut session_ref = self.0.write();
        if session_ref.sync.in_rollback() {
            return Err(BackrollError::InRollback);
        }
        if session_ref.synchronizing {
            return Err(BackrollError::NotSynchronized);
        }

        let queue = session_ref.player_handle_to_queue(player)?;
        let frame = session_ref.sync.add_local_input(queue, input)?;
        if !is_null(frame) {
            for player in session_ref.players.iter_mut() {
                player.send_input(FrameInput::<T::Input> { frame, input });
            }
        }

        Ok(())
    }

    /// Advances the game simulation by a single frame. This will call [SessionCallbacks::advance_frame]
    /// then check if the simulation is consistent with the inputs sent by remote players. If not, a
    /// rollback will be triggered, and the game will be saved and resimulated from the point of rollback.
    ///
    /// For a corrrect simulation, [add_local_input] must be called on all local players every frame before
    /// calling this.
    ///
    /// [SessionCallbacks]: crate::SessionCallbacks
    /// [add_local_input]: self::P2PSession::add_local_input
    pub fn advance_frame(&self, callbacks: &mut impl SessionCallbacks<T>) {
        let mut session_ref = self.0.write();
        info!("End of frame ({})...", session_ref.sync.frame_count());
        session_ref.sync.increment_frame(callbacks);
        session_ref.do_poll(callbacks);
    }

    /// Disconnects a player from the game.
    ///
    /// If called on a local player, this will disconnect the client from all remote peers.
    ///
    /// If called on a remote player, this will disconnect the connection with only that player.
    ///
    /// # Errors
    /// Returns [BackrollError::InvalidPlayer] if the provided player handle does not point a vali
    /// player.
    ///
    /// Returns [BackrollError::PlayerDisconnected] if the provided player is already disconnected.
    pub fn disconnect_player(
        &self,
        callbacks: &mut impl SessionCallbacks<T>,
        player: BackrollPlayerHandle,
    ) -> BackrollResult<()> {
        let mut session_ref = self.0.write();
        let queue = session_ref.player_handle_to_queue(player)?;
        if session_ref.local_connect_status[queue].read().disconnected {
            return Err(BackrollError::PlayerDisconnected(player));
        }

        let last_frame = session_ref.local_connect_status[queue].read().last_frame;
        if session_ref.players[queue].is_local() {
            // The player is local. This should disconnect the local player from the rest
            // of the game. All other players need to be disconnected.
            // that if the endpoint is not initalized, this must be the local player.
            let current_frame = session_ref.sync.frame_count();
            info!(
                "Disconnecting local player {} at frame {} by user request.",
                queue, last_frame
            );
            for i in 0..session_ref.players.len() {
                if !session_ref.players[i].is_local() {
                    session_ref.disconnect_player_queue(callbacks, i, current_frame);
                }
            }
        } else {
            info!(
                "Disconnecting queue {} at frame {} by user request.",
                queue, last_frame
            );
            session_ref.disconnect_player_queue(callbacks, queue, last_frame);
        }
        Ok(())
    }

    /// Gets network statistics with a remote player.
    ///
    /// # Errors
    /// Returns [BackrollError::InvalidPlayer] if the provided player handle does not point a vali
    /// player.
    pub fn get_network_stats(&self, player: BackrollPlayerHandle) -> BackrollResult<NetworkStats> {
        let session_ref = self.0.read();
        let queue = session_ref.player_handle_to_queue(player)?;
        Ok(session_ref.players[queue]
            .get_network_stats()
            .unwrap_or_else(Default::default))
    }

    /// Sets the frame delay for a given player.
    ///
    /// # Errors
    /// Returns [BackrollError::InvalidPlayer] if the provided player handle does not point a vali
    /// player.
    pub fn set_frame_delay(
        &self,
        player: BackrollPlayerHandle,
        delay: Frame,
    ) -> BackrollResult<()> {
        let mut session_ref = self.0.write();
        let queue = session_ref.player_handle_to_queue(player)?;
        session_ref.sync.set_frame_delay(queue, delay);
        Ok(())
    }
}
