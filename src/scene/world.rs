use glam::Quat;

use crate::scene::Transform;
use crate::scene::entity::RenderObject;
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
    pub(crate) objects: Vec<ObjectId>,
}

pub struct World {
    objects: Vec<RenderObject>,
    standalone_objects: Vec<ObjectId>,
    model_groups: Vec<ModelGroup>,
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
            standalone_objects: Vec::new(),
            model_groups: Vec::new(),
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
        let id = ObjectId(self.objects.len());
        self.objects.push(object);
        id
    }

    pub fn add_model_group(
        &mut self,
        name: impl Into<String>,
        source_path: PathBuf,
        transform: Transform,
    ) -> ModelGroupId {
        let id = ModelGroupId(self.model_groups.len());
        self.model_groups.push(ModelGroup {
            name: name.into(),
            transform,
            source_path,
            objects: Vec::new(),
        });
        id
    }

    pub fn add_group_object(
        &mut self,
        group_id: ModelGroupId,
        mut object: RenderObject,
    ) -> Option<ObjectId> {
        if self.model_groups.get(group_id.0).is_none() {
            return None;
        }
        object.set_model_group(Some(group_id));
        let object_id = self.push_object(object);
        self.model_groups[group_id.0].objects.push(object_id);
        Some(object_id)
    }

    pub fn model_groups(&self) -> &[ModelGroup] {
        &self.model_groups
    }

    pub fn standalone_objects(&self) -> &[ObjectId] {
        &self.standalone_objects
    }

    pub fn model_group_mut(&mut self, id: ModelGroupId) -> Option<&mut ModelGroup> {
        self.model_groups.get_mut(id.0)
    }

    pub fn model_group(&self, id: ModelGroupId) -> Option<&ModelGroup> {
        self.model_groups.get(id.0)
    }

    pub fn group_model_matrix(&self, object: &RenderObject) -> glam::Mat4 {
        object.model_group().map_or_else(
            || object.model_matrix(),
            |id| {
                self.model_groups.get(id.0).map_or_else(
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

    pub fn get_object_mut(&mut self, id: ObjectId) -> Option<&mut RenderObject> {
        self.objects.get_mut(id.0)
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
        let object = &world.objects()[id.0];
        let world_position = world
            .group_model_matrix(object)
            .transform_point3(Vec3::ZERO);

        assert_eq!(world_position, Vec3::new(10.0, 2.0, 0.0));
    }
}
