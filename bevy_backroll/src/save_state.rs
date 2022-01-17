use bevy_ecs::prelude::*;
use bevy_log::*;
use std::any::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
struct ComponentSlab<T: Clone> {
    components: Vec<T>,
    entities: Vec<Entity>,
}

/// A mutable builder for [`SaveState`]s.
pub struct SaveStateBuilder {
    components: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl SaveStateBuilder {
    pub fn new() -> Self {
        Self {
            components: HashMap::new(),
        }
    }

    pub(crate) fn save_components<T: Component + Clone>(&mut self, query: Query<(Entity, &T)>) {
        let mut components = Vec::new();
        let mut entities = Vec::new();
        for (entity, component) in query.iter() {
            entities.push(entity);
            components.push(component.clone());
        }
        self.components.insert(
            TypeId::of::<T>(),
            Box::new(ComponentSlab {
                components,
                entities,
            }),
        );
    }

    pub fn build(self) -> SaveState {
        SaveState(Arc::new(self))
    }
}

/// A read only save state of a Bevy world.
#[derive(Clone)]
pub struct SaveState(Arc<SaveStateBuilder>);

impl SaveState {
    pub(crate) fn load_components<T: Component + Clone>(&self, mut query: Query<&mut T>) {
        let slab = self
            .0
            .components
            .get(&TypeId::of::<T>())
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

// This disallows parallelization while saving. Is this design OK?
pub(crate) fn save_components<T: Component + Clone>(
    mut save_state: ResMut<SaveStateBuilder>,
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
