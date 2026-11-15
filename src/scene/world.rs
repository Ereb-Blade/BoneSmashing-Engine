use glam::{Quat, Vec3};

use crate::scene::Transform;
use crate::scene::entity::RenderObject;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelGroupId(pub usize);

#[derive(Clone, Debug)]
pub struct ModelGroup {
    pub name: String,
    pub transform: Transform,
    pub source_path: PathBuf,
    pub animation_clips: Vec<ModelAnimationClip>,
    pub animation: Option<ModelAnimationPlayback>,
    pub(crate) objects: Vec<ObjectId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimationClip {
    pub name: String,
    pub duration: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelAnimationPlayback {
    pub clip_index: usize,
    pub time: f32,
    pub speed: f32,
    pub looping: bool,
    pub playing: bool,
}

impl Default for ModelAnimationPlayback {
    fn default() -> Self {
        Self {
            clip_index: 0,
            time: 0.0,
            speed: 1.0,
            looping: true,
            playing: false,
        }
    }
}

#[derive(Clone)]
pub struct World {
    objects: Vec<RenderObject>,
    object_ids: Vec<ObjectId>,
    object_indices: HashMap<ObjectId, usize>,
    next_object_id: usize,
    standalone_objects: Vec<ObjectId>,
    model_groups: Vec<ModelGroup>,
    model_group_ids: Vec<ModelGroupId>,
    model_group_indices: HashMap<ModelGroupId, usize>,
    next_model_group_id: usize,
    rotating_cube_id: Option<ObjectId>,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
            object_ids: Vec::new(),
            object_indices: HashMap::new(),
            next_object_id: 0,
            standalone_objects: Vec::new(),
            model_groups: Vec::new(),
            model_group_ids: Vec::new(),
            model_group_indices: HashMap::new(),
            next_model_group_id: 0,
            rotating_cube_id: None,
        }
    }

    /// Чистый метод добавления объекта без привязки к анимациям
    pub fn add_object(&mut self, object: RenderObject) -> ObjectId {
        let id = self.push_object(object);
        self.standalone_objects.push(id);
        id
    }

    fn push_object(&mut self, object: RenderObject) -> ObjectId {
        let id = ObjectId(self.next_object_id);
        self.next_object_id = self
            .next_object_id
            .checked_add(1)
            .expect("ObjectId space exhausted");
        self.object_indices.insert(id, self.objects.len());
        self.object_ids.push(id);
        self.objects.push(object);
        id
    }

    pub fn add_model_group(
        &mut self,
        name: impl Into<String>,
        source_path: PathBuf,
        transform: Transform,
    ) -> ModelGroupId {
        let id = ModelGroupId(self.next_model_group_id);
        self.next_model_group_id = self
            .next_model_group_id
            .checked_add(1)
            .expect("ModelGroupId space exhausted");
        self.model_group_indices.insert(id, self.model_groups.len());
        self.model_group_ids.push(id);
        self.model_groups.push(ModelGroup {
            name: name.into(),
            transform,
            source_path,
            animation_clips: Vec::new(),
            animation: None,
            objects: Vec::new(),
        });
        id
    }

    pub fn add_group_object(
        &mut self,
        group_id: ModelGroupId,
        mut object: RenderObject,
    ) -> Option<ObjectId> {
        let group_index = *self.model_group_indices.get(&group_id)?;
        if self.model_groups.get(group_index).is_none() {
            return None;
        }
        object.set_model_group(Some(group_id));
        let object_id = self.push_object(object);
        self.model_groups[group_index].objects.push(object_id);
        Some(object_id)
    }

    pub fn model_groups(&self) -> &[ModelGroup] {
        &self.model_groups
    }

    pub fn model_group_ids(&self) -> &[ModelGroupId] {
        &self.model_group_ids
    }

    pub fn object_id_at(&self, index: usize) -> Option<ObjectId> {
        self.object_ids.get(index).copied()
    }

    pub fn standalone_objects(&self) -> &[ObjectId] {
        &self.standalone_objects
    }

    pub fn model_group_mut(&mut self, id: ModelGroupId) -> Option<&mut ModelGroup> {
        let index = *self.model_group_indices.get(&id)?;
        self.model_groups.get_mut(index)
    }

    pub fn model_group(&self, id: ModelGroupId) -> Option<&ModelGroup> {
        let index = *self.model_group_indices.get(&id)?;
        self.model_groups.get(index)
    }

    pub fn group_model_matrix(&self, object: &RenderObject) -> glam::Mat4 {
        object.model_group().map_or_else(
            || object.model_matrix(),
            |id| {
                self.model_group(id).map_or_else(
                    || object.model_matrix(),
                    |group| group.transform.to_matrix() * object.model_matrix(),
                )
            },
        )
    }

    /// Явно назначаем, какой объект должен вращаться
    pub fn set_rotating_object(&mut self, id: ObjectId) {
        self.rotating_cube_id = Some(id);
    }

