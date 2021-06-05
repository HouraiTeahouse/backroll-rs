use super::{BackrollError, BackrollResult, Player, PlayerHandle};
use crate::{
    command::Commands,
    input::FrameInput,
    is_null,
    protocol::{ConnectionStatus, Event as ProtocolEvent, Peer, PeerConfig},
    sync::{self, Sync},
    transport::Peer as TransportPeer,
    Command, Config, Event, Frame, NetworkStats, TaskPool,
};
use async_channel::TryRecvError;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

const RECOMMENDATION_INTERVAL: Frame = 240;
const DEFAULT_FRAME_DELAY: Frame = 3;
const DEFAULT_DISCONNECT_TIMEOUT: Duration = Duration::from_millis(5000);
const DEFAULT_DISCONNECT_NOTIFY_START: Duration = Duration::from_millis(750);

enum PlayerType<T>
where
    T: Config,
{
    Local,
    Remote {
        peer: Peer<T>,
        rx: async_channel::Receiver<ProtocolEvent<T::Input>>,
    },
    Spectator {
        peer: Peer<T>,
        rx: async_channel::Receiver<ProtocolEvent<T::Input>>,
    },
}

impl<T: Config> Clone for PlayerType<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Local => Self::Local,
            Self::Remote { peer, rx } => Self::Remote {
                peer: peer.clone(),
                rx: rx.clone(),
            },
            Self::Spectator { peer, rx } => Self::Spectator {
                peer: peer.clone(),
                rx: rx.clone(),
            },
        }
    }
}

impl<T: Config> PlayerType<T> {
    pub fn new(
        queue: usize,
        player: &Player,
        builder: &P2PSessionBuilder<T>,
        connect: Arc<[RwLock<ConnectionStatus>]>,
        task_pool: TaskPool,
    ) -> Self {
        match player {
            Player::Local => Self::Local,
            Player::Remote(peer) => {
                let (peer, rx) = Self::make_peer(queue, peer, builder, connect, task_pool);
                PlayerType::<T>::Remote { peer, rx }
            }
            Player::Spectator(peer) => {
                let (peer, rx) = Self::make_peer(queue, peer, builder, connect, task_pool);
                PlayerType::<T>::Spectator { peer, rx }
            }
        }
    }

    fn make_peer(
        queue: usize,
        peer: &TransportPeer,
        builder: &P2PSessionBuilder<T>,
        connect: Arc<[RwLock<ConnectionStatus>]>,
        pool: TaskPool,
    ) -> (Peer<T>, async_channel::Receiver<ProtocolEvent<T::Input>>) {
        let config = PeerConfig {
            peer: peer.clone(),
            disconnect_timeout: builder.disconnect_timeout,
            disconnect_notify_start: builder.disconnect_notify_start,
            task_pool: pool,
        };

        Peer::<T>::new(queue, config, connect)
    }

