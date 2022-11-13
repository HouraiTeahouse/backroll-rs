use bevy_app::{App, Plugin};
use bevy_ecs::prelude::*;
use bevy_steamworks::Client;
use std::ops::Deref;

#[derive(Resource)]
pub struct SteamP2PManager(backroll_transport_steam::SteamP2PManager);

fn initialize_steam_socket(client: Option<Res<Client>>, mut commands: Commands) {
    if let Some(client) = client {
        let client = client.deref().deref();
        commands.insert_resource(SteamP2PManager(
            backroll_transport_steam::SteamP2PManager::bind(client.clone()),
        ));
    }
}

pub struct BackrollSteamPlugin;

impl Plugin for BackrollSteamPlugin {
    fn build(&self, builder: &mut App) {
        builder.add_startup_system(initialize_steam_socket);
    }
}
