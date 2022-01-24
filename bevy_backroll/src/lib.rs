#![warn(missing_docs)]

//! A [Bevy](https://bevyengine.org) plugin that adds support for running
//! [Backroll](https://crates.io/crates/backroll) sessions.
//!
//! Installing the plugin:
//! ```rust no_run
//! use backroll::*;
//! use bytemuck::*;
//! use bevy::prelude::*;
//! use bevy_backroll::*;
//!
//! // Create your Backroll input type
//! #[repr(C)]
//! #[derive(Clone, Copy, Eq, PartialEq, Pod, Zeroable)]
//! pub struct PlayerInput {
//!    // Input data...
//!    pub buttons_pressed: u8,
//! }
//!
//! // Create your state. Must implement Clone.
//! #[derive(Component, Clone)]
//! pub struct PlayerState {
//!    pub handle: PlayerHandle,
//!    pub current_value: u64,
//! }
//!
//! // Sample input from the local player's controller.
//! fn sample_player_input(player: In<PlayerHandle>) -> PlayerInput {
//!    // Sample input data...
//!    PlayerInput {
//!       buttons_pressed: 1,
//!    }
//! }
//!
//! // Use input to advance the game simulation.
//! fn simulate_game(
//!   input: Res<GameInput<PlayerInput>>,
//!   mut query: Query<&mut PlayerState>
//! ) {
//!    for mut player in query.iter_mut() {
//!       if let Ok(input) = input.get(player.handle) {
//!          player.current_value += input.buttons_pressed as u64;
//!       }
//!    }
//! }
//!
//! fn main() {
//!     App::new()
//!         .add_plugin(BackrollPlugin)
//!         .register_rollback_input(sample_player_input.system())
//!         .register_rollback_component::<PlayerState>()
//!         .add_rollback_system(simulate_game)
//!         .run();
//! }
//! ```
use backroll::{
    command::{Command, Commands},
    Config, Event, GameInput, PlayerHandle,
};
use bevy_app::{App, CoreStage, Events, Plugin};
use bevy_ecs::{
    prelude::*,
    schedule::{IntoSystemDescriptor, ShouldRun, Stage, SystemSet, SystemStage},
    system::{Commands as BevyCommands, System},
    world::World,
};
use bevy_log::{debug, error};
use std::marker::PhantomData;

mod id;
mod save_state;
#[cfg(feature = "steam")]
mod steam;

pub use backroll;
pub use id::*;
use save_state::*;

/// A [`P2PSession`] alias for bevy_backroll sessions. Uses [`BevyBackrollConfig`]
/// as the config type.
pub type P2PSession<Input> = backroll::P2PSession<BevyBackrollConfig<Input>>;

/// The [SystemLabel] used by the [BackrollStage] added by [BackrollPlugin].
///
/// [SystemLabel]: bevy_ecs::schedule::SystemLabel
/// [BackrollStage]: self::BackrollStage
/// [BackrollPlugin]: self::BackrollPlugin
#[derive(Debug, Clone, Eq, Hash, StageLabel, PartialEq)]
pub struct BackrollUpdate;

/// Manages when to inject frame stalls to keep in sync with remote players.
struct FrameStaller {
    frames_ahead: u8,
    frames_until_stall: u8,
}

impl FrameStaller {
    pub fn new() -> Self {
        Self {
            frames_ahead: 0,
            frames_until_stall: 0,
        }
    }

    pub fn reset(&mut self, frames_ahead: u8) {
        self.frames_ahead = frames_ahead;
        self.frames_until_stall = self.stall_cadence();
    }

    pub fn should_stall(&mut self) -> bool {
        if self.frames_ahead == 0 {
            return false;
        }
        if self.frames_until_stall == 0 {
            self.frames_ahead -= 1;
            self.frames_until_stall = self.stall_cadence();
            true
        } else {
            self.frames_until_stall -= 1;
            false
        }
    }

    fn stall_cadence(&self) -> u8 {
        // Linearly decay the cadence based on how many frames ahead
        // the is. This will result in fast initial catch up and then
        // slowly smooth out small hitches.
        if self.frames_ahead > 9 {
            1
        } else {
            11 - self.frames_ahead
        }
    }
}

