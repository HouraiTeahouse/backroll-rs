use async_channel::TrySendError;
use async_net::{SocketAddr, UdpSocket};
use backroll_transport::{Peer, Peers};
use bevy_tasks::IoTaskPool;
use std::convert::TryFrom;
use std::net::{ToSocketAddrs, UdpSocket as BlockingUdpSocket};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tracing::{debug, error};

const CLEANUP_INTERVAL: Duration = Duration::from_millis(1000);

/// The maximum size of packet that can be sent or recieved, in bytes.
///
/// This is based on the ethernet standard MTU (1500 bytes), the size of the
/// IPv4/6 header (20/40 bytes), and the UDP header (8 bytes).
pub const MAX_TRANSMISSION_UNIT: usize = 1452;

pub struct UdpConnectionConfig {
    pub addr: SocketAddr,
    pub max_queue_size: Option<usize>,
}

impl UdpConnectionConfig {
    /// Shorthand for creating unbounded connections. Unbounded connections
    /// will never drop a recieved packet. However, because it will not drop
    /// packets, malicious actors can flood the connection.
    pub fn unbounded(addr: SocketAddr) -> UdpConnectionConfig {
        Self {
            addr,
            max_queue_size: None,
        }
    }

    /// Shorthand for creating bounded connections. Bounded connections
    /// will drop a recieved packet if the recieve queue is full.
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
}

impl UdpManager {
    /// Binds a [UdpSocket] and starts listening on it.
    ///
    /// # Errors
    /// Returns a [std::io::Error] if it fails to bind to the provided socket addresses
    /// or start an async poll on the socket.
    ///
    /// [UdpSocket]: async_net::UdpSocket
    pub fn bind(addrs: impl ToSocketAddrs) -> std::io::Result<Self> {
        let blocking = BlockingUdpSocket::bind(addrs)?;
        let socket = UdpSocket::try_from(blocking)?;
        let peers = Arc::new(Peers::default());
        let manager = Self {
            peers: peers.clone(),
            socket: socket.clone(),
        };

        IoTaskPool::get()
            .spawn(Self::recv(Arc::downgrade(&peers), socket))
            .detach();

        Ok(manager)
    }

    /// Creates a [Peer] bound to a specific target [SocketAddr].
    ///
    /// Note this does not block or send any I/O. It simply creates the
    /// tasks for reading and sending.
    ///
    /// [Peer]: backroll_transport::Peer
    /// [SocketAddr]: std::net::SocketAddr
    pub fn connect(&self, config: UdpConnectionConfig) -> Peer {
        let peer = if let Some(limit) = config.max_queue_size {
            self.peers.create_bounded(config.addr, limit)
        } else {
            self.peers.create_unbounded(config.addr)
        };
        let other = self.peers.get(&config.addr).unwrap();
        let socket = self.socket.clone();
        let task = Self::send(other, config.addr, socket);

        IoTaskPool::get().spawn(task).detach();
        peer
    }

    /// Disconnects the connection to a given [SocketAddr] if available.
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    #[serial_test::serial]
    pub fn test_basic_connect() {
        const ADDR_A: &str = "127.0.0.1:10000";
        const ADDR_B: &str = "127.0.0.1:10001";

        let socket_a = UdpManager::bind(ADDR_A).unwrap();
        let socket_b = UdpManager::bind(ADDR_B).unwrap();

        let peer_a = socket_b.connect(UdpConnectionConfig::unbounded(ADDR_A.parse().unwrap()));
        let peer_b = socket_a.connect(UdpConnectionConfig::unbounded(ADDR_B.parse().unwrap()));

        let msg_a: Box<[u8]> = b"Hello A!"[0..].into();
        let msg_b: Box<[u8]> = b"Hello B!"[0..].into();

        peer_a.try_send(msg_b.clone()).unwrap();
        peer_b.try_send(msg_a.clone()).unwrap();

        let recv_msg_a = futures::executor::block_on(peer_a.recv()).unwrap();
        let recv_msg_b = futures::executor::block_on(peer_b.recv()).unwrap();

        assert_eq!(msg_a, recv_msg_a);
        assert_eq!(msg_b, recv_msg_b);
    }

    #[test]
    #[serial_test::serial]
    pub fn test_multiple_send() {
        const ADDR_A: &str = "127.0.0.1:10000";
        const ADDR_B: &str = "127.0.0.1:10001";

        let socket_a = UdpManager::bind(ADDR_A).unwrap();
        let socket_b = UdpManager::bind(ADDR_B).unwrap();

        let peer_a = socket_b.connect(UdpConnectionConfig::unbounded(ADDR_A.parse().unwrap()));
        let peer_b = socket_a.connect(UdpConnectionConfig::unbounded(ADDR_B.parse().unwrap()));

        peer_a.try_send(b"100"[0..].into()).unwrap();
        peer_a.try_send(b"101"[0..].into()).unwrap();
        peer_a.try_send(b"102"[0..].into()).unwrap();
        peer_a.try_send(b"103"[0..].into()).unwrap();
        peer_a.try_send(b"104"[0..].into()).unwrap();
        peer_a.try_send(b"105"[0..].into()).unwrap();

        assert_eq!(
            futures::executor::block_on(peer_b.recv()),
            Ok(b"100"[0..].into())
        );
        assert_eq!(
            futures::executor::block_on(peer_b.recv()),
            Ok(b"101"[0..].into())
        );
        assert_eq!(
            futures::executor::block_on(peer_b.recv()),
            Ok(b"102"[0..].into())
        );
        assert_eq!(
            futures::executor::block_on(peer_b.recv()),
            Ok(b"103"[0..].into())
        );
        assert_eq!(
            futures::executor::block_on(peer_b.recv()),
            Ok(b"104"[0..].into())
        );
        assert_eq!(
            futures::executor::block_on(peer_b.recv()),
            Ok(b"105"[0..].into())
        );
    }
}
