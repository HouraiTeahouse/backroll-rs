use bevy_steamworks::{Client, ClientManager, SteamworksPlugin};
use bevy_app::{AppBuilder, Plugin};
use bevy_tasks::IoTaskPool;
use bevy_ecs::prelude::*;
use backroll_transport_steam::SteamP2PManager;
use std::ops::Deref;

fn initialize_steam_socket(
    client: Option<Res<Client<ClientManager>>>,
    task_pool: Res<IoTaskPool>,
    mut commands: Commands
) {
    if let Some(client) = client {
        commands.insert_resource(
            SteamP2PManager::bind(
                task_pool.deref().deref().clone(), 
                client.clone()
            )
        );
    }
}

pub struct BackrollSteamPlugin;

impl Plugin for BackrollSteamPlugin {
    fn build(&self, builder: &mut AppBuilder)  {
        builder
            .add_plugin(SteamworksPlugin)
            .add_startup_system(initialize_steam_socket.system());
    }
}