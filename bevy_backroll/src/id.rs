use bevy_ecs::component::Component;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_NETWORK_ID: AtomicU64 = AtomicU64::new(0);

/// A marker [`Component`]. Required to mark entities with network state.
///
/// Registered network components will only be saved or loaded with this
/// marker component present.
///
/// Backed by a `u64` generated from a global [`AtomicU64`].
#[derive(Debug, Component, Copy, Clone, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct NetworkId(u64);

impl NetworkId {
    /// Creates a new `NetworkID`.
    pub fn new() -> Self {
        Self(NEXT_NETWORK_ID.fetch_add(1, Ordering::Relaxed))
    }
}
