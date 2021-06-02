use crate::{
    input::GameInput,
    sync::{SavedCell, SavedFrame},
    Config, Event, Frame,
};

pub enum Command<T>
where
    T: Config,
{
    Save(SaveState<T>),
    Load(LoadState<T>),
    AdvanceFrame(GameInput<T::Input>),
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
    pub(crate) fn new(cell: SavedCell<T>, frame: Frame) -> Self {
        Self { cell, frame }
    }

    /// Saves a single frame's state to the session's state buffer.
    ///
    /// Note this consumes the SaveState, saving multiple times is
    /// not allowed.
    pub fn save(mut self, state: T::State, checksum: Option<u64>) {
        self.cell.save(SavedFrame::<T> {
            frame: self.frame,
            data: Some(Box::new(state)),
            checksum,
        });
        debug_assert!(self.cell.is_valid());
    }
}

#[cfg(debug_assertions)]
impl<'a, T: Config> Drop for SaveState<T> {
    fn drop(&mut self) {
        if !self.cell.is_valid() {
            panic!("A SaveState command was dropped without saving a valid state.");
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