struct BackrollStages {
    save: SystemStage,
    simulate: SystemStage,
    before_load: SystemStage,
    load: SystemStage,
    run_criteria: Option<Box<dyn System<In = (), Out = ShouldRun>>>,
}

/// The Backroll config type for bevy_backroll sessions.
pub struct BevyBackrollConfig<Input> {
    _marker: PhantomData<Input>,
}

impl<Input: PartialEq + bytemuck::Pod + bytemuck::Zeroable + Send + Sync> Config
    for BevyBackrollConfig<Input>
{
    type Input = Input;
    type State = SaveState;
}

/// A [Stage] that transparently runs and handles Backroll sessions.
///
/// Each time the stage runs, it will poll the Backroll session, sample local player
/// inputs for the session, then advance the frame.
///
/// The stage will automatically handle Backroll commands by doing the following:
///  
///  - [Command::Save]: Saves an immutable copy of the components and resoures from the
///    main app [`World`] into a save state.
///  - [Command::Load]: Loads a prior saved World state into the main app [`World`].
///  - [Command::AdvanceFrame]: Injects the provided [GameInput] as a resource then
///    runs all simulation based systems once (see: [add_rollback_system])
///  - [Command::Event]: Forwards all events to Bevy. Can be read out via [EventReader].
///    Automatically handles time synchronization by smoothly injecting stall frames when
///    ahead of remote players.
///
/// This stage is best used with a [FixedTimestep] run criteria to ensure that the systems
/// are running at a consistent rate on all players in the game.
///
/// This stage will only run when there is a [P2PSession] with the same [Config] parameter
/// registered as a resource within the running World. If the stage was added via
/// [BackrollPlugin], [BackrollCommands::start_backroll_session] and [BackrollCommands::end_backroll_session]
/// can be used to start or end a session.
///
/// [Stage]: bevy_ecs::schedule::Stage
/// [World]: bevy_ecs::world::World
/// [Command]: backroll::Command
/// [BackrollCommands]: self::BackrollCommands
/// [FixedTimestep]: bevy_core::FixedTimestep
/// [EventReader]: bevy_app::EventReader
/// [add_rollback_system]: self::BackrollApp::add_rollback_system
pub struct BackrollStage<Input>
where
    Input: PartialEq + bytemuck::Pod + bytemuck::Zeroable + Send + Sync,
{
    staller: FrameStaller,
    input_sample_fn: Box<dyn System<In = PlayerHandle, Out = Input> + Send + Sync + 'static>,
}

impl<Input> BackrollStage<Input>
where
    Input: PartialEq + bytemuck::Pod + bytemuck::Zeroable + Send + Sync,
{
    fn run_commands(&mut self, commands: Commands<BevyBackrollConfig<Input>>, world: &mut World) {
        world.resource_scope(|world, mut stages: Mut<BackrollStages>| {
            for command in commands {
                match command {
                    Command::Save(save_state) => {
                        world.insert_resource(SaveStateBuilder::new());
                        stages.save.run(world);
                        // TODO(james7132): Find a way to hash the state here generically.
                        save_state.save_without_hash(
                            world.remove_resource::<SaveStateBuilder>().unwrap().build(),
                        );
                    }
                    Command::Load(load_state) => {
                        world.insert_resource(load_state.load());
                        stages.before_load.run(world);
                        stages.load.run(world);
                        world.remove_resource::<SaveState>();
                    }
                    Command::AdvanceFrame(inputs) => {
                        // Insert input via Resource
                        *world.get_resource_mut::<GameInput<Input>>().unwrap() = inputs;
                        stages.simulate.run(world);
                    }
                    Command::Event(evt) => {
                        debug!("Received Backroll Event: {:?}", evt);

                        // Update time sync stalls properly.
                        if let Event::TimeSync { frames_ahead } = &evt {
                            self.staller.reset(*frames_ahead);
                        }

                        let mut events = world.get_resource_mut::<Events<Event>>().unwrap();
                        events.send(evt.clone());
                    }
                }
            }
        });
    }
}

