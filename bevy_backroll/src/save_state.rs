use bevy_ecs::prelude::*;
use parking_lot::Mutex;
use std::any::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
struct SavedComponents<T: Clone> {
    components: HashMap<Entity, T>,
}

/// A mutable builder for [`SaveState`]s.
pub(crate) struct SaveStateBuilder {
    state: Mutex<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

impl SaveStateBuilder {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn build(self) -> SaveState {
        SaveState(Arc::new(self.state.into_inner()))
    }
}

/// A read only save state of a Bevy world.
#[derive(Clone)]
pub struct SaveState(Arc<HashMap<TypeId, Box<dyn Any + Send + Sync>>>);

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
    let saved = save_state.0.get(&TypeId::of::<T>());
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

pub(crate) fn save_components<T: Component + Clone>(
    save_state: Res<SaveStateBuilder>,
    query: Query<(Entity, &T)>,
) {
    let components: HashMap<Entity, T> = query
        .iter()
        .map(|(entity, component)| (entity, component.clone()))
        .collect();
    if !components.is_empty() {
        save_state.state.lock().insert(
            TypeId::of::<SavedComponents<T>>(),
            Box::new(SavedComponents { components }),
        );
    }
}

pub(crate) fn load_components<T: Component + Clone>(
    save_state: Res<SaveState>,
    mut query: Query<(Entity, &mut T)>,
    mut commands: Commands,
) {
    let saved = save_state.0.get(&TypeId::of::<SavedComponents<T>>());
    let slab = if let Some(slab) = saved {
        slab.downcast_ref::<SavedComponents<T>>().unwrap()
    } else {
        for (entity, _) in query.iter() {
            commands.entity(entity).remove::<T>();
        }
        return;
    };

    let mut components = slab.components.clone();
    // HACK: This is REALLY going to screw with any change detection on these types.
    for (entity, mut comp) in query.iter_mut() {
        if let Some(component) = components.remove(&entity) {
            *comp = component;
        } else {
            commands.entity(entity).remove::<T>();
        }
    }

    // Everything else that remains was either removed or had it's entity despawned.
    for (entity, component) in components {
        commands.entity(entity).insert(component);
    }
}
