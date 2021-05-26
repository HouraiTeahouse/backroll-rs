use self::input_buffer::*;
use self::message::*;
use super::{
    input::FrameInput,
    time_sync::{TimeSync, UnixMillis},
    BackrollConfig, Frame, NetworkStats,
};
use async_channel::TrySendError;
use backroll_transport::connection::Peer;
use parking_lot::RwLock;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::num::Wrapping;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

mod compression;
mod input_buffer;
mod message;

const MSG_MAX_PLAYERS: usize = 8;
const UDP_HEADER_SIZE: usize = 28; /* Size of IP + UDP headers */
const NUM_SYNC_PACKETS: u8 = 5;
const TARGET_TPS: u64 = 60;
const SYNC_RETRY_INTERVAL: Duration = Duration::from_millis(2000);
const SYNC_FIRST_RETRY_INTERVAL: Duration = Duration::from_millis(500);
const RUNNING_RETRY_INTERVAL: Duration = Duration::from_millis(200);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(200);
const QUALITY_REPORT_INTERVAL: Duration = Duration::from_millis(1000);
const NETWORK_STATS_INTERVAL: Duration = Duration::from_millis(1000);
const UDP_SHUTDOWN_TIMER: Duration = Duration::from_millis(5000);
const MAX_SEQ_DISTANCE: Wrapping<u16> = Wrapping(1 << 15);

#[derive(Clone, Copy, Debug)]
pub enum PeerState {
    Syncing {
        roundtrips_remaining: u8,
        random: u32,
    },
    Running {
        last_quality_report_time: UnixMillis,
        last_network_stats_interval: UnixMillis,
        last_input_packet_recv_time: UnixMillis,
    },
    Disconnected,
}

impl PeerState {
    pub fn is_running(&self) -> bool {
        if let Self::Running { .. } = self {
            true
        } else {
            false
        }
    }

    pub fn is_disconnected(&self) -> bool {
        if let Self::Disconnected = self {
            true
        } else {
            false
        }
    }
}

impl Default for PeerState {
    fn default() -> Self {
        Self::Syncing {
            roundtrips_remaining: 0,
            random: 0,
        }
    }
}

pub struct BackrollPeer<T>
where
    T: BackrollConfig,
{
    queue: usize,

    magic_number: u16,
    remote_magic_number: Option<u16>,

    peer: Peer,
    timesync: TimeSync<T::Input>,
    state: PeerState,

    shutdown_timeout: UnixMillis,
    disconnect_timeout: Option<Duration>,
    disconnect_notify_start: Option<Duration>,
    disconnect_notify_sent: bool,
    disconnect_event_sent: bool,

    connected: bool,

    packets_sent: usize,
    bytes_sent: usize,
    stats_start_time: Option<UnixMillis>,
    last_send_time: Option<UnixMillis>,
    last_recv_time: Option<UnixMillis>,
    round_trip_time: Duration,
    kbps_sent: u32,
    peer_connect_status: Vec<ConnectionStatus>,
    local_connect_status: Arc<[RwLock<ConnectionStatus>]>,

    local_frame_advantage: Frame,
    remote_frame_advantage: Frame,

    next_send_seq: Wrapping<u16>,
    next_recv_seq: Wrapping<u16>,

    input_encoder: InputEncoder<T::Input>,
    input_decoder: InputDecoder<T::Input>,
}

impl<T: BackrollConfig> BackrollPeer<T> {
    pub fn new(
        queue: usize,
        peer: Peer,
        local_connect_status: Arc<[RwLock<ConnectionStatus>]>,
    ) -> Self {
        let mut rng = rand::thread_rng();
        let mut magic = rng.next_u32() as u16;
        while magic == 0 {
            magic = rng.next_u32() as u16;
        }
        Self {
            queue,
            timesync: TimeSync::default(),

            magic_number: magic,
            remote_magic_number: None,

            peer,
            state: PeerState::default(),

            shutdown_timeout: UnixMillis::from_millis(0),
            disconnect_timeout: None,
            disconnect_notify_start: None,
            disconnect_notify_sent: false,
            disconnect_event_sent: false,

            connected: false,

            packets_sent: 0,
            bytes_sent: 0,
            stats_start_time: None,
            last_send_time: None,
            last_recv_time: None,
            round_trip_time: Duration::from_millis(0),
            kbps_sent: 0,

            peer_connect_status: Vec::new(),
            local_connect_status,

            local_frame_advantage: 0,
            remote_frame_advantage: 0,

            input_encoder: Default::default(),
            input_decoder: Default::default(),
            next_send_seq: Wrapping(0),
            next_recv_seq: Wrapping(0),
        }
    }

