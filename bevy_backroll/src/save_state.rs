use bevy_ecs::prelude::*;
use bevy_log::*;
use std::any::*;
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::Mutex;

#[derive(Clone)]
struct ComponentSlab<T: Clone> {
    components: Vec<T>,
    entities: Vec<Entity>,
}

/// A mutable builder for [`SaveState`]s.
pub struct SaveStateBuilder {
    state: Mutex<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

impl SaveStateBuilder {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn save_resource<T: Clone + Send + Sync + 'static>(&self, resource: Res<T>) {
        self.state
	.lock()
            .insert(TypeId::of::<T>(), Box::new(resource.clone()));
    }

    pub(crate) fn save_components<T: Component + Clone>(&self, query: Query<(Entity, &T)>) {
	let iter = query.iter();
	let (_, max) = iter.size_hint();
        let (mut components, mut entities) = if let Some(max) = max {
		(Vec::with_capacity(max), Vec::with_capacity(max))
	} else {
		(Vec::new(), Vec::new())
	};
        for (entity, component) in iter {
            entities.push(entity);
            components.push(component.clone());
        }
        self.state.lock().insert(
            TypeId::of::<ComponentSlab<T>>(),
            Box::new(ComponentSlab {
                components,
                entities,
            }),
        );
    }

    pub fn build(self) -> SaveState {
        SaveState(Arc::new(self.state.into_inner()))
    }
}

/// A read only save state of a Bevy world.
#[derive(Clone)]
pub struct SaveState(Arc<HashMap<TypeId, Box<dyn Any + Send + Sync>>>);

impl SaveState {
    pub(crate) fn load_resource<T: Clone + Send + Sync + 'static>(&self, mut resource: ResMut<T>) {
        // HACK: This is REALLY going to screw with any change detection on these types.
        let saved = self
            .0
            .get(&TypeId::of::<T>())
            .unwrap()
            .downcast_ref::<T>()
            .unwrap();
        *resource = saved.clone();
    }

    pub(crate) fn load_components<T: Component + Clone>(&self, mut query: Query<&mut T>) {
        let slab = self
            .0
            .get(&TypeId::of::<ComponentSlab<T>>())
            .unwrap()
            .downcast_ref::<ComponentSlab<T>>()
            .unwrap();

        // HACK: This is REALLY going to screw with any change detection on these types.
        for (entity, component) in slab.entities.iter().zip(slab.components.iter()) {
            if let Ok(mut comp) = query.get_mut(*entity) {
                *comp = component.clone();
            } else {
                warn!(
                    "Attempted to load component '{}' from a save state for entity {:?} but the entity no longer exists.",
                    type_name::<T>(),
                    entity
                )
            }
        }
    }
}

pub(crate) fn save_resource<T: Clone + Send + Sync + 'static>(
    save_state: Res<SaveStateBuilder>,
    resource: Res<T>,
) {
    save_state.save_resource(resource);
}

pub(crate) fn load_resource<T: Clone + Send + Sync + 'static>(
    save_state: Res<SaveState>,
    resource: ResMut<T>,
) {
    save_state.load_resource(resource);
}

// This disallows parallelization while saving. Is this design OK?
pub(crate) fn save_components<T: Component + Clone>(
    save_state: Res<SaveStateBuilder>,
    query: Query<(Entity, &T)>,
) {
    save_state.save_components(query);
}

pub(crate) fn load_components<T: Component + Clone>(
    save_state: Res<SaveState>,
    query: Query<&mut T>,
) {
    save_state.load_components(query);
}