    pub fn peer(&self) -> Option<&Peer<T>> {
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
    T: Config,
{
    players: Vec<Player>,
    frame_delay: Frame,
    disconnect_timeout: Duration,
    disconnect_notify_start: Duration,
    marker_: std::marker::PhantomData<T>,
}

impl<T> Default for P2PSessionBuilder<T>
where
    T: Config,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T> P2PSessionBuilder<T>
where
    T: Config,
{
    /// Creates a new builder. Identical to [P2PSession::build].
    ///
    /// [P2PSession]: self::P2PSession
    pub fn new() -> Self {
        Self {
            players: Vec::new(),
            frame_delay: DEFAULT_FRAME_DELAY,
            disconnect_timeout: DEFAULT_DISCONNECT_TIMEOUT,
            disconnect_notify_start: DEFAULT_DISCONNECT_NOTIFY_START,
            marker_: Default::default(),
        }
    }

    /// Sets how much frame delay is used for all active players.
    /// Defaults to 3 frames.
    pub fn with_frame_delay(mut self, frame_delay: Frame) -> Self {
        self.frame_delay = frame_delay;
        self
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
    pub fn add_player(&mut self, player: Player) -> PlayerHandle {
        let id = self.players.len();
        self.players.push(player);
        PlayerHandle(id)
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
    T: Config,
{
    sync: Sync<T>,
    players: Vec<PlayerType<T>>,

    synchronizing: bool,
    next_recommended_sleep: Frame,
    next_spectator_frame: Frame,

    local_connect_status: Arc<[RwLock<ConnectionStatus>]>,
}

impl<T: Config> P2PSessionRef<T> {
    fn players(&self) -> impl Iterator<Item = &Peer<T>> {
        self.players
            .iter()
            .filter(|player| player.is_remote_player())
            .map(|player| player.peer())
            .flatten()
    }

    fn spectators(&self) -> impl Iterator<Item = &Peer<T>> {
        self.players
            .iter()
            .filter(|player| player.is_spectator())
            .map(|player| player.peer())
            .flatten()
    }

    fn player_handle_to_queue(&self, player: PlayerHandle) -> BackrollResult<usize> {
        let offset = player.0;
        if offset >= self.sync.player_count() {
            return Err(BackrollError::InvalidPlayer(player));
        }
        Ok(offset)
    }

    fn check_initial_sync(&mut self, commands: &mut Commands<T>) {
        if self.synchronizing && self.is_synchronized() {
            commands.push(Command::Event(Event::Running));
            self.synchronizing = false;
        }
    }

    fn disconnect_player(
        &mut self,
        commands: &mut Commands<T>,
        player: PlayerHandle,
    ) -> BackrollResult<()> {
        let queue = self.player_handle_to_queue(player)?;
        let (last_frame, disconnected) = {
            let status = self.local_connect_status[queue].read();
            (status.last_frame, status.disconnected)
        };

        if disconnected {
            return Err(BackrollError::PlayerDisconnected(player));
        }

        if self.players[queue].is_local() {
            // The player is local. This should disconnect the local player from the rest
            // of the game. All other players need to be disconnected.
            // that if the endpoint is not initalized, this must be the local player.
            let current_frame = self.sync.frame_count();
            info!(
                "Disconnecting local player {} at frame {} by user request.",
                queue, last_frame
            );
            for i in 0..self.players.len() {
                if !self.players[i].is_local() {
                    self.disconnect_player_queue(commands, i, current_frame);
                }
            }
        } else {
            info!(
                "Disconnecting queue {} at frame {} by user request.",
                queue, last_frame
            );
            self.disconnect_player_queue(commands, queue, last_frame);
        }
        Ok(())
    }

    fn disconnect_player_queue(&mut self, commands: &mut Commands<T>, queue: usize, syncto: Frame) {
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
            self.sync.adjust_simulation(commands, syncto);
            info!("Finished adjusting simulation.");
        }

        commands.push(Command::Event(Event::Disconnected(PlayerHandle(queue))));

        self.check_initial_sync(commands);
    }

    fn flush_events(&mut self, commands: &mut Commands<T>) {
        for (queue, player) in self.players.clone().iter().enumerate() {
            if let PlayerType::<T>::Remote { rx, .. } = player {
                self.flush_peer_events(commands, queue, rx.clone());
            }
        }
    }

    fn flush_peer_events(
        &mut self,
        commands: &mut Commands<T>,
        queue: usize,
        rx: async_channel::Receiver<ProtocolEvent<T::Input>>,
    ) {
        loop {
            match rx.try_recv() {
                Ok(evt) => self.handle_event(commands, queue, evt),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => {
                    self.disconnect_player(commands, PlayerHandle(queue))
                        .expect("Disconnecting should not error on closing connection");
                    break;
                }
            }
        }
    }

    fn handle_event(
        &mut self,
        commands: &mut Commands<T>,
        queue: usize,
        evt: ProtocolEvent<T::Input>,
    ) {
        let player = PlayerHandle(queue);
        match evt {
            ProtocolEvent::<T::Input>::Connected => {
                commands.push(Command::Event(Event::Connected(PlayerHandle(queue))));
            }
            ProtocolEvent::<T::Input>::Synchronizing { total, count } => {
                commands.push(Command::Event(Event::Synchronizing {
                    player,
                    total,
                    count,
                }));
            }
            ProtocolEvent::<T::Input>::Inputs(inputs) => {
                let mut status = self.local_connect_status[queue].write();
                if status.disconnected {
                    return;
                }

                for input in inputs {
                    let current_remote_frame = status.last_frame;
                    let new_remote_frame = input.frame;
                    debug_assert!(
                        crate::is_null(current_remote_frame)
                            || new_remote_frame == (current_remote_frame + 1)
                    );
                    self.sync.add_remote_input(queue, input);

                    // Notify the other endpoints which frame we received from a peer
                    debug!(
                        "setting remote connect status for queue {} to {}",
                        queue, new_remote_frame
                    );

                    status.last_frame = new_remote_frame;
                }
            }
            ProtocolEvent::<T::Input>::Synchronized => {
                commands.push(Command::Event(Event::Synchronized(player)));
                self.check_initial_sync(commands);
            }
            ProtocolEvent::<T::Input>::NetworkInterrupted { disconnect_timeout } => {
                commands.push(Command::Event(Event::ConnectionInterrupted {
                    player,
                    disconnect_timeout,
                }));
            }
            ProtocolEvent::<T::Input>::NetworkResumed => {
                commands.push(Command::Event(Event::Synchronized(player)));
            }
        }
    }

    fn do_poll(&mut self, commands: &mut Commands<T>) {
        if self.sync.in_rollback() {
            return;
        }

        self.flush_events(commands);

        if self.synchronizing {
            return;
        }

        self.sync.check_simulation(commands);

        // notify all of our endpoints of their local frame number for their
        // next connection quality report
        let current_frame = self.sync.frame_count();
        for player in self.players() {
            player.set_local_frame_number(current_frame);
        }

        let remote_player_count = self
            .players
            .iter()
            .filter(|player| !player.is_local())
            .count();

        let min_frame = if remote_player_count == 0 {
            current_frame
        } else if self.players().count() <= 2 {
            self.poll_2_players(commands)
        } else {
            self.poll_n_players(commands)
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
                commands.push(Command::Event(Event::TimeSync {
                    frames_ahead: interval as u8,
                }));
                self.next_recommended_sleep = current_frame + RECOMMENDATION_INTERVAL;
            }
        }
    }

    fn poll_2_players(&mut self, commands: &mut Commands<T>) -> Frame {
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
                self.disconnect_player_queue(commands, i, min_frame);
            }
            info!("min_frame = {}.", min_frame);
        }
        min_frame
    }

    fn poll_n_players(&mut self, commands: &mut Commands<T>) -> Frame {
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
                    self.disconnect_player_queue(commands, queue, queue_min_confirmed);
                }
            }
            info!("min_frame = {}.", min_frame);
        }
        min_frame
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
    T: Config;

