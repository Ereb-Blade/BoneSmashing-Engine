use glam::Quat;

use crate::scene::entity::RenderObject;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectId(pub usize);

pub struct World {
    objects: Vec<RenderObject>,
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
            rotating_cube_id: None,
        }
    }

    /// Чистый метод добавления объекта без привязки к анимациям
    pub fn add_object(&mut self, object: RenderObject) -> ObjectId {
        let id = ObjectId(self.objects.len());
        self.objects.push(object);
        id
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
}