    pub fn state(&self) -> &PeerState {
        &self.state
    }

    pub fn disconnect(&mut self) {
        self.state = PeerState::Disconnected;
        self.shutdown_timeout = UnixMillis::now() + UDP_SHUTDOWN_TIMER;
    }

    pub fn set_disconnect_timeout(&mut self, timeout: Option<Duration>) {
        self.disconnect_timeout = timeout;
    }

    pub fn set_disconnect_notify_start(&mut self, timeout: Option<Duration>) {
        self.disconnect_notify_start = timeout;
    }

    pub fn get_network_stats(&self) -> NetworkStats {
        NetworkStats {
            ping: self.round_trip_time,
            send_queue_len: self.peer.pending_send_count(),
            recv_queue_len: self.peer.pending_recv_count(),
            kbps_sent: self.kbps_sent,

            local_frames_behind: self.local_frame_advantage,
            remote_frames_behind: self.remote_frame_advantage,
        }
    }

    // void
    // UdpProtocol::Init(Udp *udp,
    //                   Poll &poll,
    //                   int queue,
    //                   char *ip,
    //                   u_short port,
    //                   UdpMsg::connect_status *status)
    // {
    //    _udp = udp;
    //    _queue = queue;
    //    _local_connect_status = status;

    //    do {
    //       _magic_number = (uint16)rand();
    //    } while (_magic_number == 0);
    //    poll.RegisterLoop(this);
    // }

    pub fn send_input(&mut self, input: FrameInput<T::Input>) {
        if self.state.is_running() {
            // Check to see if this is a good time to adjust for the rift...
            self.timesync.advance_frame(
                input.clone(),
                self.local_frame_advantage,
                self.remote_frame_advantage,
            );

            // Save this input packet
            //
            // XXX: This queue may fill up for spectators who do not ack input packets in a timely
            // manner.  When this happens, we can either resize the queue (ug) or disconnect them
            // (better, but still ug).  For the meantime, make this queue really big to decrease
            // the odds of this happening...
            self.input_encoder.push(input);
        }
        self.send_pending_output();
    }

    fn send_pending_output(&mut self) {
        let (start_frame, bits) = self.input_encoder.encode();
        self.send(Input {
            peer_connect_status: self
                .local_connect_status
                .iter()
                .map(|status| status.read().clone())
                .collect(),
            start_frame,
            ack_frame: self.input_decoder.last_decoded_frame(),
            disconnect_requested: self.state.is_disconnected(),
            bits,
        });
    }

    pub fn send_input_ack(&mut self) {
        self.send(InputAck {
            ack_frame: self.input_decoder.last_decoded_frame(),
        });
    }

    // bool
    // UdpProtocol::GetEvent(UdpProtocol::Event &e)
    // {
    //    if (_event_queue.size() == 0) {
    //       return false;
    //    }
    //    e = _event_queue.front();
    //    _event_queue.pop();
    //    return true;
    // }

