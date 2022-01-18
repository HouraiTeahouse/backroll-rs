use bevy_ecs::component::Component;

/// A marker [`Component`]. Required to mark entities with network state.
///
/// Registered network components will only be saved or loaded with this
/// marker component present.
#[derive(Debug, Component, Copy, Clone, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct NetworkId(u64);

/// A provider resource of new, globally unique [`NetworkId`] components.
/// 
/// This resource itself is registered as a saveable resource and is guarenteed
/// to deterministically produce IDs across rollbacks.
/// 
/// This resource is reset upon starting a new session via 
/// [`BackrollCommands::start_new_session`][start_new_session].
/// 
/// [start_new_session]: crate::BackrollCommands::start_new_session
#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct NetworkIdProvider(u64);

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
            .expect("NetworkId has overflowed u64::MAX.");
        id
    }
}
