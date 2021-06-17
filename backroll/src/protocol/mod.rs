use self::input_buffer::*;
use self::message::*;
use crate::{
    input::FrameInput,
    time_sync::{TimeSync, UnixMillis},
    Config, Frame, NetworkStats, TaskPool,
};
use async_channel::TrySendError;
use backroll_transport::Peer as TransportPeer;
use bincode::config::Options;
use futures::FutureExt;
use futures_timer::Delay;
use parking_lot::RwLock;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::num::Wrapping;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error};

pub(crate) use event::Event;

mod bitfield;
mod compression;
mod event;
mod input_buffer;
mod message;

pub enum PeerError {
    LocalDisconnected,
    RemoteDisconnected,
    InvalidMessage,
}

const UDP_HEADER_SIZE: usize = 28; // Size of IP + UDP headers
const MAX_TRANSMISSION_UNIT: u64 = 1450; // A sane common packet size.
const NUM_SYNC_PACKETS: u8 = 5;
const TARGET_TPS: u64 = 60;
const POLL_INTERVAL: Duration = Duration::from_millis(1000 / TARGET_TPS);
const SYNC_RETRY_INTERVAL: Duration = Duration::from_millis(2000);
const SYNC_FIRST_RETRY_INTERVAL: Duration = Duration::from_millis(500);
const RUNNING_RETRY_INTERVAL: Duration = Duration::from_millis(200);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(200);
const QUALITY_REPORT_INTERVAL: Duration = Duration::from_millis(1000);
const NETWORK_STATS_INTERVAL: Duration = Duration::from_millis(1000);
const MAX_SEQ_DISTANCE: Wrapping<u16> = Wrapping(1 << 15);

fn random() -> u32 {
    let mut rng = rand::thread_rng();
    loop {
        let random = rng.next_u32();
        if random != 0 {
            return random;
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PeerState {
    Connecting {
        random: u32,
    },
    Syncing {
        random: u32,
        roundtrips_remaining: u8,
    },
    Running {
        remote_magic: u16,
    },
    Interrupted {
        remote_magic: u16,
    },
    Disconnected,
}

impl PeerState {
    pub fn random(&self) -> Option<u32> {
        match *self {
            Self::Connecting { random } => Some(random),
            Self::Syncing { random, .. } => Some(random),
            _ => None,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. } | Self::Interrupted { .. })
    }

    fn create_sync_request(&self) -> SyncRequest {
        if let PeerState::Connecting { random, .. } | PeerState::Syncing { random, .. } = self {
            SyncRequest { random: *random }
        } else {
            panic!("Sending sync request while not syncing.")
        }
    }

    pub fn is_disconnected(&self) -> bool {
        matches!(self, Self::Disconnected)
    }

    pub fn is_interrupted(&self) -> bool {
        matches!(self, Self::Interrupted { .. })
    }

    pub fn start_syncing(&mut self, round_trips: u8) {
        if let Self::Connecting { random } = *self {
            *self = Self::Syncing {
                random,
                roundtrips_remaining: round_trips,
            };
        }
    }

    pub fn interrupt(&mut self) -> bool {
        if let Self::Running { remote_magic } = *self {
            *self = Self::Interrupted { remote_magic };
            true
        } else {
            false
        }
    }

    pub fn resume(&mut self) -> bool {
        if let Self::Interrupted { remote_magic } = *self {
            *self = Self::Running { remote_magic };
            true
        } else {
            false
        }
    }
}

impl Default for PeerState {
    fn default() -> Self {
        Self::Connecting { random: random() }
    }
}

#[derive(Default)]
struct PeerStats {
    pub packets_sent: usize,
    pub bytes_sent: usize,
    pub last_send_time: Option<UnixMillis>,
    pub last_input_packet_recv_time: UnixMillis,
    pub round_trip_time: Duration,
    pub kbps_sent: u32,

    pub local_frame_advantage: Frame,
    pub remote_frame_advantage: Frame,
}

#[derive(Clone)]
pub(crate) struct PeerConfig {
    pub peer: TransportPeer,
    pub disconnect_timeout: Duration,
    pub disconnect_notify_start: Duration,
    pub task_pool: TaskPool,
}

pub(crate) struct Peer<T>
where
    T: Config,
{
    queue: usize,
    config: PeerConfig,
    timesync: TimeSync<T::Input>,
    state: Arc<RwLock<PeerState>>,

    stats: Arc<RwLock<PeerStats>>,
    local_connect_status: Arc<[RwLock<ConnectionStatus>]>,
    peer_connect_status: Vec<ConnectionStatus>,

    input_encoder: InputEncoder<T::Input>,
    input_decoder: InputDecoder<T::Input>,

    message_in: async_channel::Receiver<Message>,
    message_out: async_channel::Sender<MessageData>,
    events: async_channel::Sender<Event<T::Input>>,
}

impl<T: Config> Clone for Peer<T> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue,
            config: self.config.clone(),
            timesync: self.timesync.clone(),
            state: self.state.clone(),

            stats: self.stats.clone(),
            local_connect_status: self.local_connect_status.clone(),
            peer_connect_status: self.peer_connect_status.clone(),

            input_encoder: self.input_encoder.clone(),
            input_decoder: self.input_decoder.clone(),

            message_in: self.message_in.clone(),
            message_out: self.message_out.clone(),
            events: self.events.clone(),
        }
    }
}