    pub fn manual_poll(&mut self) {
        let now = UnixMillis::now();
        let mut next_interval = Duration::from_millis(0);

        match self.state {
            PeerState::Syncing {
                roundtrips_remaining,
                random,
            } => {
                next_interval = if roundtrips_remaining == NUM_SYNC_PACKETS {
                    SYNC_FIRST_RETRY_INTERVAL
                } else {
                    SYNC_RETRY_INTERVAL
                };
                if let Some(last_send_time) = self.last_send_time {
                    if last_send_time + next_interval < now {
                        info!(
                            "No luck syncing after {:?} ms... Re-queueing sync packet.",
                            next_interval
                        );
                        self.send_sync_request();
                    }
                }
                self.state = PeerState::Syncing {
                    roundtrips_remaining,
                    random,
                };
            }
            PeerState::Running {
                mut last_input_packet_recv_time,
                mut last_quality_report_time,
                mut last_network_stats_interval,
            } => {
                // xxx: rig all this up with a timer wrapper
                if last_input_packet_recv_time + RUNNING_RETRY_INTERVAL < now {
                    info!("Haven't exchanged packets in a while (last received: {}  last sent: {}).  Resending.", 
                          self.input_decoder.last_decoded_frame(),
                          self.input_encoder.last_encoded_frame());
                    last_input_packet_recv_time = now;
                    self.send_pending_output();
                }

                if last_quality_report_time + QUALITY_REPORT_INTERVAL < now {
                    last_quality_report_time = now;
                    self.send(QualityReport {
                        ping: now,
                        frame_advantage: self.local_frame_advantage,
                    });
                }

                if last_network_stats_interval + NETWORK_STATS_INTERVAL < now {
                    last_network_stats_interval = now;
                    self.update_network_stats();
                }

                if let Some(last_send_time) = self.last_send_time {
                    if last_send_time + KEEP_ALIVE_INTERVAL < now {
                        info!("Sending keep alive packet");
                        self.send(MessageData::KeepAlive);
                    }
                }

                let last_recv_time = self.last_recv_time.unwrap_or(now);

                // FIXME(james7132): Properly fire this event
                if let Some(timeout) = self.disconnect_timeout {
                    if let Some(notify_start) = self.disconnect_notify_start {
                        if !self.disconnect_notify_sent && (last_recv_time + notify_start < now) {
                            info!("Endpoint has stopped receiving packets for {} ms.  Sending notification.", 
                                notify_start.as_millis());
                            // Event e(Event::NetworkInterrupted);
                            // e.u.network_interrupted.disconnect_timeout = _disconnect_timeout - _disconnect_notify_start;
                            // QueueEvent(e);
                            self.disconnect_notify_sent = true;
                        }
                    }

                    if last_recv_time + timeout < now {
                        if !self.disconnect_event_sent {
                            info!(
                                "Endpoint has stopped receiving packets for {} ms.  Disconnecting.",
                                timeout.as_millis()
                            );
                            // QueueEvent(Event(Event::Disconnected));
                            self.disconnect_event_sent = true;
                        }
                    }
                }
                self.state = PeerState::Running {
                    last_input_packet_recv_time,
                    last_quality_report_time,
                    last_network_stats_interval,
                };
            }
            PeerState::Disconnected => {
                if self.shutdown_timeout < now {
                    info!("Shutting down udp connection.");
                    self.shutdown_timeout = UnixMillis::from_millis(0);
                }
            }
        }
    }

    fn send(&mut self, message: impl Into<MessageData>) {
        let message = Message {
            magic: self.magic_number,
            sequence_number: self.next_send_seq,
            data: message.into(),
        };
        self.next_send_seq += Wrapping(1);
        let mut bytes = Vec::new();
        {
            let compressor = lz4_flex::frame::FrameEncoder::new(&mut bytes);
            let mut bincode =
                bincode::Serializer::new(compressor, bincode::config::DefaultOptions::new());
            message
                .serialize(&mut bincode)
                .expect("Should not be producing invalid inputs.");
        }
        self.send_data(bytes.into());
    }

    fn send_data(&mut self, mut message: Box<[u8]>) {
        let msg_size = message.len();
        loop {
            // Block until there is capacity to send.
            message = match self.peer.try_send(message) {
                Ok(_) => {
                    self.packets_sent += 1;
                    self.last_send_time = Some(UnixMillis::now());
                    self.bytes_sent += msg_size;
                    return;
                }
                Err(TrySendError::Full(msg)) => msg,
                Err(TrySendError::Closed(_)) => {
                    self.disconnect();
                    error!("Failed to send message due to disconnection.");
                    return;
                }
            }
        }
    }