impl<Input> Stage for BackrollStage<Input>
where
    Input: PartialEq + bytemuck::Pod + bytemuck::Zeroable + Send + Sync,
{
    fn run(&mut self, world: &mut World) {
        loop {
            let should_run = world.resource_scope(|world, mut stages: Mut<BackrollStages>| {
                if let Some(ref mut run_criteria) = stages.run_criteria {
                    run_criteria.run((), world)
                } else {
                    ShouldRun::Yes
                }
            });

            if let ShouldRun::No = should_run {
                return;
            }

            let session = if let Some(session) = world.get_resource_mut::<P2PSession<Input>>() {
                session.clone()
            } else {
                // No ongoing session, don't run.
                return;
            };

            self.run_commands(session.poll(), world);

            if self.staller.should_stall() {
                continue;
            }

            for player_handle in session.local_players() {
                let input = self.input_sample_fn.run(player_handle, world);
                if let Err(err) = session.add_local_input(player_handle, input) {
                    error!(
                        "Error while adding local input for {:?}: {:?}",
                        player_handle, err
                    );
                    return;
                }
            }

            self.run_commands(session.advance_frame(), world);

            if let ShouldRun::Yes = should_run {
                return;
            }
        }
    }
}

/// A Bevy plugin that adds a [BackrollStage] to the app.
///
/// **Note:** This stage does not enforce any specific system execution order.
/// Users of this stage should ensure that their included systems have a strict
/// deterministic execution order, otherwise simulation may result in desyncs.
///
/// Also registers Backroll's [Event] as an event type, which the stage will
/// forward to Bevy.
///
/// If the feature is enabled, this will also register the associated transport
/// layer implementations for Steam.
///
/// [BackrolLStage]: self::BackrollStage
/// [Event]: backroll::Event
#[derive(Default)]
pub struct BackrollPlugin;

impl Plugin for BackrollPlugin {
    fn build(&self, app: &mut App) {
        let mut save = SystemStage::parallel();
        save.add_system(save_network_ids);
        let mut before_load = SystemStage::parallel();
        before_load.add_system(sync_network_ids);
        app.add_event::<backroll::Event>()
            .insert_resource(NetworkIdProvider::new())
            .insert_resource(BackrollStages {
                save,
                simulate: SystemStage::parallel(),
                before_load,
                load: SystemStage::parallel(),
                run_criteria: None,
            })
            .register_rollback_resource::<NetworkIdProvider>();

        #[cfg(feature = "steam")]
        app.add_plugin(steam::BackrollSteamPlugin);
    }
}

/// Extension trait for configuring [App]s using a [BackrollPlugin].
///
/// [App]: bevy_app::App
/// [BackrollPlugin]: self::BackrollPlugin
pub trait BackrollApp {
    /// Sets the input sampler system for Backroll. This is required. Backroll will
    /// not start without this being set.
    fn register_rollback_input<Input, S>(&mut self, system: S) -> &mut Self
    where
        Input: PartialEq + bytemuck::Pod + bytemuck::Zeroable + Send + Sync,
        S: System<In = PlayerHandle, Out = Input> + Send + Sync + 'static;

    /// Registers a specific component type for saving into Backroll's save states.
    /// Any game simulation state stored in components should be registered here.
    fn register_rollback_component<T: Component + Clone>(&mut self) -> &mut Self;

    /// Registers a specific resource type for saving into Backroll's save states.
    /// Any game simulation state stored in resources should be registered here.
    fn register_rollback_resource<T: Clone + Send + Sync + 'static>(&mut self) -> &mut Self;

    /// Sets the [RunCriteria] for the [BackrollStage]. By default this uses a [FixedTimestep]
    /// set to 60 ticks per second.
    ///
    /// [RunCriteria]: bevy_ecs::schedule::RunCriteria
    /// [BackrollStage]: self::BackrollStage
    /// [FixedTimestep]: bevy_core::FixedTimestep
    fn with_rollback_run_criteria<Input, S>(&mut self, system: S) -> &mut Self
    where
        S: System<In = (), Out = ShouldRun>;

    /// Adds a system to the Backroll stage.
    fn add_rollback_system<S, U>(&mut self, system: S) -> &mut Self
    where
        S: IntoSystemDescriptor<U>;

