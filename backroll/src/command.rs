use crate::{
    input::GameInput,
    sync::{SavedCell, SavedFrame},
    Config, Event, Frame,
};
use tracing::{error, info};

pub enum Command<T>
where
    T: Config,
{
    /// The client should copy the entire contents of the current game state into a
    ///  new state struct and return it.
    ///
    /// Optionally, the client can compute a 64-bit checksum of the data and return it.
    Save(SaveState<T>),

    /// Backroll will issue this command at the beginning of a rollback. The argument
    /// provided will be a previously saved state returned from the save_state function.  
    /// The client should make the current game state match the state contained in the
    /// argument.
    Load(LoadState<T>),

    /// Clients should advance the game state by exactly one frame.  
    /// The provided inputs will contain the inputs you should use for the given frame.
    AdvanceFrame(GameInput<T::Input>),

    /// Notification that something has happened in the lower level protocols. See the
    /// `[Event]` struct for more information.
    Event(Event),
}

/// A command for saving the state of the game.
///
/// Consumers MUST call save before the command is dropped. In debug mode,
/// failure to do so will panic.
pub struct SaveState<T>
where
    T: Config,
{
    pub(crate) cell: SavedCell<T>,
    pub(crate) frame: Frame,
}

impl<T: Config> SaveState<T> {
    /// Saves a single frame's state to the session's state buffer.
    ///
    /// Note this consumes the SaveState, saving multiple times is
    /// not allowed.
    pub fn save(self, state: T::State, checksum: Option<u64>) {
        info!(
            "=== Saved frame state {} (checksum: {:08x}).",
            self.frame,
            checksum.unwrap_or(0)
        );
        self.cell.save(SavedFrame::<T> {
            frame: self.frame,
            data: Some(Box::new(state)),
            checksum,
        });
        debug_assert!(self.cell.is_valid());
    }
}

#[cfg(debug_assertions)]
impl<T: Config> Drop for SaveState<T> {
    fn drop(&mut self) {
        if !self.cell.is_valid() {
            error!("A SaveState command was dropped without saving a valid state.");
        }
    }
}

/// A command for loading a saved state of the game.
pub struct LoadState<T>
where
    T: Config,
{
    pub(crate) cell: SavedCell<T>,
}

impl<T: Config> LoadState<T> {
    /// Loads the saved state of the game.
    ///
    /// This will clone the internal copy ofthe save state. For games with
    /// potentially large save state, this might be expensive.
    ///
    /// Note this consumes the LoadState, loading multiple times is
    /// not allowed.
    pub fn load(self) -> T::State {
        self.cell.load()
    }
}

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