impl<T: Config> Peer<T> {
    pub fn new(
        queue: usize,
        config: PeerConfig,
        local_connect_status: Arc<[RwLock<ConnectionStatus>]>,
    ) -> (Self, async_channel::Receiver<Event<T::Input>>) {
        let (deserialize_send, message_in) = async_channel::unbounded::<Message>();
        let (message_out, serialize_recv) = async_channel::unbounded::<MessageData>();
        let (events, events_rx) = async_channel::unbounded();
        let peer_connect_status = local_connect_status
            .iter()
            .map(|status| status.read().clone())
            .collect();
        let task_pool = config.task_pool.clone();

        let peer = Self {
            queue,
            config,
            timesync: Default::default(),
            state: Default::default(),

            stats: Default::default(),
            local_connect_status,
            peer_connect_status,

            input_encoder: Default::default(),
            input_decoder: Default::default(),

            message_in,
            message_out,
            events,
        };

        // Start the base subtasks on the provided executor
        task_pool
            .spawn(peer.clone().serialize_outgoing(serialize_recv))
            .detach();
        task_pool
            .spawn(peer.clone().deserialize_incoming(deserialize_send))
            .detach();
        task_pool.spawn(peer.clone().run()).detach();

        (peer, events_rx)
    }

    pub fn is_running(&self) -> bool {
        self.state.read().is_running()
    }

    pub fn disconnect(&self) {
        *self.state.write() = PeerState::Disconnected;
        self.message_in.close();
        self.message_out.close();
        self.events.close();
    }

