use backroll::{
    BackrollConfig, BackrollEvent, BackrollPlayerHandle, GameInput, P2PSession, SessionCallbacks,
};
use bevy_ecs::{
    schedule::{Schedule, ShouldRun, Stage},
    system::System,
    world::World,
};

pub const BACKROLL_UPDATE: &str = "backroll_update";

struct BackrollStageCallbacks<'a, T>
where
    T: BackrollConfig,
{
    world: &'a mut World,
    schedule: &'a mut Schedule,
    save_world_fn:
        &'a mut (dyn System<In = (), Out = (T::State, Option<u64>)> + Send + Sync + 'static),
    load_world_fn: &'a mut (dyn System<In = T::State, Out = ()> + Send + Sync + 'static),
    data: std::marker::PhantomData<T>,
}

impl<'a, T: BackrollConfig> SessionCallbacks<T> for BackrollStageCallbacks<'a, T> {
    fn save_state(&mut self) -> (T::State, Option<u64>) {
        self.save_world_fn.run((), self.world)
    }

    fn load_state(&mut self, state: T::State) {
        self.load_world_fn.run(state, self.world);
    }

    fn advance_frame(&mut self, input: GameInput<T::Input>) {
        // Insert input via Resource
        *self
            .world
            .get_resource_mut::<GameInput<T::Input>>()
            .unwrap() = input;
        self.schedule.run_once(self.world);
    }

    fn handle_event(&mut self, _: BackrollEvent) {
        // TODO(james7132): Figure out how this will work.
    }
}

pub struct BackrollStage<T>
where
    T: BackrollConfig,
{
    schedule: Schedule,
    run_criteria: Option<Box<dyn System<In = (), Out = ShouldRun>>>,
    run_criteria_initialized: bool,
    input_sample_fn:
        Box<dyn System<In = BackrollPlayerHandle, Out = T::Input> + Send + Sync + 'static>,
    save_world_fn: Box<dyn System<In = (), Out = (T::State, Option<u64>)> + Send + Sync + 'static>,
    load_world_fn: Box<dyn System<In = T::State, Out = ()> + Send + Sync + 'static>,
}

impl<T: BackrollConfig> Stage for BackrollStage<T> {
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

            for player_handle in session.local_players() {
                let input = self.input_sample_fn.run(player_handle, world);
                session
                    .add_local_input(player_handle, input)
                    .expect("Adding local input for local players shouldn't fail.")
            }

            world.insert_resource(GameInput::<T::Input>::default());
            {
                let mut callbacks = BackrollStageCallbacks::<T> {
                    world,
                    schedule: &mut self.schedule,
                    save_world_fn: self.save_world_fn.as_mut(),
                    load_world_fn: self.load_world_fn.as_mut(),
                    data: Default::default(),
                };
                session.advance_frame(&mut callbacks);
            }
            world.remove_resource::<GameInput<T::Input>>();

            if let ShouldRun::Yes = should_run {
                return;
            }
        }
    }
}
