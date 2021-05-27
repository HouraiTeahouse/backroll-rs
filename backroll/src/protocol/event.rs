use crate::input::FrameInput;
use std::time::Duration;

pub(crate) enum Event<T> {
    Connected,
    Synchronizing { total: u8, count: u8 },
    Synchronized,
    Inputs(Vec<FrameInput<T>>),
    NetworkInterrupted { disconnect_timeout: Duration },
    NetworkResumed,
}