impl<T: Config> Clone for P2PSession<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Config> P2PSession<T> {
    pub fn build() -> P2PSessionBuilder<T> {
        P2PSessionBuilder::new()
    }

    fn new_internal(builder: P2PSessionBuilder<T>, task_pool: TaskPool) -> BackrollResult<Self> {
        let local_player_count = builder
            .players
            .iter()
            .filter(|player| player.is_local())
            .count();
        let remote_player_count = builder.players.len() - local_player_count;

        if local_player_count > 1 && remote_player_count > 1 {
            return Err(BackrollError::MultipleLocalPlayers);
        }
        let player_count = builder.players.len();
        let connect_status: Vec<RwLock<ConnectionStatus>> =
            (0..player_count).map(|_| Default::default()).collect();
        let connect_status: Arc<[RwLock<ConnectionStatus>]> = connect_status.into();

        let players: Vec<PlayerType<T>> = builder
            .players
            .iter()
            .enumerate()
            .map(|(i, player)| {
                PlayerType::<T>::new(
                    i,
                    player,
                    &builder,
                    connect_status.clone(),
                    task_pool.clone(),
                )
            })
            .collect();

        let synchronizing = players.iter().any(|player| !player.is_local());
        let config = sync::PlayerConfig {
            player_count,
            frame_delay: builder.frame_delay,
        };
        let sync = Sync::<T>::new(config, connect_status.clone());
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

    pub fn local_players(&self) -> Vec<PlayerHandle> {
        self.0
            .read()
            .players
            .iter()
            .enumerate()
            .filter(|(_, player)| player.is_local())
            .map(|(i, _)| PlayerHandle(i))
            .collect()
    }

    pub fn remote_players(&self) -> Vec<PlayerHandle> {
        self.0
            .read()
            .players
            .iter()
            .enumerate()
            .filter(|(_, player)| player.is_remote_player())
            .map(|(i, _)| PlayerHandle(i))
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
    /// # Panics
    /// This function will panic if the player is not a local player.
    ///
    /// [BackrollError]: crate::BackrollError
    /// [advance_frame]: self::P2PSession::advance_frame
    pub fn add_local_input(&self, player: PlayerHandle, input: T::Input) -> BackrollResult<()> {
        let mut session_ref = self.0.write();
        if session_ref.sync.in_rollback() {
            return Err(BackrollError::InRollback);
        }
        if session_ref.synchronizing {
            return Err(BackrollError::NotSynchronized);
        }

        let queue = session_ref.player_handle_to_queue(player)?;
        assert!(
            session_ref.players[queue].is_local(),
            "{:?} is not a local player!",
            player
        );
        let frame = session_ref.sync.add_local_input(queue, input)?;
        if !is_null(frame) {
            // Update the local connect status state to indicate that we've got a
            // confirmed local frame for this player.  this must come first so it
            // gets incorporated into the next packet we send.

            debug!(
                "setting local connect status for local queue {} to {}",
                queue, frame
            );
            session_ref.local_connect_status[queue].write().last_frame = frame;

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
    pub fn advance_frame(&self) -> Commands<T> {
        let mut session_ref = self.0.write();
        let mut commands = Commands::<T>::default();
        info!("End of frame ({})...", session_ref.sync.frame_count());
        if !session_ref.synchronizing {
            session_ref.sync.increment_frame(&mut commands);
        }
        session_ref.do_poll(&mut commands);
        commands
    }

    /// Polls the network events. This should always be called before every frame of the game
    /// regardless of if the game is advancing it's state or not.
    pub fn poll(&self) -> Commands<T> {
        let mut session_ref = self.0.write();
        let mut commands = Commands::default();
        session_ref.do_poll(&mut commands);
        commands
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
    pub fn disconnect_player(&self, player: PlayerHandle) -> BackrollResult<Commands<T>> {
        let mut session_ref = self.0.write();
        let queue = session_ref.player_handle_to_queue(player)?;
        if session_ref.local_connect_status[queue].read().disconnected {
            return Err(BackrollError::PlayerDisconnected(player));
        }

        let mut commands = Commands::<T>::default();
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
                    session_ref.disconnect_player_queue(&mut commands, i, current_frame);
                }
            }
        } else {
            info!(
                "Disconnecting queue {} at frame {} by user request.",
                queue, last_frame
            );
            session_ref.disconnect_player_queue(&mut commands, queue, last_frame);
        }
        Ok(commands)
    }

    /// Gets network statistics with a remote player.
    ///
    /// # Errors
    /// Returns [BackrollError::InvalidPlayer] if the provided player handle does not point a vali
    /// player.
    pub fn get_network_stats(&self, player: PlayerHandle) -> BackrollResult<NetworkStats> {
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
    pub fn set_frame_delay(&self, player: PlayerHandle, delay: Frame) -> BackrollResult<()> {
        let mut session_ref = self.0.write();
        let queue = session_ref.player_handle_to_queue(player)?;
        session_ref.sync.set_frame_delay(queue, delay);
        Ok(())
    }
}
