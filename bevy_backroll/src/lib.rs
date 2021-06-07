use backroll::{Command, Commands, Config, Event, GameInput, P2PSession, PlayerHandle};
use bevy_app::{AppBuilder, CoreStage, Events, Plugin};
use bevy_ecs::{
    schedule::{Schedule, ShouldRun, Stage, SystemDescriptor, SystemSet, SystemStage},
    system::{Commands as BevyCommands, System},
    world::World,
};
use tracing::{debug, error};

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

pub struct BackrollStage<T>
where
    T: Config,
{
    schedule: Schedule,
    run_criteria: Option<Box<dyn System<In = (), Out = ShouldRun>>>,
    staller: FrameStaller,
    input_sample_fn:
        Option<Box<dyn System<In = PlayerHandle, Out = T::Input> + Send + Sync + 'static>>,
    save_world_fn:
        Option<Box<dyn System<In = (), Out = (T::State, Option<u64>)> + Send + Sync + 'static>>,
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
                    let (state, checksum) = self.save_world_fn.as_mut().unwrap().run((), world);
                    save_state.save(state, checksum);
                }
                Command::<T>::Load(load_state) => {
                    self.load_world_fn
                        .as_mut()
                        .unwrap()
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

            self.run_commands(session.poll(), world);

            if self.staller.should_stall() {
                continue;
            }

            for player_handle in session.local_players() {
                let input = self
                    .input_sample_fn
                    .as_mut()
                    .unwrap()
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

pub struct BackrollPlugin<T>(std::marker::PhantomData<T>)
where
    T: backroll::Config + Send + Sync;

impl<T: backroll::Config + Send + Sync> Plugin for BackrollPlugin<T> {
    fn build(&self, builder: &mut AppBuilder) {
        let mut schedule = Schedule::default();
        schedule.add_stage(BACKROLL_LOGIC_UPDATE, SystemStage::single_threaded());
        builder.add_event::<backroll::Event>().add_stage_before(
            CoreStage::Update,
            BACKROLL_UPDATE,
            BackrollStage::<T> {
                schedule,
                ..Default::default()
            },
        );
    }
}

pub trait BackrollAppBuilder {
    /// Sets the imput sampler system for Backroll. This is required. Attempting to use
    /// the BackrollPlugin without setting this will result in a panic.
    fn with_input_sampler_system<T: backroll::Config>(
        &mut self,
        system: impl System<In = PlayerHandle, Out = T::Input> + Send + Sync + 'static,
    ) -> &mut Self;

    /// Sets the world save system for Backroll. This is required. Attempting to use
    /// the BackrollPlugin without setting this will result in a panic.
    fn with_world_save_system<T: backroll::Config>(
        &mut self,
        system: impl System<In = (), Out = (T::State, Option<u64>)> + Send + Sync + 'static,
    ) -> &mut Self;

    /// Sets the world load system for Backroll. This is required. Attempting to use
    /// the BackrollPlugin without setting this will result in a panic.
    fn with_world_load_system<T: backroll::Config>(
        &mut self,
        system: impl System<In = T::State, Out = ()> + Send + Sync + 'static,
    ) -> &mut Self;

    /// Sets the [RunCriteria] for the [BackrollStage]. By default this uses a [FixedTimestep]
    /// set to 60 ticks per second.
    ///
    /// [RunCriteria]: bevy_ecs::schedule::RunCriteria
    /// [BackrollStage]: self::BackrollStage
    /// [FixedTimestep]: bevy_core::FixedTimestep
    fn with_rollback_run_citeria<T: backroll::Config>(
        &mut self,
        system: impl System<In = (), Out = ShouldRun>,
    ) -> &mut Self;

    /// Adds a system to the Backroll stage.
    fn with_rollback_system<T: backroll::Config>(
        &mut self,
        system: impl Into<SystemDescriptor>,
    ) -> &mut Self;

    /// Adds a [SystemSet] to the BackrollStage.
    fn with_rollback_system_set<T: backroll::Config>(&mut self, system: SystemSet) -> &mut Self;
}

impl BackrollAppBuilder for AppBuilder {
    fn with_input_sampler_system<T: backroll::Config>(
        &mut self,
        system: impl System<In = PlayerHandle, Out = T::Input> + Send + Sync + 'static,
    ) -> &mut Self {
        let stage = self
            .app
            .schedule
            .get_stage_mut::<BackrollStage<T>>(&BACKROLL_UPDATE)
            .expect("No BackrollStage found! Did you install the plugin?");
        stage.input_sample_fn = Some(Box::new(system));
        self
    }

    fn with_world_save_system<T: backroll::Config>(
        &mut self,
        system: impl System<In = (), Out = (T::State, Option<u64>)> + Send + Sync + 'static,
    ) -> &mut Self {
        let stage = self
            .app
            .schedule
            .get_stage_mut::<BackrollStage<T>>(&BACKROLL_UPDATE)
            .expect("No BackrollStage found! Did you install the plugin?");
        stage.save_world_fn = Some(Box::new(system));
        self
    }

    fn with_world_load_system<T: backroll::Config>(
        &mut self,
        system: impl System<In = T::State, Out = ()> + Send + Sync + 'static,
    ) -> &mut Self {
        let stage = self
            .app
            .schedule
            .get_stage_mut::<BackrollStage<T>>(&BACKROLL_UPDATE)
            .expect("No BackrollStage found! Did you install the plugin?");
        stage.load_world_fn = Some(Box::new(system));
        self
    }

    fn with_rollback_run_citeria<T: backroll::Config>(
        &mut self,
        run_criteria: impl System<In = (), Out = ShouldRun>,
    ) -> &mut Self {
        let stage = self
            .app
            .schedule
            .get_stage_mut::<BackrollStage<T>>(&BACKROLL_UPDATE)
            .expect("No BackrollStage found! Did you install the plugin?");
        stage.run_criteria = Some(Box::new(run_criteria));
        self
    }

    fn with_rollback_system<T: backroll::Config>(
        &mut self,
        system: impl Into<SystemDescriptor>,
    ) -> &mut Self {
        let stage = self
            .app
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
            .app
            .schedule
            .get_stage_mut::<BackrollStage<T>>(&BACKROLL_UPDATE)
            .expect("No BackrollStage found! Did you install the plugin?");
        stage
            .schedule
            .add_system_set_to_stage(BACKROLL_LOGIC_UPDATE, system);
        self
    }
}

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

impl<'a> BackrollCommands for BevyCommands<'a> {
    fn start_backroll_session<T: backroll::Config>(&mut self, session: P2PSession<T>) {
        self.insert_resource(session);
    }

    fn end_backroll_session<T: backroll::Config>(&mut self) {
        self.remove_resource::<P2PSession<T>>();
    }
}
