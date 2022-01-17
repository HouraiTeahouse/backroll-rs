use crate::NetworkId;
use bevy_ecs::prelude::*;
use parking_lot::Mutex;
use std::any::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone)]
struct SavedComponents<T: Clone> {
    components: HashMap<NetworkId, T>,
}

/// A mutable builder for [`SaveState`]s.
pub(crate) struct SaveStateBuilder {
    ids: HashSet<NetworkId>,
    state: Mutex<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

impl SaveStateBuilder {
    pub fn new() -> Self {
        Self {
            ids: HashSet::new(),
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn build(self) -> SaveState {
        SaveState(Arc::new(SaveStateRef {
            ids: self.ids,
            state: self.state.into_inner(),
        }))
    }
}

struct SaveStateRef {
    ids: HashSet<NetworkId>,
    state: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

/// A read only save state of a Bevy world.
#[derive(Clone)]
pub struct SaveState(Arc<SaveStateRef>);

pub(crate) fn save_resource<T: Clone + Send + Sync + 'static>(
    save_state: Res<SaveStateBuilder>,
    resource: Option<Res<T>>,
) {
    if let Some(resource) = resource {
        save_state
            .state
            .lock()
            .insert(TypeId::of::<T>(), Box::new(resource.clone()));
    }
}

pub(crate) fn load_resource<T: Clone + Send + Sync + 'static>(
    save_state: Res<SaveState>,
    resource: Option<ResMut<T>>,
    mut commands: Commands,
) {
    // HACK: This is REALLY going to screw with any change detection on these types.
    let saved = save_state.0.state.get(&TypeId::of::<T>());
    match (saved, resource) {
        (Some(saved), Some(mut resource)) => {
            let saved = saved.downcast_ref::<T>().unwrap();
            *resource = saved.clone();
        }
        (Some(saved), None) => {
            let saved = saved.downcast_ref::<T>().unwrap();
            commands.insert_resource(saved.clone());
        }
        (None, Some(_)) => {
            commands.remove_resource::<T>();
        }
        (None, None) => {}
    }
}

pub(crate) fn save_network_ids(mut save_state: ResMut<SaveStateBuilder>, query: Query<&NetworkId>) {
    save_state.ids = query.iter().cloned().collect();
}

pub(crate) fn save_components<T: Component + Clone>(
    save_state: Res<SaveStateBuilder>,
    query: Query<(&NetworkId, &T)>,
) {
    let components: HashMap<NetworkId, T> = query
        .iter()
        .map(|(id, component)| (*id, component.clone()))
        .collect();
    if !components.is_empty() {
        save_state.state.lock().insert(
            TypeId::of::<SavedComponents<T>>(),
            Box::new(SavedComponents { components }),
        );
    }
}

pub(crate) fn sync_network_ids(
    save_state: Res<SaveState>,
    query: Query<(Entity, &NetworkId)>,
    mut commands: Commands,
) {
    // Despawn all network identities that shouldn't exist this frame.
    let mut ids = save_state.0.ids.clone();
    for (entity, network_id) in query.iter() {
        if !ids.remove(network_id) {
            commands.entity(entity).despawn();
        }
    }

    // All IDs that remain need to re-spawned.
    for network_id in ids {
        commands.spawn_bundle((network_id,));
    }
}

pub(crate) fn load_components<T: Component + Clone>(
    save_state: Res<SaveState>,
    mut query: Query<(Entity, &NetworkId, Option<&mut T>)>,
    mut commands: Commands,
) {
    let saved = save_state.0.state.get(&TypeId::of::<SavedComponents<T>>());
    let slab = if let Some(slab) = saved {
        slab.downcast_ref::<SavedComponents<T>>().unwrap()
    } else {
        for (entity, _, _) in query.iter() {
            commands.entity(entity).remove::<T>();
        }
        return;
    };

    let mut components = slab.components.clone();
    // HACK: This is REALLY going to screw with any change detection on these types.
    for (entity, network_id, comp) in query.iter_mut() {
        match (components.remove(&network_id), comp) {
            (Some(saved), Some(mut comp)) => {
                *comp = saved;
            }
            (Some(saved), None) => {
                commands.entity(entity).insert(saved);
            }
            (None, Some(_)) => {
                commands.entity(entity).remove::<T>();
            }
            (None, None) => {}
        }
    }
}
