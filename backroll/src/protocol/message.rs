use super::ConnectionStatus;
use crate::{time_sync::UnixMillis, Frame};
use rkyv::{Archive, Deserialize, Serialize};

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
pub(super) struct Message {
    pub magic: u16,
    pub sequence_number: u16,
    pub data: MessageData,
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
pub(super) enum MessageData {
    KeepAlive,
    SyncRequest(SyncRequest),
    SyncReply(SyncReply),
    Input(Input),
    InputAck(InputAck),
    QualityReport(QualityReport),
    QualityReply(QualityReply),
}

impl MessageData {
    pub fn is_sync_message(&self) -> bool {
        match self {
            Self::SyncRequest(_) | Self::SyncReply(_) => true,
            _ => false,
        }
    }
}

impl From<SyncRequest> for MessageData {
    fn from(value: SyncRequest) -> Self {
        Self::SyncRequest(value)
    }
}

impl From<SyncReply> for MessageData {
    fn from(value: SyncReply) -> Self {
        Self::SyncReply(value)
    }
}

impl From<Input> for MessageData {
    fn from(value: Input) -> Self {
        Self::Input(value)
    }
}

impl From<InputAck> for MessageData {
    fn from(value: InputAck) -> Self {
        Self::InputAck(value)
    }
}

impl From<QualityReport> for MessageData {
    fn from(value: QualityReport) -> Self {
        Self::QualityReport(value)
    }
}

impl From<QualityReply> for MessageData {
    fn from(value: QualityReply) -> Self {
        Self::QualityReply(value)
    }
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
pub(super) struct Input {
    pub peer_connect_status: Vec<ConnectionStatus>,
    pub start_frame: Frame,
    pub ack_frame: Frame,
    pub bits: Vec<u8>,
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
pub(super) struct InputAck {
    pub ack_frame: Frame,
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
pub(super) struct SyncRequest {
    pub random: u32,
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
pub(super) struct SyncReply {
    pub random: u32,
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
pub(super) struct QualityReport {
    pub frame_advantage: i32,
    pub ping: UnixMillis,
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
pub(super) struct QualityReply {
    pub pong: UnixMillis,
}