    fn push_event(&self, evt: Event<T::Input>) -> Result<(), PeerError> {
        // Failure to send just means
        match self.events.try_send(evt) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                panic!("This channel should never be full, it should be unbounded")
            }
            Err(TrySendError::Closed(_)) => Err(PeerError::LocalDisconnected),
        }
    }

    fn send(&self, msg: impl Into<MessageData>) -> Result<(), PeerError> {
        match self.message_out.try_send(msg.into()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                panic!("This channel should never be full, it should be unbounded")
            }
            Err(TrySendError::Closed(_)) => Err(PeerError::RemoteDisconnected),
        }
    }

    pub fn get_network_stats(&self) -> NetworkStats {
        let stats = self.stats.read();
        NetworkStats {
            ping: stats.round_trip_time,
            send_queue_len: self.message_out.len(),
            recv_queue_len: self.message_in.len(),
            kbps_sent: stats.kbps_sent,

            local_frames_behind: stats.local_frame_advantage,
            remote_frames_behind: stats.remote_frame_advantage,
        }
    }

    pub fn send_input(&self, input: FrameInput<T::Input>) -> Result<(), PeerError> {
        if self.state.read().is_running() {
            let stats = self.stats.read();
            // Check to see if this is a good time to adjust for the rift...
            self.timesync.advance_frame(
                input.clone(),
                stats.local_frame_advantage,
                stats.remote_frame_advantage,
            );

            // Save this input packet
            //
            // XXX: This queue may fill up for spectators who do not ack input packets in a timely
            // manner.  When this happens, we can either resize the queue (ug) or disconnect them
            // (better, but still ug).  For the meantime, make this queue really big to decrease
            // the odds of this happening...
            self.input_encoder.push(input);
        }
        self.send_pending_output()
    }

    fn send_pending_output(&self) -> Result<(), PeerError> {
        let (start_frame, bits) = self.input_encoder.encode().expect(
            "The Backroll client has somehow sent created an input \
             queue of 65,535 bytes or more. This is ill advised. \
             Consider further compressing your inputs.",
        );
        self.send(Input {
            peer_connect_status: self
                .local_connect_status
                .iter()
                .map(|status| status.read().clone())
                .collect(),
            start_frame,
            ack_frame: self.input_decoder.last_decoded_frame(),
            bits,
        })
    }

    async fn heartbeat(self, interval: Duration) {
        while let Ok(()) = self.send(MessageData::KeepAlive) {
            debug!("Sent keep alive packet");
            Delay::new(interval).await;
        }
    }

    async fn send_quality_reports(self, interval: Duration) -> Result<(), PeerError> {
        debug!("Starting quality reports to queue: {}", self.queue);
        let mut result = Ok(());
        while self.is_running() {
            let frame_advantage = self.stats.read().local_frame_advantage;
            let msg = QualityReport {
                ping: UnixMillis::now(),
                frame_advantage,
            };
            // Erroring means disconnection.
            if let Err(err) = self.send(msg) {
                result = Err(err);
                break;
            }
            Delay::new(interval).await;
        }
        debug!("Stopped sending quality reports to: {}", self.queue);
        result
    }

    async fn resend_inputs(self, interval: Duration) -> Result<(), PeerError> {
        while self.is_running() {
            {
                let mut stats = self.stats.write();
                let now = UnixMillis::now();
                // xxx: rig all this up with a timer wrapper
                if stats.last_input_packet_recv_time + RUNNING_RETRY_INTERVAL < now {
                    debug!("Haven't exchanged packets in a while (last received: {}  last sent: {}).  Resending.", 
                        self.input_decoder.last_decoded_frame(),
                        self.input_encoder.last_encoded_frame());
                    stats.last_input_packet_recv_time = now;
                    self.send_pending_output()?;
                }
            }
            Delay::new(interval).await;
        }
        Ok(())
    }

    async fn run(mut self) -> Result<(), PeerError> {
        let mut last_recv_time = UnixMillis::now();
        loop {
            futures::select! {
                 message = self.message_in.recv().fuse() => {
                    let message = message.map_err(|_| PeerError::RemoteDisconnected)?;
                    match self.handle_message(message).await {
                        Ok(()) => {
                            last_recv_time = UnixMillis::now();
                            if self.state.write().resume() {
                                self.push_event(Event::<T::Input>::NetworkResumed)?;
                            }
                        },
                        Err(PeerError::InvalidMessage) => {
                            error!("Invalid incoming message");
                        },
                        err => {
                            self.disconnect();
                            return err;
                        }
                    }
                },
                _ = Delay::new(POLL_INTERVAL).fuse() => {
                    let timeout = self.config.disconnect_timeout;
                    let notify_start = self.config.disconnect_notify_start;
                    let now = UnixMillis::now();

                    {
                        let mut state = self.state.write();
                        if !state.is_interrupted() && (last_recv_time + notify_start < now) {
                            state.interrupt();
                            debug!("Endpoint has stopped receiving packets for {} ms.  Sending notification.",
                                  notify_start.as_millis());
                            self.push_event(Event::<T::Input>::NetworkInterrupted {
                                disconnect_timeout: timeout - notify_start
                            })?;
                        }
                    }

                    if last_recv_time + timeout < now {
                        debug!(
                            "Endpoint has stopped receiving packets for {} ms. Disconnecting.",
                            timeout.as_millis()
                        );
                        self.disconnect();
                        return Err(PeerError::RemoteDisconnected);
                    }
                    self.poll()?;
                },
            }
        }
    }

    fn poll(&mut self) -> Result<(), PeerError> {
        let state = self.state.read();
        let next_interval = match *state {
            PeerState::Connecting { .. } => SYNC_FIRST_RETRY_INTERVAL,
            PeerState::Syncing { .. } => SYNC_RETRY_INTERVAL,
            _ => return Ok(()),
        };
        let now = UnixMillis::now();
        if let Some(last_send_time) = self.stats.read().last_send_time {
            if last_send_time + next_interval < now {
                debug!(
                    "No luck syncing after {:?} ms... Re-queueing sync packet.",
                    next_interval
                );
                self.send(state.create_sync_request())?;
            }
        } else {
            // If we have not sent anything yet, kick off the connection with a
            // sync request.
            self.send(state.create_sync_request())?;
        }

        Ok(())
    }

    async fn serialize_outgoing(self, messages: async_channel::Receiver<MessageData>) {
        let magic = random() as u16;
        let mut next_send_seq = Wrapping(0);
        while let Ok(data) = messages.recv().await {
            let message = Message {
                magic,
                sequence_number: next_send_seq,
                data,
            };
            next_send_seq += Wrapping(1);

            let mut bytes = Vec::new();
            {
                let mut bincode = bincode::Serializer::new(
                    &mut bytes,
                    bincode::options().with_limit(MAX_TRANSMISSION_UNIT),
                );
                if let Err(err) = message.serialize(&mut bincode) {
                    error!(
                        "Dropping outgoing packet. Error while serializing outgoing message: {:?}",
                        err
                    );
                    continue;
                }
            }

            let msg_size = bytes.len();
            if let Ok(()) = self.config.peer.send(bytes.into()).await {
                let mut stats = self.stats.write();
                stats.packets_sent += 1;
                stats.last_send_time = Some(UnixMillis::now());
                stats.bytes_sent += msg_size;
            } else {
                break;
            }
        }
        debug!("Stopping sending of messages for queue: {}", self.queue);
    }

    async fn deserialize_incoming(
        self,
        messages: async_channel::Sender<Message>,
    ) -> Result<(), PeerError> {
        let mut next_recv_seq = Wrapping(0);

        while let Ok(bytes) = self.config.peer.recv().await {
            let mut bincode = bincode::de::Deserializer::with_reader(
                &*bytes,
                bincode::options().with_limit(MAX_TRANSMISSION_UNIT),
            );
            let message = match Message::deserialize(&mut bincode) {
                Ok(message) => message,
                Err(err) => {
                    error!("Dropping incoming message. Error while deserialilzing incoming message: {:?}", err);
                    continue;
                }
            };

            let seq = message.sequence_number;
            if message.data.is_sync_message() {
                if let PeerState::Running { remote_magic } = *self.state.read() {
                    if message.magic != remote_magic {
                        continue;
                    }
                }

                // filter out out-of-order packets
                let skipped = seq - next_recv_seq;
                if skipped > MAX_SEQ_DISTANCE {
                    debug!(
                        "dropping out of order packet (seq: {}, last seq: {})",
                        seq, next_recv_seq
                    );
                    continue;
                }
            }

            next_recv_seq = message.sequence_number;
            messages
                .send(message)
                .await
                .map_err(|_| PeerError::LocalDisconnected)?;
        }

        debug!("Stopped receiving messages for queue: {}", self.queue);
        Ok(())
    }

    async fn handle_message(&mut self, message: Message) -> Result<(), PeerError> {
        match message.data {
            MessageData::KeepAlive => Ok(()),
            MessageData::SyncRequest(data) => self.on_sync_request(message.magic, data),
            MessageData::SyncReply(data) => self.on_sync_reply(message.magic, data),
            MessageData::Input(input) => self.on_input(input),
            MessageData::InputAck(data) => {
                self.input_encoder.acknowledge_frame(data.ack_frame);
                Ok(())
            }
            MessageData::QualityReport(data) => self.on_quality_report(data),
            MessageData::QualityReply(data) => {
                self.stats.write().round_trip_time = UnixMillis::now() - data.pong;
                Ok(())
            }
        }
    }

    async fn update_network_stats(self, interval: Duration) {
        let mut start_time: Option<UnixMillis> = None;

        loop {
            Delay::new(interval).await;

            if !self.is_running() {
                start_time = None;
                continue;
            }

            let now = UnixMillis::now();
            if start_time.is_none() {
                start_time = Some(now);
            }

            let mut stats = self.stats.write();
            let total_bytes_sent =
                (stats.bytes_sent + (UDP_HEADER_SIZE * stats.packets_sent)) as f32;
            let seconds = (now - start_time.unwrap()).as_millis() as f32 / 1000.0;
            let bps = total_bytes_sent / seconds;
            let udp_overhead =
                100.0 * (UDP_HEADER_SIZE * stats.packets_sent) as f32 / stats.bytes_sent as f32;
            stats.kbps_sent = (bps / 1024.0) as u32;

            debug!(
                "Network Stats -- Bandwidth: {} KBps   Packets Sent: {} ({} pps) \
                KB Sent: {} UDP Overhead: {:.2}.",
                stats.kbps_sent,
                stats.packets_sent,
                stats.packets_sent as f32 * 1000.0 / (now - start_time.unwrap()).as_millis() as f32,
                total_bytes_sent / 1024.0,
                udp_overhead
            );
        }
    }

    pub fn get_peer_connect_status(&self, id: usize) -> &ConnectionStatus {
        &self.peer_connect_status[id]
    }

    fn on_sync_request(&mut self, magic: u16, data: SyncRequest) -> Result<(), PeerError> {
        let SyncRequest { random } = data;
        if let PeerState::Running { remote_magic } = *self.state.read() {
            if magic != remote_magic {
                debug!(
                    "Ignoring sync request from unknown endpoint ({} != {:?}).",
                    magic, remote_magic
                );
                return Err(PeerError::InvalidMessage);
            }
        }
        self.send(SyncReply { random })?;
        Ok(())
    }

    fn on_sync_reply(&self, magic: u16, data: SyncReply) -> Result<(), PeerError> {
        let mut state = self.state.write();
        if let Some(random) = state.random() {
            if data.random != random {
                debug!("sync reply {} != {}.  Keep looking...", data.random, random);
                return Err(PeerError::InvalidMessage);
            }
        }

        match *state {
            PeerState::Connecting { .. } => {
                self.push_event(Event::<T::Input>::Connected)?;
                state.start_syncing(NUM_SYNC_PACKETS);
                self.send(state.create_sync_request())?;
                Ok(())
            }
            PeerState::Syncing {
                ref mut roundtrips_remaining,
                ..
            } => {
                debug!(
                    "Checking sync state ({} round trips remaining).",
                    *roundtrips_remaining
                );
                debug_assert!(*roundtrips_remaining > 0);
                *roundtrips_remaining -= 1;
                if *roundtrips_remaining == 0 {
                    debug!("Synchronized queue {}!", self.queue);
                    self.push_event(Event::<T::Input>::Synchronized)?;
                    self.stats.write().last_input_packet_recv_time = UnixMillis::now();
                    *state = PeerState::Running {
                        remote_magic: magic,
                    };

                    // FIXME(james7132): If the network is interrupted and a reconnection is completed
                    // if these tasks do not die before they get reevaluated, there will be multiple
                    // alive tasks. This is not the end of the world, but will use extra queue space
                    // and bandwidth.
                    let task_pool = self.config.task_pool.clone();
                    task_pool
                        .spawn(self.clone().heartbeat(KEEP_ALIVE_INTERVAL))
                        .detach();
                    task_pool
                        .spawn(self.clone().send_quality_reports(QUALITY_REPORT_INTERVAL))
                        .detach();
                    task_pool
                        .spawn(self.clone().resend_inputs(QUALITY_REPORT_INTERVAL))
                        .detach();
                    task_pool
                        .spawn(self.clone().update_network_stats(NETWORK_STATS_INTERVAL))
                        .detach();
                } else {
                    self.push_event(Event::<T::Input>::Synchronizing {
                        total: NUM_SYNC_PACKETS,
                        count: NUM_SYNC_PACKETS - *roundtrips_remaining as u8,
                    })?;
                    self.send(state.create_sync_request())?;
                }
                Ok(())
            }
            PeerState::Running { remote_magic } if magic == remote_magic => Ok(()),
            _ => {
                debug!("Ignoring SyncReply while not syncing.");
                Err(PeerError::InvalidMessage)
            }
        }
    }

    fn on_input(&mut self, msg: Input) -> Result<(), PeerError> {
        let Input {
            peer_connect_status,
            start_frame,
            ack_frame,
            bits,
        } = msg;

        // Update the peer connection status if this peer is still considered to be part
        // of the network.
        for (i, remote_status) in peer_connect_status.iter().enumerate() {
            if i < self.peer_connect_status.len() {
                debug_assert!(remote_status.last_frame >= self.peer_connect_status[i].last_frame);
                self.peer_connect_status[i].disconnected |= remote_status.disconnected;
                self.peer_connect_status[i].last_frame = std::cmp::max(
                    self.peer_connect_status[i].last_frame,
                    remote_status.last_frame,
                );
            } else {
                self.peer_connect_status.push(remote_status.clone());
            }
        }

        // Decompress the input.
        match self.input_decoder.decode(start_frame, bits) {
            Ok(inputs) => {
                if !inputs.is_empty() {
                    self.push_event(Event::<T::Input>::Inputs(inputs))?;
                    self.stats.write().last_input_packet_recv_time = UnixMillis::now();
                    self.send(InputAck {
                        ack_frame: self.input_decoder.last_decoded_frame(),
                    })?;
                }
            }
            Err(err) => {
                error!(
                    "Error while decoding recieved inputs. discarding: {:?}",
                    err
                );
                return Err(PeerError::InvalidMessage);
            }
        }

        // Get rid of our buffered input
        self.input_encoder.acknowledge_frame(ack_frame);
        Ok(())
    }

    fn on_quality_report(&self, data: QualityReport) -> Result<(), PeerError> {
        self.stats.write().remote_frame_advantage = data.frame_advantage;
        self.send(QualityReply { pong: data.ping })?;
        Ok(())
    }

    pub fn set_local_frame_number(&self, local_frame: Frame) {
        let mut stats = self.stats.write();
        // Estimate which frame the other guy is one by looking at the
        // last frame they gave us plus some delta for the one-way packet
        // trip time.
        let remote_frame = self.input_decoder.last_decoded_frame()
            + ( (stats.round_trip_time.as_secs() / 2) * TARGET_TPS) as i32;

        // Our frame advantage is how many frames *behind* the other guy
        // we are.  Counter-intuative, I know.  It's an advantage because
        // it means they'll have to predict more often and our moves will
        // pop more frequently.
        stats.local_frame_advantage = remote_frame - local_frame;
    }

    pub fn recommend_frame_delay(&self) -> Frame {
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
