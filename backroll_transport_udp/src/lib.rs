use async_channel::TrySendError;
use async_net::{SocketAddr, UdpSocket};
use backroll_transport::{Peer, Peers};
use bevy_tasks::TaskPool;
use std::convert::TryFrom;
use std::net::{ToSocketAddrs, UdpSocket as BlockingUdpSocket};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tracing::{debug, error};

const CLEANUP_INTERVAL: Duration = Duration::from_millis(1000);
pub const MAX_TRANSMISSION_UNIT: usize = 1450;

pub struct UdpConnectionConfig {
    pub addr: SocketAddr,
    pub max_queue_size: Option<usize>,
}

impl UdpConnectionConfig {
    pub fn unbounded(addr: SocketAddr) -> UdpConnectionConfig {
        Self {
            addr,
            max_queue_size: None,
        }
    }

    pub fn bounded(addr: SocketAddr, limit: usize) -> UdpConnectionConfig {
        Self {
            addr,
            max_queue_size: Some(limit),
        }
    }
}

pub struct UdpManager {
    peers: Arc<Peers<SocketAddr>>,
    socket: UdpSocket,
    task_pool: TaskPool,
}

impl UdpManager {
    /// Binds a `[UdpSocket]` and starts listening on it.
    pub fn bind(pool: TaskPool, addrs: impl ToSocketAddrs) -> std::io::Result<Self> {
        let blocking = BlockingUdpSocket::bind(addrs)?;
        let socket = UdpSocket::try_from(blocking)?;
        let peers = Arc::new(Peers::default());
        let manager = Self {
            peers: peers.clone(),
            socket: socket.clone(),
            task_pool: pool.clone(),
        };

        pool.spawn(Self::recv(Arc::downgrade(&peers), socket))
            .detach();

        Ok(manager)
    }

    /// Creates a `[Peer]` bound to a specific target `[SocketAddr]`.
    ///
    /// Note this does not block or send any I/O. It simply creates the
    /// tasks for reading and sending.
    pub fn connect(&self, config: UdpConnectionConfig) -> Peer {
        let peer = if let Some(limit) = config.max_queue_size {
            self.peers.create_bounded(config.addr, limit)
        } else {
            self.peers.create_unbounded(config.addr)
        };
        let other = self.peers.get(&config.addr).unwrap().clone();
        let socket = self.socket.clone();
        let task = Self::send(other, config.addr, socket);
        self.task_pool.spawn(task).detach();
        peer
    }

    /// Disconnects the connection to a given `[SocketAddr]` if available.
    ///
    /// [SocketAddr]: std::net::SocketAddr
    pub fn disconnect(&self, addr: SocketAddr) {
        self.peers.disconnect(&addr);
    }

    async fn send(peer: Peer, target_addr: SocketAddr, socket: UdpSocket) {
        while let Ok(message) = peer.recv().await {
            if let Err(err) = socket.send_to(message.as_ref(), target_addr).await {
                error!(
                    "Error while sending message to {:?}: {:?}",
                    target_addr, err
                );
            }
        }
    }

    async fn recv(peers: Weak<Peers<SocketAddr>>, socket: UdpSocket) {
        let mut read_buf = [0u8; MAX_TRANSMISSION_UNIT];
        let last_flush = Instant::now();
        while let Some(peers) = peers.upgrade() {
            match socket.recv_from(&mut read_buf).await {
                Ok((len, addr)) => {
                    debug_assert!(len < MAX_TRANSMISSION_UNIT);
                    if let Some(peer) = peers.get(&addr) {
                        Self::forward_packet(addr, peer, &read_buf[0..len]);
                    }
                }
                Err(err) => {
                    error!("Error while receiving UDP packets: {:?}", err);
                }
            }

            // Periodically cleanup the peers.
            if Instant::now() - last_flush > CLEANUP_INTERVAL {
                peers.flush_disconnected();
            }
        }
    }

    fn forward_packet(addr: SocketAddr, peer: Peer, data: &[u8]) {
        match peer.try_send(data.into()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                debug!(
                    "Dropped packet due to the packet queue for {} being full",
                    addr
                );
            }
            Err(TrySendError::Closed(_)) => {
                debug!("Dropped packet for disconnected packet queue: {} ", addr);
            }
        }
    }
}
