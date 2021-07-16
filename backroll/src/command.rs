use crate::{
    input::GameInput,
    sync::{SavedCell, SavedFrame},
    Config, Event, Frame,
};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};
use tracing::{debug, error};

/// A singular command for a Backroll session client to execute.
///
/// Proper execution of the command is not optional, and must be done in the exact
/// order [Commands] returns it in. Filtering or altering the order in which commands
/// are executed, or dropping commands without executing them may result incorrect
/// simulation and/or panics.
///
/// [Commands]: self::Commands
pub enum Command<T>
where
    T: Config,
{
    /// The client should copy the entire contents of the current game state into a
    ///  new state struct and return it.
    ///
    /// Optionally, the client can compute a 64-bit checksum of the data and return it.
    Save(SaveState<T::State>),

    /// Backroll will issue this command at the beginning of a rollback. The argument
    /// provided will be a previously saved state returned from the save_state function.  
    /// The client should make the current game state match the state contained in the
    /// argument.
    Load(LoadState<T::State>),

    /// Clients should advance the game state by exactly one frame.  
    /// The provided inputs will contain the inputs you should use for the given frame.
    AdvanceFrame(GameInput<T::Input>),

    /// Notification that something has happened in the lower level protocols. See the
    /// `[Event]` struct for more information.
    Event(Event),
}

/// A command for saving the state of the game.
///
/// Consumers MUST save before the command is dropped. Failure to do so will
/// result in a panic.
pub struct SaveState<T> {
    pub(crate) cell: SavedCell<T>,
    pub(crate) frame: Frame,
}

impl<T: Clone> SaveState<T> {
    /// Saves a single frame's state to the session's state buffer and uses
    /// the hash of the state as the checksum. This uses the
    /// [DefaultHasher] implementation.
    ///
    /// This consumes the SaveState, saving multiple times is not allowed.
    ///
    /// [DefaultHasher]: std::collections::hash_map::DefaultHasher
    pub fn save(self, state: T)
    where
        T: Hash,
    {
        let mut hasher = DefaultHasher::new();
        state.hash(&mut hasher);
        self.save_with_hash(state, hasher.finish());
    }

    /// Saves a single frame's state to the session's state buffer without
    /// a saved checksum.
    ///
    /// This consumes the SaveState, saving multiple times is not allowed.
    pub fn save_without_hash(self, state: T) {
        self.save_state(state, None);
    }

    /// Saves a single frame's state to the session's state buffer with a
    /// provided checksum.
    ///
    /// This consumes the SaveState, saving multiple times is not allowed.
    pub fn save_with_hash(self, state: T, checksum: u64) {
        self.save_state(state, Some(checksum));
    }

    fn save_state(self, state: T, checksum: Option<u64>) {
        debug!(
            "=== Saved frame state {} (checksum: {:08x}).",
            self.frame,
            checksum.unwrap_or(0)
        );
        self.cell.save(SavedFrame::<T> {
            frame: self.frame,
            data: Some(Box::new(state)),
            checksum,
        });
        assert!(self.cell.is_valid());
    }
}

impl<T> Drop for SaveState<T> {
    fn drop(&mut self) {
        if !self.cell.is_valid() {
            error!("A SaveState command was dropped without saving a valid state.");
        }
    }
}

/// A command for loading a saved state of the game.
pub struct LoadState<T> {
    pub(crate) cell: SavedCell<T>,
}

impl<T: Clone> LoadState<T> {
    /// Loads the saved state of the game.
    ///
    /// This will clone the internal copy ofthe save state. For games with
    /// potentially large save state, this might be expensive.
    ///
    /// Note this consumes the LoadState, loading multiple times is
    /// not allowed.
    pub fn load(self) -> T {
        self.cell.load()
    }
}

/// An ordered container of commands for clients to execute.
///
/// Proper execution of the command is not optional, and must be done in the exact
/// order they are returned in. Filtering or altering the order in which commands
/// are executed, or dropping commands without executing them may result incorrect
/// simulation and/or panics.
pub struct Commands<T>
where
    T: Config,
{
    commands: Vec<Command<T>>,
}

impl<T: Config> Commands<T> {
    pub(crate) fn push(&mut self, command: Command<T>) {
        self.commands.push(command);
    }
}

impl<T: Config> Default for Commands<T> {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
        }
    }
}

impl<T: Config> IntoIterator for Commands<T> {
    type Item = Command<T>;
    type IntoIter = std::vec::IntoIter<Command<T>>;
    fn into_iter(self) -> Self::IntoIter {
        self.commands.into_iter()
    }
}