    fn handle_message(&mut self, message: Message) {
        let seq = message.sequence_number;
        match &message.data {
            MessageData::SyncRequest { .. } => {}
            MessageData::SyncReply { .. } => {}
            _ => {
                if Some(message.magic) != self.remote_magic_number {
                    info!("recv rejecting invalid magic number");
                    return;
                }
                // filter out out-of-order packets
                let skipped = seq - self.next_recv_seq;
                if skipped > MAX_SEQ_DISTANCE {
                    info!(
                        "dropping out of order packet (seq: {}, last seq: {})",
                        seq, self.next_recv_seq
                    );
                    return;
                }
            }
        }

        self.next_recv_seq = seq;

        let mut handled = true;
        match message.data {
            MessageData::KeepAlive => {}
            MessageData::SyncRequest(data) => {
                handled = self.on_sync_request(message.magic, data);
            }
            MessageData::SyncReply(data) => {
                handled = self.on_sync_reply(message.magic, data);
            }
            MessageData::Input(input) => self.on_input(input),
            MessageData::InputAck(data) => self.input_encoder.acknowledge_frame(data.ack_frame),
            MessageData::QualityReport(data) => self.on_quality_report(data),
            MessageData::QualityReply(data) => self.on_quality_reply(data),
        };

        if handled {
            self.last_recv_time = Some(UnixMillis::now());
            if self.disconnect_notify_sent && self.state.is_running() {
                // QueueEvent(Event(Event::NetworkResumed));
                self.disconnect_notify_sent = false;
            }
        }
    }

    fn on_sync_request(&mut self, magic: u16, data: SyncRequest) -> bool {
        let SyncRequest { random } = data;
        if let Some(remote_magic) = self.remote_magic_number {
            if magic != remote_magic {
                info!(
                    "Ignoring sync request from unknown endpoint ({} != {:?}).",
                    magic, remote_magic
                );
                return false;
            }
        }
        self.send(SyncReply { random });
        true
    }

    pub fn synchronize(&mut self) {
        let random = rand::thread_rng().next_u32();
        self.state = PeerState::Syncing {
            roundtrips_remaining: NUM_SYNC_PACKETS,
            random,
        };
        self.send_sync_request();
    }

    pub fn send_sync_request(&mut self) {
        if let PeerState::Syncing { random, .. } = self.state {
            self.send(SyncRequest { random });
        } else {
            panic!("Sending sync request while not syncing.")
        }
    }

    fn update_network_stats(&mut self) {
        let now = UnixMillis::now();

        if self.stats_start_time.is_none() {
            self.stats_start_time = Some(now);
        }

        let total_bytes_sent = (self.bytes_sent + (UDP_HEADER_SIZE * self.packets_sent)) as f32;
        let seconds = (now - self.stats_start_time.unwrap()).as_millis() as f32 / 1000.0;
        let bps = total_bytes_sent / seconds;
        let udp_overhead =
            100.0 * (UDP_HEADER_SIZE * self.packets_sent) as f32 / self.bytes_sent as f32;

        self.kbps_sent = (bps / 1024.0) as u32;

        info!(
            "Network Stats -- Bandwidth: {} KBps   Packets Sent: {} ({} pps) \
               KB Sent: {} UDP Overhead: {:.2}.",
            self.kbps_sent,
            self.packets_sent,
            self.packets_sent as f32 * 1000.0
                / (now - self.stats_start_time.unwrap()).as_millis() as f32,
            total_bytes_sent / 1024.0,
            udp_overhead
        );
    }

    pub fn get_peer_connect_status(&self, id: usize) -> &ConnectionStatus {
        &self.peer_connect_status[id]
    }

    // void
    // UdpProtocol::QueueEvent(const UdpProtocol::Event &evt)
    // {
    //    _event_queue.push(evt);
    // }

    fn on_sync_reply(&mut self, magic: u16, data: SyncReply) -> bool {
        if let PeerState::Syncing {
            random,
            ref mut roundtrips_remaining,
            ..
        } = self.state
        {
            if data.random != random {
                info!("sync reply {} != {}.  Keep looking...", data.random, random);
                return false;
            }

            if !self.connected {
                // QueueEvent(Event(Event::Connected));
                self.connected = true;
            }

            info!(
                "Checking sync state ({} round trips remaining).",
                *roundtrips_remaining
            );
            debug_assert!(*roundtrips_remaining > 0);
            *roundtrips_remaining -= 1;
            if *roundtrips_remaining == 0 {
                info!("Synchronized queue {}!", self.queue);
                // QueueEvent(UdpProtocol::Event(UdpProtocol::Event::Synchronzied));
                let now = UnixMillis::now();
                self.state = PeerState::Running {
                    last_quality_report_time: now,
                    last_network_stats_interval: now,
                    last_input_packet_recv_time: now,
                };
                self.input_decoder.reset();
                self.remote_magic_number = Some(magic);
            } else {
                // UdpProtocol::Event evt(UdpProtocol::Event::Synchronizing);
                // evt.u.synchronizing.total = NUM_SYNC_PACKETS;
                // evt.u.synchronizing.count = NUM_SYNC_PACKETS - _state.sync.roundtrips_remaining;
                // QueueEvent(evt);
                self.send_sync_request();
            }
            true
        } else {
            info!("Ignoring SyncReply while not syncing.");
            Some(magic) == self.remote_magic_number
        }
    }