    pub fn get_object(&self, id: ObjectId) -> Option<&RenderObject> {
        let index = *self.object_indices.get(&id)?;
        self.objects.get(index)
    }

    pub fn get_object_mut(&mut self, id: ObjectId) -> Option<&mut RenderObject> {
        let index = *self.object_indices.get(&id)?;
        self.objects.get_mut(index)
    }

    pub fn remove_object(&mut self, id: ObjectId) -> Option<RenderObject> {
        let object = self.remove_object_storage(id)?;
        self.standalone_objects.retain(|&object_id| object_id != id);

        if let Some(group_id) = object.model_group()
            && self
                .model_group(group_id)
                .is_some_and(|group| group.objects.is_empty())
        {
            self.remove_model_group_storage(group_id);
        }

        Some(object)
    }

    fn remove_object_storage(&mut self, id: ObjectId) -> Option<RenderObject> {
        let index = self.object_indices.remove(&id)?;
        let removed = self.objects.swap_remove(index);
        self.object_ids.swap_remove(index);
        if let Some(&moved_id) = self.object_ids.get(index) {
            self.object_indices.insert(moved_id, index);
        }
        self.standalone_objects.retain(|&object_id| object_id != id);
        if self.rotating_cube_id == Some(id) {
            self.rotating_cube_id = None;
        }
        if let Some(group_id) = removed.model_group()
            && let Some(group) = self.model_group_mut(group_id)
        {
            group.objects.retain(|&object_id| object_id != id);
        }
        Some(removed)
    }

    pub fn remove_model_group(&mut self, id: ModelGroupId) -> Option<ModelGroup> {
        let group = self.model_group(id)?.clone();
        for object_id in &group.objects {
            self.remove_object_storage(*object_id);
        }
        self.remove_model_group_storage(id)
    }

    fn remove_model_group_storage(&mut self, id: ModelGroupId) -> Option<ModelGroup> {
        let index = self.model_group_indices.remove(&id)?;
        let group = self.model_groups.swap_remove(index);
        self.model_group_ids.swap_remove(index);
        if let Some(&moved_id) = self.model_group_ids.get(index) {
            self.model_group_indices.insert(moved_id, index);
        }
        Some(group)
    }

    pub fn duplicate_object(&mut self, id: ObjectId) -> Option<ObjectId> {
        let mut object = self.get_object(id)?.clone();
        object.transform.position += Vec3::X;
        object.name = if object.name.is_empty() {
            "Object Copy".to_owned()
        } else {
            format!("{} Copy", object.name)
        };
        match object.model_group() {
            Some(group_id) => self.add_group_object(group_id, object),
            None => Some(self.add_object(object)),
        }
    }

    pub fn duplicate_model_group(&mut self, id: ModelGroupId) -> Option<ModelGroupId> {
        let source = self.model_group(id)?.clone();
        let objects: Vec<_> = source
            .objects
            .iter()
            .filter_map(|&object_id| self.get_object(object_id).cloned())
            .collect();
        let mut transform = source.transform;
        transform.position += Vec3::X;
        let copy_id = self.add_model_group(
            format!("{} Copy", source.name),
            source.source_path,
            transform,
        );
        if let Some(copy) = self.model_group_mut(copy_id) {
            copy.animation_clips = source.animation_clips;
            copy.animation = source.animation;
        }
        for object in objects {
            self.add_group_object(copy_id, object)?;
        }
        Some(copy_id)
    }

    pub(crate) fn id_sequences(&self) -> (usize, usize) {
        (self.next_object_id, self.next_model_group_id)
    }

    pub(crate) fn preserve_id_sequences(&mut self, sequences: (usize, usize)) {
        self.next_object_id = self.next_object_id.max(sequences.0);
        self.next_model_group_id = self.next_model_group_id.max(sequences.1);
    }

    pub fn update(&mut self, dt: f32) {
        if let Some(cube_id) = self.rotating_cube_id {
            if let Some(cube) = self.get_object_mut(cube_id) {
                let rotation_delta = Quat::from_rotation_y(1.0 * dt);
                cube.transform.rotation = rotation_delta * cube.transform.rotation;
            }
        }
    }

    pub fn objects(&self) -> &[RenderObject] {
        &self.objects
    }

