use bevy_ecs::{component::Component, system::Resource};

/// A marker [`Component`]. Required to mark entities with network state.
///
/// Registered network components will only be saved or loaded with this
/// marker component present.
#[derive(Debug, Component, Copy, Clone, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct NetworkId(pub(crate) u32);

/// A provider resource of new, globally unique [`NetworkId`] components.
///
/// This resource itself is registered as a saveable resource and is guarenteed
/// to deterministically produce IDs across rollbacks.
///
/// This resource is reset upon starting a new session via
/// [`BackrollCommands::start_backroll_session`][start_backroll_session].
///
/// [start_backroll_session]: crate::BackrollCommands::start_backroll_session
#[derive(Resource, Debug, Clone)]
#[repr(transparent)]
pub struct NetworkIdProvider(u32);

impl NetworkIdProvider {
    pub(crate) fn new() -> Self {
        Self(0)
    }

    /// Creates a new, unique [`NetworkId`].
    pub fn new_id(&mut self) -> NetworkId {
        let id = NetworkId(self.0);
        self.0 = self
            .0
            .checked_add(1)
            .expect("NetworkId has overflowed u32::MAX.");
        id
    }
}