    /// Adds a [SystemSet] to the BackrollStage.
    ///
    /// [SystemSet]: bevy_ecs::schedule::SystemSet
    fn add_rollback_system_set(&mut self, system: impl Into<SystemSet>) -> &mut Self;
}

impl BackrollApp for App {
    fn register_rollback_input<Input, S>(&mut self, system: S) -> &mut Self
    where
        Input: PartialEq + bytemuck::Pod + bytemuck::Zeroable + Send + Sync,
        S: System<In = PlayerHandle, Out = Input> + Send + Sync + 'static,
    {
        self.insert_resource(GameInput::<Input>::default());
        self.add_stage_before(
            CoreStage::Update,
            BackrollUpdate,
            BackrollStage::<Input> {
                staller: FrameStaller::new(),
                input_sample_fn: Box::new(system),
            },
        );

        self
    }

    fn register_rollback_component<T: Component + Clone>(&mut self) -> &mut Self {
        let mut stages = self
            .world
            .get_resource_mut::<BackrollStages>()
            .expect("No BackrollStages found! Did you install the plugin?");
        stages.load.add_system(load_components::<T>);
        stages.save.add_system(save_components::<T>);
        self
    }

    fn register_rollback_resource<T: Clone + Send + Sync + 'static>(&mut self) -> &mut Self {
        let mut stages = self
            .world
            .get_resource_mut::<BackrollStages>()
            .expect("No BackrollStages found! Did you install the plugin?");
        stages.load.add_system(load_resource::<T>);
        stages.save.add_system(save_resource::<T>);
        self
    }

    fn with_rollback_run_criteria<Input, S>(&mut self, run_criteria: S) -> &mut Self
    where
        S: System<In = (), Out = ShouldRun>,
    {
        self.world
            .get_resource_mut::<BackrollStages>()
            .expect("No BackrollStages found! Did you install the plugin?")
            .run_criteria = Some(Box::new(run_criteria));
        self
    }

    fn add_rollback_system<S, U>(&mut self, system: S) -> &mut Self
    where
        S: IntoSystemDescriptor<U>,
    {
        self.world
            .get_resource_mut::<BackrollStages>()
            .expect("No BackrollStages found! Did you install the plugin?")
            .simulate
            .add_system(system);
        self
    }

    fn add_rollback_system_set(&mut self, system: impl Into<SystemSet>) -> &mut Self {
        self.world
            .get_resource_mut::<BackrollStages>()
            .expect("No BackrollStages found! Did you install the plugin?")
            .simulate
            .add_system_set(system.into());
        self
    }
}

/// Extension trait for [Commands] to start and stop Backroll sessions.
///
/// [Commands]: bevy_ecs::system::Commands
pub trait BackrollCommands {
    /// Starts a new Backroll session. If one is already in progress, it will be replaced
    /// and the old session will be dropped. This will add the session as a resource, which
    /// can be accessed via [Res].
    ///
    /// [Res]: bevy_ecs::system::Res
    fn start_backroll_session<Input>(&mut self, session: P2PSession<Input>)
    where
        Input: PartialEq + bytemuck::Pod + bytemuck::Zeroable + Send + Sync;

    /// Ends the ongoing Backroll session. This will remove the associated resource and drop
    /// the session.
    ///
    /// Does nothing if there is no ongoing session.
    fn end_backroll_session<Input>(&mut self)
    where
        Input: PartialEq + bytemuck::Pod + bytemuck::Zeroable + Send + Sync;
}

impl<'w, 's> BackrollCommands for BevyCommands<'w, 's> {
    fn start_backroll_session<Input>(&mut self, session: P2PSession<Input>)
    where
        Input: PartialEq + bytemuck::Pod + bytemuck::Zeroable + Send + Sync,
    {
        // Reset the NetworkIdProvider on the start of a new session.
        self.insert_resource(NetworkIdProvider::new());
        self.insert_resource(session);
    }

    fn end_backroll_session<Input>(&mut self)
    where
        Input: PartialEq + bytemuck::Pod + bytemuck::Zeroable + Send + Sync,
    {
        self.remove_resource::<P2PSession<Input>>();
    }
}