    pub(crate) fn objects_mut(&mut self) -> &mut [RenderObject] {
        &mut self.objects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{MaterialId, MeshId};
    use crate::scene::Transform;
    use glam::Vec3;

    #[test]
    fn test_add_and_get_object() {
        let mut world = World::new();
        let custom_obj = RenderObject::new(
            Transform::from_translation(Vec3::new(5.0, 5.0, 5.0)),
            MeshId(0),
            MaterialId(0),
        );

        let id = world.add_object(custom_obj);
        assert_eq!(world.objects().len(), 1);

        let retrieved = world
            .get_object_mut(id)
            .expect("Объект должен существовать");
        assert_eq!(retrieved.transform.position, Vec3::new(5.0, 5.0, 5.0));
    }

    #[test]
    fn test_world_update_animation() {
        let mut world = World::new();
        let obj = RenderObject::new(Transform::default(), MeshId(0), MaterialId(0));
        let cube_id = world.add_object(obj);

        world.set_rotating_object(cube_id);

        let initial_rotation = world.get_object_mut(cube_id).unwrap().transform.rotation;
        world.update(0.5);
        let updated_rotation = world.get_object_mut(cube_id).unwrap().transform.rotation;

        assert_ne!(initial_rotation, updated_rotation);
    }

    #[test]
    fn model_group_transform_is_applied_before_part_transform() {
        let mut world = World::new();
        let group = world.add_model_group(
            "Vehicle",
            PathBuf::from("vehicle.glb"),
            Transform::from_translation(Vec3::new(10.0, 0.0, 0.0)),
        );
        let object = RenderObject::new(
            Transform::from_translation(Vec3::new(0.0, 2.0, 0.0)),
            MeshId(0),
            MaterialId(0),
        );
        let id = world.add_group_object(group, object).unwrap();
        let object = world.get_object(id).unwrap();
        let world_position = world
            .group_model_matrix(object)
            .transform_point3(Vec3::ZERO);

        assert_eq!(world_position, Vec3::new(10.0, 2.0, 0.0));
    }

    #[test]
    fn removed_object_ids_do_not_alias_objects_added_later() {
        let mut world = World::new();
        let first = world.add_object(RenderObject::new(
            Transform::from_translation(Vec3::X),
            MeshId(0),
            MaterialId(0),
        ));
        let second = world.add_object(RenderObject::new(
            Transform::from_translation(Vec3::Y),
            MeshId(0),
            MaterialId(0),
        ));

        world.remove_object(first).unwrap();
        let third = world.add_object(RenderObject::new(
            Transform::from_translation(Vec3::Z),
            MeshId(0),
            MaterialId(0),
        ));

        assert!(world.get_object(first).is_none());
        assert_eq!(
            world.get_object(second).unwrap().transform.position,
            Vec3::Y
        );
        assert_eq!(world.get_object(third).unwrap().transform.position, Vec3::Z);
    }

    #[test]
    fn duplicate_standalone_object_is_offset_and_keeps_source_intact() {
        let mut world = World::new();
        let source = world.add_object(RenderObject::new(
            Transform::from_translation(Vec3::Y),
            MeshId(0),
            MaterialId(0),
        ));

        let copy = world.duplicate_object(source).unwrap();

        assert_ne!(copy, source);
        assert_eq!(
            world.get_object(source).unwrap().transform.position,
            Vec3::Y
        );
        assert_eq!(
            world.get_object(copy).unwrap().transform.position,
            Vec3::Y + Vec3::X
        );
    }

    #[test]
    fn removing_model_group_removes_its_parts_without_invalidating_other_ids() {
        let mut world = World::new();
        let first_group =
            world.add_model_group("First", PathBuf::from("first.glb"), Transform::default());
        let first_part = world
            .add_group_object(
                first_group,
                RenderObject::new(Transform::default(), MeshId(0), MaterialId(0)),
            )
            .unwrap();
        let second_group =
            world.add_model_group("Second", PathBuf::from("second.glb"), Transform::default());
        let second_part = world
            .add_group_object(
                second_group,
                RenderObject::new(Transform::default(), MeshId(0), MaterialId(0)),
            )
            .unwrap();

        world.remove_model_group(first_group).unwrap();

        assert!(world.get_object(first_part).is_none());
        assert!(world.model_group(first_group).is_none());
        assert!(world.get_object(second_part).is_some());
        assert!(world.model_group(second_group).is_some());
        assert_eq!(world.model_group_ids(), &[second_group]);
    }

    #[test]
    fn duplicate_group_creates_new_stable_ids_and_copies_children() {
        let mut world = World::new();
        let group = world.add_model_group(
            "Vehicle",
            PathBuf::from("vehicle.glb"),
            Transform::default(),
        );
        let part = world
            .add_group_object(
                group,
                RenderObject::new(Transform::default(), MeshId(1), MaterialId(2)).with_name("Body"),
            )
            .unwrap();

        let copy = world.duplicate_model_group(group).unwrap();
        let copy_part = world.model_group(copy).unwrap().objects[0];

        assert_ne!(copy, group);
        assert_ne!(copy_part, part);
        assert_eq!(world.get_object(copy_part).unwrap().name, "Body");
        assert_eq!(
            world.get_object(copy_part).unwrap().model_group(),
            Some(copy)
        );
        assert_eq!(world.model_group(copy).unwrap().transform.position, Vec3::X);
    }
}
