use backroll::{Command, Commands, Config, GameInput, P2PSession, PlayerHandle};
use bevy_ecs::{
    schedule::{Schedule, ShouldRun, Stage},
    system::System,
    world::World,
};
use tracing::error;

pub const BACKROLL_UPDATE: &str = "backroll_update";

pub struct BackrollStage<T>
where
    T: Config,
{
    schedule: Schedule,
    run_criteria: Option<Box<dyn System<In = (), Out = ShouldRun>>>,
    run_criteria_initialized: bool,
    input_sample_fn: Box<dyn System<In = PlayerHandle, Out = T::Input> + Send + Sync + 'static>,
    save_world_fn: Box<dyn System<In = (), Out = (T::State, Option<u64>)> + Send + Sync + 'static>,
    load_world_fn: Box<dyn System<In = T::State, Out = ()> + Send + Sync + 'static>,
}

impl<T: Config> BackrollStage<T> {
    fn run_commands(&mut self, commands: Commands<T>, world: &mut World) {
        for command in commands {
            match command {
                Command::<T>::Save(save_state) => {
                    let (state, checksum) = self.save_world_fn.run((), world);
                    save_state.save(state, checksum);
                }
                Command::<T>::Load(load_state) => {
                    self.load_world_fn.run(load_state.load(), world);
                }
                Command::AdvanceFrame(inputs) => {
                    // Insert input via Resource
                    *world.get_resource_mut::<GameInput<T::Input>>().unwrap() = inputs;
                    self.schedule.run_once(world);
                }
                Command::Event(_) => {
                    // TODO(james7132): Figure out how this will work.
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

            world.insert_resource(GameInput::<T::Input>::default());
            self.run_commands(session.advance_frame(), world);
            world.remove_resource::<GameInput<T::Input>>();

            if let ShouldRun::Yes = should_run {
                return;
            }
        }
    }
}
