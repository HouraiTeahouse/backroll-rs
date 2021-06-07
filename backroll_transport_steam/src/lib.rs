use async_channel::TrySendError;
use backroll_transport::{Peer, Peers};
use bevy_tasks::TaskPool;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use steamworks::{Client, ClientManager, SendType, SteamId};
use tracing::{debug, error};

/// The maximum size of unreliable packet that can be sent or recieved,
/// in bytes.
pub const UNRELIABLE_MTU: usize = 1200;

// High cleanup interval since the P2P socket layer may take a while to
// initialize and connect.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(20);

pub struct SteamConnectionConfig {
    pub remote: SteamId,
    pub max_queue_size: Option<usize>,
}

impl SteamConnectionConfig {
    /// Shorthand for creating unbounded connections. Unbounded connections
    /// will never drop a recieved packet. However, because it will not drop
    /// packets, malicious actors can flood the connection.
    pub fn unbounded(remote: SteamId) -> Self {
        Self {
            remote,
            max_queue_size: None,
        }
    }

    /// Shorthand for creating bounded connections. Bounded connections
    /// will drop a recieved packet if the recieve queue is full.
    pub fn bounded(remote: SteamId, limit: usize) -> Self {
        Self {
            remote,
            max_queue_size: Some(limit),
        }
    }
}

pub struct SteamP2PManager {
    peers: Arc<Peers<SteamId>>,
    client: Client<ClientManager>,
    task_pool: TaskPool,
}

impl SteamP2PManager {
    /// Starts a new thread to listen for P2P messages from Steam.
    pub fn bind(pool: TaskPool, client: Client<ClientManager>) -> Self {
        let peers = Arc::new(Peers::default());
        let manager = Self {
            peers: peers.clone(),
            client: client.clone(),
            task_pool: pool.clone(),
        };

        let peers = Arc::downgrade(&peers);
        std::thread::spawn(move || Self::recv(peers, client));

        manager
    }

    /// Creates a [Peer] bound to a specific target [SteamId].
    ///
    /// Note this does not block or send any I/O. It simply creates the
    /// tasks for reading and sending.
    ///
    /// [Peer]: backroll_transport::Peer
    /// [SteamId]: steamworks::SteamId
    pub fn connect(&self, config: SteamConnectionConfig) -> Peer {
        let peer = if let Some(limit) = config.max_queue_size {
            self.peers.create_bounded(config.remote, limit)
        } else {
            self.peers.create_unbounded(config.remote)
        };
        let other = self.peers.get(&config.remote).unwrap().clone();
        let client = self.client.clone();
        let task = Self::send(other, config.remote, client);
        self.task_pool.spawn(task).detach();
        peer
    }

    /// Disconnects the connection to a given [SteamId] if available.
    ///
    /// [SteamId]: steamworks::SteamId
    pub fn disconnect(&self, remote: SteamId) {
        self.peers.disconnect(&remote);
    }

    async fn send(peer: Peer, remote: SteamId, client: Client<ClientManager>) {
        while let Ok(message) = peer.recv().await {
            if message.len() > UNRELIABLE_MTU {
                error!(
                    "Failed to send unreliable message to {:?}: Too big, size ({}) exceeds MTU of {}",
                    remote, message.len(), UNRELIABLE_MTU,
                );
                continue;
            }
            if !client
                .networking()
                .send_p2p_packet(remote, SendType::Unreliable, message.as_ref())
            {
                error!("Error while sending message to {:?}", remote);
            }
        }
    }

    fn recv(peers: Weak<Peers<SteamId>>, client: Client<ClientManager>) {
        let mut read_buf = vec![0u8; UNRELIABLE_MTU];
        let last_flush = Instant::now();
        while let Some(peers) = peers.upgrade() {
            if let Some(size) = client.networking().is_p2p_packet_available() {
                if size >= read_buf.len() {
                    read_buf.resize(size, 0u8);
                }

                let (remote, len) = client
                    .networking()
                    .read_p2p_packet(read_buf.as_mut())
                    .unwrap();
                if let Some(peer) = peers.get(&remote) {
                    Self::forward_packet(remote, peer, &read_buf[0..len]);
                }
            }

            // Periodically cleanup the peers.
            if Instant::now() - last_flush > CLEANUP_INTERVAL {
                peers.flush_disconnected();
            }
        }
    }

    fn forward_packet(remote: SteamId, peer: Peer, data: &[u8]) {
        match peer.try_send(data.into()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                debug!(
                    "Dropped packet due to the packet queue for {:?} being full",
                    remote
                );
            }
            Err(TrySendError::Closed(_)) => {
                debug!("Dropped packet for disconnected packet queue: {:?}", remote);
            }
        }
    }
}
