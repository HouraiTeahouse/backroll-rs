//! A [Bevy](https://bevyengine.org) plugin that adds support for running
//! [Backroll](https://crates.io/crates/backroll) sessions.
//!
//! Installing the plugin:
//! ```rust no_run
//! use bevy::prelude::*;
//! use bevy_backroll::*;
//!
//! // Create your Backroll Config
//! pub struct BackrollConfig;
//!
//! impl backroll::Config for BackrollConfig {
//!     type Input = i32;
//!     type State = i32;
//! }
//!
//! fn main() {
//!     App::build()
//!         .add_plugin(BackrollPlugin::<BackrollConfig>::default())
//!         .run();
//! }
//! ```
use backroll::{
    command::{Command, Commands},
    Config, Event, GameInput, P2PSession, PlayerHandle,
};
use bevy_app::{App, CoreStage, Events, Plugin};
use bevy_core::FixedTimestep;
use bevy_ecs::{
    schedule::{IntoSystemDescriptor, Schedule, ShouldRun, Stage, SystemSet, SystemStage},
    system::{Commands as BevyCommands, System},
    world::World,
};
use bevy_log::{debug, error};

#[cfg(feature = "steam")]
mod steam;

pub use backroll;

/// The [SystemLabel] used by the [BackrollStage] added by [BackrollPlugin].
///
/// [SystemLabel]: bevy_ecs::schedule::SystemLabel
/// [BackrollStage]: self::BackrollStage
/// [BackrollPlugin]: self::BackrollPlugin
pub const BACKROLL_UPDATE: &str = "backroll_update";
const BACKROLL_LOGIC_UPDATE: &str = "backroll_logic_update";

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

/// A [Stage] that transparently runs and handles Backroll sessions.
///
/// Each time the stage runs, it will poll the Backroll session, sample local player
/// inputs for the session, then advance the frame.
///
/// The stage will automatically handle Backroll commands by doing the following:
///  
///  - [Command::Save]: Runs the system registered through [with_world_save_system]
///    to make a save state of the [World].
///  - [Command::Load]: Runs the system registered through [with_world_load_system]
///    to restore a save state of the [World].
///  - [Command::AdvanceFrame]: Injects the provided [GameInput] as a resource then
///    runs all simulation based systems once (see: [with_rollback_system])
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
/// [with_world_save_system]: self::BackrollAppBuilder::with_world_save_system
/// [with_world_load_system]: self::BackrollAppBuilder::with_world_save_system
/// [with_rollback_system]: self::BackrollAppBuilder::with_rollback_system
pub struct BackrollStage<T>
where
    T: Config,
{
    pub schedule: Schedule,
    run_criteria: Option<Box<dyn System<In = (), Out = ShouldRun>>>,
    staller: FrameStaller,
    input_sample_fn:
        Option<Box<dyn System<In = PlayerHandle, Out = T::Input> + Send + Sync + 'static>>,
    save_world_fn: Option<Box<dyn System<In = (), Out = T::State> + Send + Sync + 'static>>,
    load_world_fn: Option<Box<dyn System<In = T::State, Out = ()> + Send + Sync + 'static>>,
}

impl<T: Config> Default for BackrollStage<T> {
    fn default() -> Self {
        Self {
            schedule: Default::default(),
            run_criteria: None,
            staller: FrameStaller::new(),
            input_sample_fn: None,
            save_world_fn: None,
            load_world_fn: None,
        }
    }
}