    fn on_input(&mut self, msg: Input) {
        let Input {
            peer_connect_status,
            start_frame,
            ack_frame,
            disconnect_requested,
            bits,
        } = msg;

        // If a disconnect is requested, go ahead and disconnect now.
        if disconnect_requested {
            if !self.state.is_disconnected() && !self.disconnect_event_sent {
                info!("Disconnecting endpoint on remote request.");
                //  QueueEvent(Event(Event::Disconnected));
                self.disconnect_event_sent = true;
            }
        } else {
            // Update the peer connection status if this peer is still considered to be part
            // of the network.
            for (i, remote_status) in peer_connect_status.iter().enumerate() {
                if i < self.peer_connect_status.len() {
                    debug_assert!(
                        remote_status.last_frame >= self.peer_connect_status[i].last_frame
                    );
                    self.peer_connect_status[i].disconnected |= remote_status.disconnected;
                    self.peer_connect_status[i].last_frame = std::cmp::max(
                        self.peer_connect_status[i].last_frame,
                        remote_status.last_frame,
                    );
                } else {
                    self.peer_connect_status.push(remote_status.clone());
                }
            }
        }

        // Decompress the input.
        match self.input_decoder.decode(start_frame, bits) {
            Ok(inputs) => {
                // TODO(james7132): Push event upwards
                if let PeerState::Running {
                    mut last_input_packet_recv_time,
                    ..
                } = &mut self.state
                {
                    last_input_packet_recv_time = UnixMillis::now();
                }
            }
            Err(err) => {
                error!(
                    "Error while decoding recieved inputs. discarding: {:?}",
                    err
                );
                return;
            }
        }

        // Get rid of our buffered input
        self.input_encoder.acknowledge_frame(ack_frame);
    }

    fn on_quality_report(&mut self, data: QualityReport) {
        self.remote_frame_advantage = data.frame_advantage;
        self.send(QualityReply { pong: data.ping });
    }

    fn on_quality_reply(&mut self, data: QualityReply) {
        self.round_trip_time = UnixMillis::now() - data.pong;
    }

    pub fn set_local_frame_number(&mut self, local_frame: Frame) {
        // Estimate which frame the other guy is one by looking at the
        // last frame they gave us plus some delta for the one-way packet
        // trip time.
        let remote_frame = self.input_decoder.last_decoded_frame()
            + (self.round_trip_time.as_secs() * TARGET_TPS) as i32;

        // Our frame advantage is how many frames *behind* the other guy
        // we are.  Counter-intuative, I know.  It's an advantage because
        // it means they'll have to predict more often and our moves will
        // pop more frequently.
        self.local_frame_advantage = remote_frame - local_frame;
    }

    pub fn recommend_frame_delay(&mut self) -> Frame {
        // XXX: require idle input should be a configuration parameter
        self.timesync.recommend_frame_wait_duration(false)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConnectionStatus {
    pub disconnected: bool,
    pub last_frame: Frame,
}

impl Default for ConnectionStatus {
    fn default() -> Self {
        Self {
            disconnected: false,
            last_frame: super::NULL_FRAME,
        }
    }
}

struct Stats {
    ping: i32,
    remote_frame_advantage: i32,
    local_frame_advantage: i32,
    send_queue_len: usize,
    // Udp::Stats          udp;
}

pub enum Event<T> {
    Connected,
    Synchronizing { total: i32, count: i32 },
    Synchronized,
    Input { input: FrameInput<T> },
    Disconnected,
    NetworkInterrupted { disconnect_timeout: i32 },
    NetworkResumed,
}