impl<T: Config> BackrollStage<T> {
    fn run_commands(&mut self, commands: Commands<T>, world: &mut World) {
        for command in commands {
            match command {
                Command::<T>::Save(save_state) => {
                    let state = self
                        .save_world_fn
                        .as_mut()
                        .expect(
                            "No world save system found. Please use App::with_world_load_system",
                        )
                        .run((), world);
                    // TODO(james7132): Find a way to hash the state here generically.
                    save_state.save_without_hash(state);
                }
                Command::<T>::Load(load_state) => {
                    self.load_world_fn
                        .as_mut()
                        .expect(
                            "No world load system found. Please use App::with_world_load_system",
                        )
                        .run(load_state.load(), world);
                }
                Command::AdvanceFrame(inputs) => {
                    // Insert input via Resource
                    *world.get_resource_mut::<GameInput<T::Input>>().unwrap() = inputs;
                    self.schedule.run_once(world);
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
    }
}

impl<T: Config> Stage for BackrollStage<T> {
    fn run(&mut self, world: &mut World) {
        loop {
            let should_run = if let Some(ref mut run_criteria) = self.run_criteria {
                run_criteria.run((), world)
            } else {
                ShouldRun::Yes
            };

            if let ShouldRun::No = should_run {
                return;
            }

            let session = if let Some(session) = world.get_resource_mut::<P2PSession<T>>() {
                session.clone()
            } else {
                // No ongoing session, don't run.
                return;
            };

            world.insert_resource(GameInput::<T::Input>::default());
            self.run_commands(session.poll(), world);
            world.remove_resource::<GameInput<T::Input>>();

            if self.staller.should_stall() {
                continue;
            }

            for player_handle in session.local_players() {
                let input = self
                    .input_sample_fn
                    .as_mut()
                    .expect(
                        "No input sampler system found. Please use App::with_input_sampler_system",
                    )
                    .run(player_handle, world);
                if let Err(err) = session.add_local_input(player_handle, input) {
                    error!(
                        "Error while adding local input for {:?}: {:?}",
                        player_handle, err
                    );
                    return;
                }
            }

            world.insert_resource(GameInput::<T::Input>::default());
            self.run_commands(session.advance_frame(), world);
            world.remove_resource::<GameInput<T::Input>>();

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
pub struct BackrollPlugin<T>(std::marker::PhantomData<T>)
where
    T: backroll::Config + Send + Sync;

impl<T: backroll::Config + Send + Sync> Plugin for BackrollPlugin<T> {
    fn build(&self, builder: &mut App) {
        let mut schedule = Schedule::default();
        schedule.add_stage(BACKROLL_LOGIC_UPDATE, SystemStage::parallel());
        builder.add_event::<backroll::Event>().add_stage_before(
            CoreStage::Update,
            BACKROLL_UPDATE,
            BackrollStage::<T> {
                schedule,
                run_criteria: Some(Box::new(FixedTimestep::steps_per_second(60.0))),
                ..Default::default()
            },
        );

        #[cfg(feature = "steam")]
        builder.add_plugin(steam::BackrollSteamPlugin);
    }
}

impl<T: Config + Send + Sync> Default for BackrollPlugin<T> {
    fn default() -> Self {
        Self(Default::default())
    }
}

/// Extension trait for configuring [App]s using a [BackrollPlugin].
///
/// [App]: bevy_app::AppBuilder
/// [BackrollPlugin]: self::BackrollPlugin
pub trait BackrollApp {
    /// Sets the input sampler system for Backroll. This is required. Attempting to start
    /// a Backroll session without setting this will result in a panic.
    fn with_input_sampler_system<T, S>(&mut self, system: S) -> &mut Self
    where
        T: backroll::Config,
        S: System<In = PlayerHandle, Out = T::Input> + Send + Sync + 'static;

    /// Sets the world save system for Backroll. This is required. Attempting to start a
    /// Backroll session without setting this will result in a panic.
    fn with_world_save_system<T, S>(&mut self, system: S) -> &mut Self
    where
        T: backroll::Config,
        S: System<In = (), Out = T::State> + Send + Sync + 'static;

    /// Sets the world load system for Backroll. This is required. Attempting to start a
    /// Backroll session without setting this will result in a panic.
    fn with_world_load_system<T, S>(&mut self, system: S) -> &mut Self
    where
        T: backroll::Config,
        S: System<In = T::State, Out = ()> + Send + Sync + 'static;

    /// Sets the [RunCriteria] for the [BackrollStage]. By default this uses a [FixedTimestep]
    /// set to 60 ticks per second.
    ///
    /// [RunCriteria]: bevy_ecs::schedule::RunCriteria
    /// [BackrollStage]: self::BackrollStage
    /// [FixedTimestep]: bevy_core::FixedTimestep
    fn with_rollback_run_criteria<T, S>(&mut self, system: S) -> &mut Self
    where
        T: backroll::Config,
        S: System<In = (), Out = ShouldRun>;

    /// Adds a system to the Backroll stage.
    fn with_rollback_system<T, S, U>(&mut self, system: S) -> &mut Self
    where
        T: backroll::Config,
        S: IntoSystemDescriptor<U>;

    /// Adds a [SystemSet] to the BackrollStage.
    ///
    /// [SystemSet]: bevy_ecs::schedule::SystemSet
    fn with_rollback_system_set<T: backroll::Config>(&mut self, system: SystemSet) -> &mut Self;
}

impl BackrollApp for App {
    fn with_input_sampler_system<T, S>(&mut self, mut system: S) -> &mut Self
    where
        T: backroll::Config,
        S: System<In = PlayerHandle, Out = T::Input> + Send + Sync + 'static,
    {
        system.initialize(&mut self.world);
        let stage = self
            .schedule
            .get_stage_mut::<BackrollStage<T>>(&BACKROLL_UPDATE)
            .expect("No BackrollStage found! Did you install the plugin?");
        stage.input_sample_fn = Some(Box::new(system));
        self
    }

    fn with_world_save_system<T, S>(&mut self, mut system: S) -> &mut Self
    where
        T: backroll::Config,
        S: System<In = (), Out = T::State> + Send + Sync + 'static,
    {
        system.initialize(&mut self.world);
        let stage = self
            .schedule
            .get_stage_mut::<BackrollStage<T>>(&BACKROLL_UPDATE)
            .expect("No BackrollStage found! Did you install the plugin?");
        stage.save_world_fn = Some(Box::new(system));
        self
    }

    fn with_world_load_system<T, S>(&mut self, mut system: S) -> &mut Self
    where
        T: backroll::Config,
        S: System<In = T::State, Out = ()> + Send + Sync + 'static,
    {
        system.initialize(&mut self.world);
        let stage = self
            .schedule
            .get_stage_mut::<BackrollStage<T>>(&BACKROLL_UPDATE)
            .expect("No BackrollStage found! Did you install the plugin?");
        stage.load_world_fn = Some(Box::new(system));
        self
    }

    fn with_rollback_run_criteria<T, S>(&mut self, mut run_criteria: S) -> &mut Self
    where
        T: backroll::Config,
        S: System<In = (), Out = ShouldRun>,
    {
        run_criteria.initialize(&mut self.world);
        let stage = self
            .schedule
            .get_stage_mut::<BackrollStage<T>>(&BACKROLL_UPDATE)
            .expect("No BackrollStage found! Did you install the plugin?");
        stage.run_criteria = Some(Box::new(run_criteria));
        self
    }

    fn with_rollback_system<T, S, U>(&mut self, system: S) -> &mut Self
    where
        T: backroll::Config,
        S: IntoSystemDescriptor<U>,
    {
        let stage = self
            .schedule
            .get_stage_mut::<BackrollStage<T>>(&BACKROLL_UPDATE)
            .expect("No BackrollStage found! Did you install the plugin?");
        stage
            .schedule
            .add_system_to_stage(BACKROLL_LOGIC_UPDATE, system);
        self
    }

    fn with_rollback_system_set<T: backroll::Config>(&mut self, system: SystemSet) -> &mut Self {
        let stage = self
            .schedule
            .get_stage_mut::<BackrollStage<T>>(&BACKROLL_UPDATE)
            .expect("No BackrollStage found! Did you install the plugin?");
        stage
            .schedule
            .add_system_set_to_stage(BACKROLL_LOGIC_UPDATE, system);
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
    fn start_backroll_session<T: backroll::Config>(&mut self, session: P2PSession<T>);

    /// Ends the ongoing Backroll session. This will remove the associated resource and drop
    /// the session.
    ///
    /// Does nothing if there is no ongoing session.
    fn end_backroll_session<T: backroll::Config>(&mut self);
}

impl<'w, 's> BackrollCommands for BevyCommands<'w, 's> {
    fn start_backroll_session<T: backroll::Config>(&mut self, session: P2PSession<T>) {
        self.insert_resource(session);
    }

    fn end_backroll_session<T: backroll::Config>(&mut self) {
        self.remove_resource::<P2PSession<T>>();
    }
}
