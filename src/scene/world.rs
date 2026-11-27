use glam::{Quat, Vec3};

use crate::scene::Transform;
use crate::scene::entity::RenderObject;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelGroupId(pub usize);

pub const MAX_LOCAL_LIGHTS: usize = 16;
pub const MAX_LOCAL_SHADOW_SPOTLIGHTS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalLightKind {
    Point,
    Spot,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalLight {
    pub name: String,
    pub kind: LocalLightKind,
    pub enabled: bool,
    pub casts_shadows: bool,
    pub position: [f32; 3],
    pub direction: [f32; 3],
    pub color: [f32; 3],
    pub intensity: f32,
    pub range: f32,
    pub inner_cone_degrees: f32,
    pub outer_cone_degrees: f32,
    #[serde(skip)]
    parent: Option<ModelGroupId>,
}

impl LocalLight {
    pub fn point(name: impl Into<String>, position: [f32; 3]) -> Self {
        Self {
            name: name.into(),
            kind: LocalLightKind::Point,
            enabled: true,
            casts_shadows: false,
            position,
            direction: [0.0, -1.0, 0.0],
            color: [1.0, 0.85, 0.65],
            intensity: 12.0,
            range: 8.0,
            inner_cone_degrees: 20.0,
            outer_cone_degrees: 30.0,
            parent: None,
        }
    }

    pub fn spot(name: impl Into<String>, position: [f32; 3], direction: [f32; 3]) -> Self {
        Self {
            kind: LocalLightKind::Spot,
            direction,
            ..Self::point(name, position)
        }
    }

    pub fn is_valid(&self) -> bool {
        let position = Vec3::from_array(self.position);
        let direction = Vec3::from_array(self.direction);
        self.name
            .chars()
            .any(|character| !character.is_whitespace())
            && position.is_finite()
            && direction.is_finite()
            && direction.length_squared() > f32::EPSILON
            && self
                .color
                .iter()
                .all(|component| component.is_finite() && (0.0..=1.0).contains(component))
            && (!self.casts_shadows
                || (self.kind == LocalLightKind::Spot && self.outer_cone_degrees > 0.0))
            && self.intensity.is_finite()
            && (0.0..=1000.0).contains(&self.intensity)
            && self.range.is_finite()
            && (0.1..=1000.0).contains(&self.range)
            && self.inner_cone_degrees.is_finite()
            && self.outer_cone_degrees.is_finite()
            && (0.0..=89.0).contains(&self.inner_cone_degrees)
            && (self.inner_cone_degrees..=89.0).contains(&self.outer_cone_degrees)
    }
}

#[derive(Clone, Debug)]
pub struct ModelGroup {
    pub name: String,
    pub transform: Transform,
    pub source_path: Option<PathBuf>,
    parent: Option<ModelGroupId>,
    gltf_node_index: Option<usize>,
    pub animation_clips: Vec<ModelAnimationClip>,
    pub animation: Option<ModelAnimationPlayback>,
    pub(crate) objects: Vec<ObjectId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimationClip {
    pub name: String,
    pub duration: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimationKeyboardControl {
    pub enabled: bool,
    pub idle_clip_index: usize,
    pub walk_clip_index: usize,
    pub run_clip_index: usize,
}

impl AnimationKeyboardControl {
    pub fn for_clips(clips: &[ModelAnimationClip]) -> Self {
        let find_named = |names: &[&str]| {
            clips.iter().position(|clip| {
                names
                    .iter()
                    .any(|name| clip.name.eq_ignore_ascii_case(name))
            })
        };
        let idle_clip_index = find_named(&["Survey", "Idle", "Stand", "Standing"]).unwrap_or(0);
        let walk_fallback = (0..clips.len())
            .find(|index| *index != idle_clip_index)
            .unwrap_or(idle_clip_index);
        let walk_clip_index = find_named(&["Walk", "Walking"]).unwrap_or(walk_fallback);
        let run_fallback = (0..clips.len())
            .find(|index| *index != idle_clip_index && *index != walk_clip_index)
            .unwrap_or(walk_clip_index);
        let run_clip_index = find_named(&["Run", "Running"]).unwrap_or(run_fallback);
        Self {
            enabled: false,
            idle_clip_index,
            walk_clip_index,
            run_clip_index,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelAnimationTransition {
    pub from_clip_index: usize,
    pub from_time: f32,
    pub elapsed: f32,
    pub duration: f32,
}

impl ModelAnimationTransition {
    pub fn progress(self) -> f32 {
        if self.duration > 0.0 {
            (self.elapsed / self.duration).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }
}

pub const DEFAULT_ANIMATION_CROSSFADE_SECONDS: f32 = 0.35;
pub const MAX_ANIMATION_CROSSFADE_SECONDS: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelAnimationPlayback {
    pub clip_index: usize,
    pub time: f32,
    pub speed: f32,
    pub looping: bool,
    pub playing: bool,
    pub crossfade_duration: f32,
    pub keyboard_control: AnimationKeyboardControl,
    pub transition: Option<ModelAnimationTransition>,
}

impl Default for ModelAnimationPlayback {
    fn default() -> Self {
        Self {
            clip_index: 0,
            time: 0.0,
            speed: 1.0,
            looping: true,
            playing: false,
            crossfade_duration: DEFAULT_ANIMATION_CROSSFADE_SECONDS,
            keyboard_control: AnimationKeyboardControl::default(),
            transition: None,
        }
    }
}

impl ModelAnimationPlayback {
    pub fn switch_to_clip(&mut self, clip_index: usize) -> bool {
        if self.clip_index == clip_index || self.transition.is_some() {
            return false;
        }
        let from_clip_index = self.clip_index;
        let from_time = self.time;
        self.clip_index = clip_index;
        self.time = 0.0;
        self.transition = (self.crossfade_duration > 0.0).then_some(ModelAnimationTransition {
            from_clip_index,
            from_time,
            elapsed: 0.0,
            duration: self.crossfade_duration,
        });
        true
    }
}

#[derive(Clone)]
pub struct World {
    local_lights: Vec<LocalLight>,
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
    cached_local_group_matrices: RefCell<HashMap<ModelGroupId, glam::Mat4>>,
    cached_world_group_matrices: RefCell<HashMap<ModelGroupId, glam::Mat4>>,
    cached_local_object_matrices: RefCell<HashMap<ObjectId, glam::Mat4>>,
    cached_world_object_matrices: RefCell<HashMap<ObjectId, glam::Mat4>>,
    dirty_groups: RefCell<HashSet<ModelGroupId>>,
    dirty_objects: RefCell<HashSet<ObjectId>>,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    pub fn new() -> Self {
        Self {
            local_lights: Vec::new(),
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
            cached_local_group_matrices: RefCell::new(HashMap::new()),
            cached_world_group_matrices: RefCell::new(HashMap::new()),
            cached_local_object_matrices: RefCell::new(HashMap::new()),
            cached_world_object_matrices: RefCell::new(HashMap::new()),
            dirty_groups: RefCell::new(HashSet::new()),
            dirty_objects: RefCell::new(HashSet::new()),
        }
    }

    fn group_is_descendant_of(&self, group_id: ModelGroupId, root: ModelGroupId) -> bool {
        let mut current = Some(group_id);
        let mut visited = 0;
        while let Some(id) = current {
            if id == root {
                return true;
            }
            current = self.model_group_parent(id);
            visited += 1;
            if visited > self.model_groups.len() {
                return false;
            }
        }
        false
    }

    fn mark_group_dirty(&mut self, root: ModelGroupId) {
        let mut stack = vec![root];
        while let Some(group_id) = stack.pop() {
            self.dirty_groups.borrow_mut().insert(group_id);
            self.cached_world_group_matrices
                .borrow_mut()
                .remove(&group_id);
            let object_ids: Vec<_> = self
                .objects
                .iter()
                .enumerate()
                .filter_map(|(index, object)| {
                    let attached = object
                        .model_group()
                        .is_some_and(|owner| self.group_is_descendant_of(owner, group_id))
                        || object
                            .parent_node()
                            .is_some_and(|node| self.group_is_descendant_of(node, group_id));
                    attached.then(|| self.object_ids[index])
                })
                .collect();
            for object_id in object_ids {
                self.dirty_objects.borrow_mut().insert(object_id);
                self.cached_world_object_matrices
                    .borrow_mut()
                    .remove(&object_id);
            }
            for object_id in self
                .model_group(group_id)
                .map(|group| group.objects.clone())
                .unwrap_or_default()
            {
                self.dirty_objects.borrow_mut().insert(object_id);
                self.cached_world_object_matrices
                    .borrow_mut()
                    .remove(&object_id);
            }
            stack.extend(
                self.model_group_ids
                    .iter()
                    .copied()
                    .filter(|&candidate| self.model_group_parent(candidate) == Some(group_id)),
            );
        }
    }

    fn mark_object_dirty(&mut self, object_id: ObjectId) {
        self.dirty_objects.borrow_mut().insert(object_id);
        self.cached_world_object_matrices
            .borrow_mut()
            .remove(&object_id);
    }

    /// Чистый метод добавления объекта без привязки к анимациям
    pub fn add_object(&mut self, object: RenderObject) -> ObjectId {
        let id = self.push_object(object);
        self.standalone_objects.push(id);
        id
    }

    pub fn local_lights(&self) -> &[LocalLight] {
        &self.local_lights
    }

    pub fn local_light_mut(&mut self, index: usize) -> Option<&mut LocalLight> {
        self.local_lights.get_mut(index)
    }

    pub fn local_light_parent(&self, index: usize) -> Option<ModelGroupId> {
        self.local_lights.get(index).and_then(|light| light.parent)
    }

    pub fn local_light_world_position(&self, index: usize) -> Option<Vec3> {
        let light = self.local_lights.get(index)?;
        let parent_matrix = light
            .parent
            .map_or(glam::Mat4::IDENTITY, |id| self.model_group_world_matrix(id));
        Some(parent_matrix.transform_point3(Vec3::from_array(light.position)))
    }

    pub fn local_light_world_direction(&self, index: usize) -> Option<Vec3> {
        let light = self.local_lights.get(index)?;
        let parent_matrix = light
            .parent
            .map_or(glam::Mat4::IDENTITY, |id| self.model_group_world_matrix(id));
        Some(
            parent_matrix
                .transform_vector3(Vec3::from_array(light.direction))
                .normalize_or_zero(),
        )
    }

    pub fn aim_local_light_at(&mut self, index: usize, target: Vec3) -> Result<(), String> {
        let position = self
            .local_light_world_position(index)
            .ok_or_else(|| "Local light does not exist".to_owned())?;
        let world_direction = (target - position).normalize_or_zero();
        if !world_direction.is_finite() || world_direction.length_squared() <= f32::EPSILON {
            return Err("The target is too close to the local light".to_owned());
        }
        let parent = self.local_light_parent(index);
        let parent_matrix =
            parent.map_or(glam::Mat4::IDENTITY, |id| self.model_group_world_matrix(id));
        let determinant = parent_matrix.determinant();
        if !determinant.is_finite() || determinant.abs() <= f32::EPSILON {
            return Err("Cannot aim a light under a parent with zero scale".to_owned());
        }
        let local_direction = parent_matrix
            .inverse()
            .transform_vector3(world_direction)
            .normalize_or_zero();
        if !local_direction.is_finite() || local_direction.length_squared() <= f32::EPSILON {
            return Err("Could not aim the local light at the selected target".to_owned());
        }
        self.local_lights[index].direction = local_direction.to_array();
        Ok(())
    }

    pub fn set_local_light_parent(
        &mut self,
        index: usize,
        parent: Option<ModelGroupId>,
    ) -> Result<(), String> {
        let current_parent = self
            .local_lights
            .get(index)
            .ok_or_else(|| "Local light does not exist".to_owned())?
            .parent;
        if current_parent == parent {
            return Ok(());
        }
        if parent.is_some_and(|id| self.model_group(id).is_none()) {
            return Err("Local light parent does not exist".to_owned());
        }

        let world_position = self
            .local_light_world_position(index)
            .expect("validated local light index must exist");
        let world_direction = self
            .local_light_world_direction(index)
            .expect("validated local light index must exist");
        let parent_matrix =
            parent.map_or(glam::Mat4::IDENTITY, |id| self.model_group_world_matrix(id));
        let determinant = parent_matrix.determinant();
        if !determinant.is_finite() || determinant.abs() <= f32::EPSILON {
            return Err("Cannot attach a light to a parent with zero scale".to_owned());
        }

        let inverse_parent = parent_matrix.inverse();
        let local_position = inverse_parent.transform_point3(world_position);
        let local_direction = inverse_parent
            .transform_vector3(world_direction)
            .normalize_or_zero();
        if !local_position.is_finite()
            || !local_direction.is_finite()
            || local_direction.length_squared() <= f32::EPSILON
        {
            return Err("Could not preserve the local light's world transform".to_owned());
        }

        let light = &mut self.local_lights[index];
        light.parent = parent;
        light.position = local_position.to_array();
        light.direction = local_direction.to_array();
        Ok(())
    }

    pub fn add_local_light(&mut self, light: LocalLight) -> Result<usize, String> {
        if self.local_lights.len() >= MAX_LOCAL_LIGHTS {
            return Err(format!(
                "A scene can contain at most {MAX_LOCAL_LIGHTS} local lights"
            ));
        }
        if !light.is_valid() {
            return Err("Cannot add a local light with invalid settings".to_owned());
        }
        if light.casts_shadows
            && self
                .local_lights
                .iter()
                .filter(|light| light.casts_shadows)
                .count()
                >= MAX_LOCAL_SHADOW_SPOTLIGHTS
        {
            return Err(format!(
                "A scene can have at most {MAX_LOCAL_SHADOW_SPOTLIGHTS} local shadow-casting spotlights"
            ));
        }
        let index = self.local_lights.len();
        self.local_lights.push(light);
        Ok(index)
    }

    pub(crate) fn add_local_light_with_parent(
        &mut self,
        mut light: LocalLight,
        parent: Option<ModelGroupId>,
    ) -> Result<usize, String> {
        if parent.is_some_and(|id| self.model_group(id).is_none()) {
            return Err("Local light parent does not exist".to_owned());
        }
        light.parent = parent;
        self.add_local_light(light)
    }

    pub fn remove_local_light(&mut self, index: usize) -> Option<LocalLight> {
        (index < self.local_lights.len()).then(|| self.local_lights.remove(index))
    }

    pub fn duplicate_local_light(&mut self, index: usize) -> Option<usize> {
        let mut light = self.local_lights.get(index)?.clone();
        light.name = format!("{} Copy", light.name);
        light.position[0] += 0.5;
        light.casts_shadows = false;
        self.add_local_light(light).ok()
    }

    pub(crate) fn set_local_lights(&mut self, lights: Vec<LocalLight>) {
        self.local_lights = lights;
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
        self.mark_object_dirty(id);
        id
    }

    pub fn add_model_group(
        &mut self,
        name: impl Into<String>,
        source_path: PathBuf,
        transform: Transform,
    ) -> ModelGroupId {
        self.add_hierarchy_node(name.into(), Some(source_path), transform)
    }

    pub fn add_empty_node(
        &mut self,
        name: impl Into<String>,
        transform: Transform,
    ) -> ModelGroupId {
        self.add_hierarchy_node(name.into(), None, transform)
    }

    fn add_hierarchy_node(
        &mut self,
        name: String,
        source_path: Option<PathBuf>,
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
            name,
            transform,
            source_path,
            parent: None,
            gltf_node_index: None,
            animation_clips: Vec::new(),
            animation: None,
            objects: Vec::new(),
        });
        self.mark_group_dirty(id);
        id
    }

    pub fn add_group_object(
        &mut self,
        group_id: ModelGroupId,
        mut object: RenderObject,
    ) -> Option<ObjectId> {
        let group_index = *self.model_group_indices.get(&group_id)?;
        let group = self.model_groups.get(group_index)?;
        group.source_path.as_ref()?;
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

    pub fn model_group_parent(&self, id: ModelGroupId) -> Option<ModelGroupId> {
        self.model_group(id)?.parent
    }

    pub fn gltf_node_index(&self, id: ModelGroupId) -> Option<usize> {
        self.model_group(id)?.gltf_node_index
    }

    pub fn set_gltf_node_index(
        &mut self,
        id: ModelGroupId,
        node_index: Option<usize>,
    ) -> Result<(), String> {
        let group = self
            .model_group_mut(id)
            .ok_or_else(|| "Model group does not exist".to_owned())?;
        if node_index.is_some() && group.source_path.is_some() {
            return Err("Imported glTF nodes cannot own model assets".to_owned());
        }
        group.gltf_node_index = node_index;
        Ok(())
    }

    pub fn can_set_model_group_parent(
        &self,
        child: ModelGroupId,
        parent: Option<ModelGroupId>,
    ) -> bool {
        if self.model_group(child).is_none() {
            return false;
        }
        let Some(mut ancestor) = parent else {
            return true;
        };
        let mut visited = 0;
        loop {
            if ancestor == child {
                return false;
            }
            let Some(group) = self.model_group(ancestor) else {
                return false;
            };
            let Some(next) = group.parent else {
                return true;
            };
            ancestor = next;
            visited += 1;
            if visited >= self.model_groups.len() {
                return false;
            }
        }
    }

    pub fn set_model_group_parent(
        &mut self,
        child: ModelGroupId,
        parent: Option<ModelGroupId>,
    ) -> Result<(), String> {
        if !self.can_set_model_group_parent(child, parent) {
            return Err("Model group parent is missing or would create a cycle".to_owned());
        }
        let index = *self
            .model_group_indices
            .get(&child)
            .expect("validated model group must have an index");
        self.model_groups[index].parent = parent;
        self.mark_group_dirty(child);
        Ok(())
    }

    pub fn reparent_model_group_preserving_world(
        &mut self,
        child: ModelGroupId,
        parent: Option<ModelGroupId>,
    ) -> Result<(), String> {
        if !self.can_set_model_group_parent(child, parent) {
            return Err("Model group parent is missing or would create a cycle".to_owned());
        }
        let world_matrix = self.model_group_world_matrix(child);
        let parent_matrix =
            parent.map_or(glam::Mat4::IDENTITY, |id| self.model_group_world_matrix(id));
        let determinant = parent_matrix.determinant();
        if !determinant.is_finite() || determinant.abs() <= f32::EPSILON {
            return Err("Cannot preserve transform under a parent with zero scale".to_owned());
        }
        let local_matrix = parent_matrix.inverse() * world_matrix;
        let (scale, rotation, position) = local_matrix.to_scale_rotation_translation();
        if !scale.is_finite() || !rotation.is_finite() || !position.is_finite() {
            return Err("Could not preserve the model group's world transform".to_owned());
        }
        let transform = Transform::new(position, rotation.normalize(), scale);
        let recomposed = transform.to_matrix();
        let matrix_error = local_matrix
            .to_cols_array()
            .iter()
            .zip(recomposed.to_cols_array())
            .map(|(expected, actual)| (expected - actual).abs())
            .fold(0.0_f32, f32::max);
        let tolerance = local_matrix
            .to_cols_array()
            .iter()
            .map(|value| value.abs())
            .fold(1.0_f32, f32::max)
            * 1.0e-4;
        if matrix_error > tolerance {
            return Err(
                "Cannot preserve the transform because the new parent would require shear"
                    .to_owned(),
            );
        }
        self.set_model_group_parent(child, parent)?;
        let group = self
            .model_group_mut(child)
            .expect("validated model group must remain available");
        group.transform = transform;
        Ok(())
    }

    pub fn gltf_node_groups(&self, model_group: ModelGroupId) -> Vec<(usize, ModelGroupId)> {
        self.model_group_ids
            .iter()
            .copied()
            .filter_map(|id| {
                let mut current = Some(id);
                let mut belongs_to_model = false;
                let mut visited = 0;
                while let Some(parent) = current {
                    if parent == model_group {
                        belongs_to_model = id != model_group;
                        break;
                    }
                    current = self.model_group_parent(parent);
                    visited += 1;
                    if visited > self.model_groups.len() {
                        break;
                    }
                }
                let node_index = self.gltf_node_index(id)?;
                belongs_to_model.then_some((node_index, id))
            })
            .collect()
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
        let object_id = self
            .objects
            .iter()
            .enumerate()
            .find_map(|(index, candidate)| {
                std::ptr::eq(candidate, object).then(|| self.object_ids[index])
            });
        let Some(object_id) = object_id else {
            return object
                .parent_node()
                .or_else(|| object.model_group())
                .map_or_else(
                    || object.model_matrix(),
                    |id| self.model_group_world_matrix(id) * object.model_matrix(),
                );
        };
        self.group_model_matrix_for_id(object_id)
            .unwrap_or_else(|| object.model_matrix())
    }

    pub fn group_model_matrix_for_id(&self, object_id: ObjectId) -> Option<glam::Mat4> {
        let object = self.get_object(object_id)?;
        let local_matrix = object.model_matrix();
        let parent_node = object.parent_node().or_else(|| object.model_group());
        let Some(parent_node) = parent_node else {
            return Some(local_matrix);
        };
        let local_changed = self
            .cached_local_object_matrices
            .borrow()
            .get(&object_id)
            .is_none_or(|cached| *cached != local_matrix);
        let matrix = self.model_group_world_matrix(parent_node) * local_matrix;
        if !self.dirty_objects.borrow().contains(&object_id)
            && !local_changed
            && self
                .cached_world_object_matrices
                .borrow()
                .get(&object_id)
                .is_some_and(|cached| *cached == matrix)
        {
            return Some(matrix);
        }
        self.cached_local_object_matrices
            .borrow_mut()
            .insert(object_id, local_matrix);
        self.cached_world_object_matrices
            .borrow_mut()
            .insert(object_id, matrix);
        self.dirty_objects.borrow_mut().remove(&object_id);
        Some(matrix)
    }

    pub fn model_group_world_matrix(&self, id: ModelGroupId) -> glam::Mat4 {
        let Some(group) = self.model_group(id) else {
            return glam::Mat4::IDENTITY;
        };
        let local = group.transform.to_matrix();
        let local_changed = self
            .cached_local_group_matrices
            .borrow()
            .get(&id)
            .is_none_or(|cached| *cached != local);
        let parent_world = group.parent.map_or(glam::Mat4::IDENTITY, |parent| {
            self.model_group_world_matrix(parent)
        });
        let world = parent_world * local;
        if !self.dirty_groups.borrow().contains(&id)
            && !local_changed
            && self
                .cached_world_group_matrices
                .borrow()
                .get(&id)
                .is_some_and(|cached| *cached == world)
        {
            return world;
        }

        self.cached_local_group_matrices
            .borrow_mut()
            .insert(id, local);
        self.cached_world_group_matrices
            .borrow_mut()
            .insert(id, world);
        self.dirty_groups.borrow_mut().remove(&id);
        world
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

    pub fn set_object_parent_node(
        &mut self,
        object_id: ObjectId,
        parent: Option<ModelGroupId>,
    ) -> Result<(), String> {
        let object = self
            .get_object(object_id)
            .ok_or_else(|| "Render object does not exist".to_owned())?;
        let owner = object
            .model_group()
            .ok_or_else(|| "Only model-owned objects can be attached to a node".to_owned())?;
        if let Some(parent_id) = parent {
            if self.model_group(parent_id).is_none() {
                return Err("Object parent node does not exist".to_owned());
            }
            let mut current = Some(parent_id);
            let mut is_owned = false;
            let mut visited = 0;
            while let Some(id) = current {
                if id == owner {
                    is_owned = true;
                    break;
                }
                current = self.model_group_parent(id);
                visited += 1;
                if visited > self.model_groups.len() {
                    break;
                }
            }
            if !is_owned {
                return Err("Object parent must belong to its model hierarchy".to_owned());
            }
        }
        self.get_object_mut(object_id)
            .expect("validated render object must remain available")
            .set_parent_node(parent);
        self.mark_object_dirty(object_id);
        Ok(())
    }

    pub fn remove_object(&mut self, id: ObjectId) -> Option<RenderObject> {
        let object = self.remove_object_storage(id)?;
        self.standalone_objects.retain(|&object_id| object_id != id);

        if let Some(group_id) = object.model_group()
            && self.model_group(group_id).is_some_and(|group| {
                group.objects.is_empty()
                    && !self
                        .model_group_ids
                        .iter()
                        .any(|&candidate| self.model_group_parent(candidate) == Some(group_id))
            })
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
            self.mark_object_dirty(moved_id);
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
        self.dirty_objects.borrow_mut().remove(&id);
        self.cached_world_object_matrices.borrow_mut().remove(&id);
        self.cached_local_object_matrices.borrow_mut().remove(&id);
        Some(removed)
    }

    pub fn remove_model_group(&mut self, id: ModelGroupId) -> Option<ModelGroup> {
        self.model_group(id)?;
        let mut descendants = Vec::new();
        let mut stack = vec![id];
        while let Some(parent) = stack.pop() {
            descendants.push(parent);
            stack.extend(
                self.model_group_ids
                    .iter()
                    .copied()
                    .filter(|&candidate| self.model_group_parent(candidate) == Some(parent)),
            );
        }
        let attached_objects: Vec<_> = self
            .objects
            .iter()
            .enumerate()
            .filter_map(|(index, object)| {
                object
                    .parent_node()
                    .filter(|parent| descendants.contains(parent))
                    .and_then(|_| self.object_ids.get(index).copied())
            })
            .collect();
        let attached_lights: Vec<_> = self
            .local_lights
            .iter()
            .enumerate()
            .filter_map(|(index, light)| {
                light
                    .parent
                    .filter(|parent| descendants.contains(parent))
                    .map(|_| index)
            })
            .collect();
        for index in attached_lights.into_iter().rev() {
            self.local_lights.remove(index);
        }
        for object_id in attached_objects {
            self.remove_object_storage(object_id);
        }
        let mut removed_root = None;
        for group_id in descendants.into_iter().rev() {
            let group = self.model_group(group_id)?.clone();
            for object_id in &group.objects {
                self.remove_object_storage(*object_id);
            }
            let removed = self.remove_model_group_storage(group_id)?;
            if group_id == id {
                removed_root = Some(removed);
            }
        }
        removed_root
    }

    fn remove_model_group_storage(&mut self, id: ModelGroupId) -> Option<ModelGroup> {
        let index = self.model_group_indices.remove(&id)?;
        let group = self.model_groups.swap_remove(index);
        self.model_group_ids.swap_remove(index);
        if let Some(&moved_id) = self.model_group_ids.get(index) {
            self.model_group_indices.insert(moved_id, index);
            self.mark_group_dirty(moved_id);
        }
        self.dirty_groups.borrow_mut().remove(&id);
        self.cached_local_group_matrices.borrow_mut().remove(&id);
        self.cached_world_group_matrices.borrow_mut().remove(&id);
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
        fn descendants(world: &World, root: ModelGroupId) -> Vec<ModelGroupId> {
            let mut result = Vec::new();
            let mut stack = vec![root];
            while let Some(parent) = stack.pop() {
                result.push(parent);
                stack.extend(
                    world
                        .model_group_ids
                        .iter()
                        .copied()
                        .filter(|&candidate| world.model_group_parent(candidate) == Some(parent)),
                );
            }
            result
        }

        fn duplicate_subtree(
            world: &mut World,
            source_id: ModelGroupId,
            parent: Option<ModelGroupId>,
            root: bool,
            copies: &mut HashMap<ModelGroupId, ModelGroupId>,
        ) -> Option<ModelGroupId> {
            let source = world.model_group(source_id)?.clone();
            let children: Vec<_> = world
                .model_group_ids
                .iter()
                .copied()
                .filter(|&candidate| world.model_group_parent(candidate) == Some(source_id))
                .collect();
            let mut transform = source.transform;
            if root {
                transform.position += Vec3::X;
            }
            let copy_name = if root {
                format!("{} Copy", source.name)
            } else {
                source.name.clone()
            };
            let copy_id =
                world.add_hierarchy_node(copy_name, source.source_path.clone(), transform);
            world.set_model_group_parent(copy_id, parent).ok()?;
            if let Some(copy) = world.model_group_mut(copy_id) {
                copy.animation_clips = source.animation_clips;
                copy.animation = source.animation;
                copy.gltf_node_index = source.gltf_node_index;
            }
            copies.insert(source_id, copy_id);
            for child in children {
                duplicate_subtree(world, child, Some(copy_id), false, copies)?;
            }
            Some(copy_id)
        }

        let parent = self.model_group(id)?.parent;
        let source_subtree = descendants(self, id);
        let source_subtree_set: HashSet<_> = source_subtree.iter().copied().collect();
        let source_objects: Vec<_> = self
            .objects
            .iter()
            .filter(|object| {
                object
                    .model_group()
                    .is_some_and(|owner| source_subtree_set.contains(&owner))
                    || object
                        .parent_node()
                        .is_some_and(|node| source_subtree_set.contains(&node))
            })
            .cloned()
            .collect();
        let mut copies = HashMap::new();
        let copy_root = duplicate_subtree(self, id, parent, true, &mut copies)?;

        for (&source_id, &copy_id) in &copies {
            if self.gltf_node_index(source_id).is_none() {
                continue;
            }
            let mut ancestor = self.model_group_parent(source_id);
            let mut owner_is_copied = false;
            while let Some(ancestor_id) = ancestor {
                let Some(group) = self.model_group(ancestor_id) else {
                    break;
                };
                if group.source_path.is_some() {
                    owner_is_copied = source_subtree_set.contains(&ancestor_id);
                    break;
                }
                ancestor = group.parent;
            }
            if !owner_is_copied {
                self.set_gltf_node_index(copy_id, None).ok()?;
            }
        }

        for object in source_objects {
            let owner = object.model_group()?;
            let copy_owner = copies.get(&owner).copied().unwrap_or(owner);
            let parent_node = object
                .parent_node()
                .map(|node| copies.get(&node).copied().unwrap_or(node));
            let object_id = self.add_group_object(copy_owner, object)?;
            if let Some(parent_node) = parent_node {
                self.set_object_parent_node(object_id, Some(parent_node))
                    .ok()?;
            }
        }
        Some(copy_root)
    }

    pub(crate) fn id_sequences(&self) -> (usize, usize) {
        (self.next_object_id, self.next_model_group_id)
    }

    pub(crate) fn preserve_id_sequences(&mut self, sequences: (usize, usize)) {
        self.next_object_id = self.next_object_id.max(sequences.0);
        self.next_model_group_id = self.next_model_group_id.max(sequences.1);
    }

    pub fn update(&mut self, dt: f32) {
        if let Some(cube_id) = self.rotating_cube_id
            && let Some(cube) = self.get_object_mut(cube_id)
        {
            let rotation_delta = Quat::from_rotation_y(1.0 * dt);
            cube.transform.rotation = rotation_delta * cube.transform.rotation;
        }
    }

    pub fn objects(&self) -> &[RenderObject] {
        &self.objects
    }

    pub(crate) fn objects_mut(&mut self) -> &mut [RenderObject] {
        for &object_id in &self.object_ids.clone() {
            self.mark_object_dirty(object_id);
        }
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
    fn local_lights_validate_settings_and_respect_the_scene_limit() {
        let mut world = World::new();
        let point = LocalLight::point("Point", [1.0, 2.0, 3.0]);
        assert!(point.is_valid());
        assert_eq!(world.add_local_light(point.clone()), Ok(0));
        assert_eq!(world.remove_local_light(4), None);
        assert_eq!(world.remove_local_light(0), Some(point));

        assert!(
            world
                .add_local_light(LocalLight::spot(
                    "Invalid spot",
                    [0.0, 0.0, 0.0],
                    [0.0, 0.0, 0.0],
                ))
                .is_err()
        );
        for index in 0..MAX_LOCAL_LIGHTS {
            world
                .add_local_light(LocalLight::point(format!("Point {index}"), [0.0, 0.0, 0.0]))
                .unwrap();
        }
        assert_eq!(world.local_lights().len(), MAX_LOCAL_LIGHTS);
        assert!(
            world
                .add_local_light(LocalLight::point("Overflow", [0.0, 0.0, 0.0]))
                .is_err()
        );
    }

    #[test]
    fn local_shadow_casting_is_limited_to_four_spotlights() {
        let mut world = World::new();
        for index in 0..MAX_LOCAL_SHADOW_SPOTLIGHTS {
            let mut spot = LocalLight::spot(
                format!("Shadow spot {index}"),
                [index as f32, 3.0, 0.0],
                [0.0, -1.0, 0.0],
            );
            spot.casts_shadows = true;
            assert!(world.add_local_light(spot).is_ok());
        }
        let mut overflow = LocalLight::spot("Overflow spot", [0.0, 3.0, 0.0], [0.0, -1.0, 0.0]);
        overflow.casts_shadows = true;
        assert!(world.add_local_light(overflow).is_err());

        let mut point = LocalLight::point("Point", [0.0, 3.0, 0.0]);
        point.casts_shadows = true;
        assert!(!point.is_valid());
    }

    #[test]
    fn local_lights_follow_parents_and_keep_world_pose_when_reparented() {
        let mut world = World::new();
        let light_index = world
            .add_local_light(LocalLight::spot(
                "Follow light",
                [1.0, 2.0, 3.0],
                [0.0, -1.0, 0.0],
            ))
            .unwrap();
        let parent = world.add_empty_node(
            "Moving node",
            Transform::new(
                Vec3::new(4.0, 0.0, -2.0),
                Quat::from_rotation_y(0.7),
                Vec3::ONE,
            ),
        );
        let world_position = world.local_light_world_position(light_index).unwrap();
        let world_direction = world.local_light_world_direction(light_index).unwrap();

        world
            .set_local_light_parent(light_index, Some(parent))
            .unwrap();
        assert!(
            world
                .local_light_world_position(light_index)
                .unwrap()
                .abs_diff_eq(world_position, 1e-5)
        );
        assert!(
            world
                .local_light_world_direction(light_index)
                .unwrap()
                .abs_diff_eq(world_direction, 1e-5)
        );

        world.model_group_mut(parent).unwrap().transform.position += Vec3::X;
        assert!(
            world
                .local_light_world_position(light_index)
                .unwrap()
                .abs_diff_eq(world_position + Vec3::X, 1e-5)
        );
    }

    #[test]
    fn removing_a_parent_also_removes_its_local_lights() {
        let mut world = World::new();
        let parent = world.add_empty_node("Parent", Transform::default());
        let light_index = world
            .add_local_light(LocalLight::point("Child light", [1.0, 2.0, 3.0]))
            .unwrap();
        world
            .set_local_light_parent(light_index, Some(parent))
            .unwrap();

        world.remove_model_group(parent);

        assert!(world.local_lights().is_empty());
    }

    #[test]
    fn keyboard_animation_control_is_opt_in_and_maps_named_clips() {
        let clips = [
            ModelAnimationClip {
                name: "Run".to_owned(),
                duration: 1.0,
            },
            ModelAnimationClip {
                name: "survey".to_owned(),
                duration: 1.0,
            },
            ModelAnimationClip {
                name: "Walk".to_owned(),
                duration: 1.0,
            },
        ];

        assert_eq!(
            AnimationKeyboardControl::for_clips(&clips),
            AnimationKeyboardControl {
                enabled: false,
                idle_clip_index: 1,
                walk_clip_index: 2,
                run_clip_index: 0,
            }
        );
    }

    #[test]
    fn selecting_clip_starts_a_configured_transition() {
        let mut playback = ModelAnimationPlayback {
            clip_index: 0,
            time: 0.4,
            crossfade_duration: 0.8,
            ..ModelAnimationPlayback::default()
        };

        assert!(playback.switch_to_clip(2));
        assert_eq!(playback.clip_index, 2);
        assert_eq!(playback.time, 0.0);
        assert_eq!(
            playback.transition,
            Some(ModelAnimationTransition {
                from_clip_index: 0,
                from_time: 0.4,
                elapsed: 0.0,
                duration: 0.8,
            })
        );
        assert!(!playback.switch_to_clip(1));
        assert_eq!(playback.clip_index, 2);

        let mut instant = ModelAnimationPlayback {
            crossfade_duration: 0.0,
            ..ModelAnimationPlayback::default()
        };
        assert!(instant.switch_to_clip(1));
        assert!(instant.transition.is_none());
    }

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

    #[test]
    fn nested_model_group_transforms_are_composed() {
        let mut world = World::new();
        let parent = world.add_model_group(
            "Parent",
            PathBuf::from("parent.glb"),
            Transform::from_translation(Vec3::new(10.0, 0.0, 0.0)),
        );
        let child = world.add_model_group(
            "Child",
            PathBuf::from("child.glb"),
            Transform::from_translation(Vec3::new(0.0, 2.0, 0.0)),
        );
        world.set_model_group_parent(child, Some(parent)).unwrap();
        let object = RenderObject::new(
            Transform::from_translation(Vec3::Z),
            MeshId(0),
            MaterialId(0),
        );
        let object_id = world.add_group_object(child, object).unwrap();
        let position = world
            .group_model_matrix(world.get_object(object_id).unwrap())
            .transform_point3(Vec3::ZERO);

        assert_eq!(position, Vec3::new(10.0, 2.0, 1.0));
    }

    #[test]
    fn parent_transform_change_propagates_to_nested_groups() {
        let mut world = World::new();
        let parent = world.add_empty_node(
            "Parent",
            Transform::from_translation(Vec3::new(10.0, 0.0, 0.0)),
        );
        let child = world.add_empty_node(
            "Child",
            Transform::from_translation(Vec3::new(0.0, 3.0, 0.0)),
        );
        world.set_model_group_parent(child, Some(parent)).unwrap();

        let before = world.model_group_world_matrix(child);
        world.model_group_mut(parent).unwrap().transform.position = Vec3::new(20.0, 5.0, 0.0);
        let after = world.model_group_world_matrix(child);

        assert_eq!(
            before.transform_point3(Vec3::ZERO),
            Vec3::new(10.0, 3.0, 0.0)
        );
        assert_eq!(
            after.transform_point3(Vec3::ZERO),
            Vec3::new(20.0, 8.0, 0.0)
        );
    }

    #[test]
    fn world_matrix_cache_returns_current_matrix_without_mutation() {
        let mut world = World::new();
        let group = world.add_empty_node(
            "Node",
            Transform::from_translation(Vec3::new(1.0, 2.0, 3.0)),
        );

        let first = world.model_group_world_matrix(group);
        let second = world.model_group_world_matrix(group);
        world.model_group_mut(group).unwrap().transform.position = Vec3::new(4.0, 5.0, 6.0);
        let updated = world.model_group_world_matrix(group);

        assert_eq!(first, second);
        assert_eq!(
            updated.transform_point3(Vec3::ZERO),
            Vec3::new(4.0, 5.0, 6.0)
        );
    }

    #[test]
    fn removing_parent_removes_cached_descendant_world_matrices() {
        let mut world = World::new();
        let parent = world.add_empty_node("Parent", Transform::default());
        let child = world.add_empty_node("Child", Transform::from_translation(Vec3::X));
        world.set_model_group_parent(child, Some(parent)).unwrap();

        assert_eq!(
            world
                .model_group_world_matrix(child)
                .transform_point3(Vec3::ZERO),
            Vec3::X
        );
        world.remove_model_group(parent).unwrap();

        assert!(world.model_group(parent).is_none());
        assert!(world.model_group(child).is_none());
        assert_eq!(world.model_group_world_matrix(child), glam::Mat4::IDENTITY);
    }

    #[test]
    fn model_group_parenting_rejects_cycles() {
        let mut world = World::new();
        let parent =
            world.add_model_group("Parent", PathBuf::from("parent.glb"), Transform::default());
        let child =
            world.add_model_group("Child", PathBuf::from("child.glb"), Transform::default());
        world.set_model_group_parent(child, Some(parent)).unwrap();

        assert!(!world.can_set_model_group_parent(parent, Some(child)));
        assert!(world.set_model_group_parent(parent, Some(child)).is_err());
        assert!(world.set_model_group_parent(parent, Some(parent)).is_err());
    }

    #[test]
    fn reparenting_preserves_the_world_transform() {
        let mut world = World::new();
        let old_parent = world.add_empty_node(
            "Old parent",
            Transform::new(
                Vec3::new(8.0, 0.0, 0.0),
                Quat::from_rotation_y(0.4),
                Vec3::splat(1.5),
            ),
        );
        let new_parent = world.add_empty_node(
            "New parent",
            Transform::new(
                Vec3::new(-3.0, 2.0, 0.0),
                Quat::from_rotation_x(-0.2),
                Vec3::splat(0.75),
            ),
        );
        let child = world.add_empty_node(
            "Child",
            Transform::new(
                Vec3::new(1.0, 4.0, 0.0),
                Quat::from_rotation_z(0.3),
                Vec3::splat(0.8),
            ),
        );
        world
            .set_model_group_parent(child, Some(old_parent))
            .unwrap();
        let before = world.model_group_world_matrix(child);

        world
            .reparent_model_group_preserving_world(child, Some(new_parent))
            .unwrap();
        let after = world.model_group_world_matrix(child);

        for (before, after) in before.to_cols_array().iter().zip(after.to_cols_array()) {
            assert!((before - after).abs() < 1.0e-5);
        }
    }

    #[test]
    fn empty_nodes_have_no_asset_and_duplicate_with_their_subtree() {
        let mut world = World::new();
        let parent = world.add_empty_node("Root", Transform::from_translation(Vec3::X));
        let child = world.add_empty_node("Nested", Transform::from_translation(Vec3::Y));
        world.set_model_group_parent(child, Some(parent)).unwrap();

        let copy = world.duplicate_model_group(parent).unwrap();
        let copied_child = world
            .model_group_ids()
            .iter()
            .copied()
            .find(|&id| world.model_group_parent(id) == Some(copy))
            .unwrap();

        assert!(world.model_group(parent).unwrap().source_path.is_none());
        assert!(world.model_group(copy).unwrap().source_path.is_none());
        assert_eq!(world.model_group(copied_child).unwrap().name, "Nested");
    }

    #[test]
    fn reparenting_rejects_transforms_that_require_shear() {
        let mut world = World::new();
        let parent = world.add_empty_node(
            "Nonuniform parent",
            Transform::new(Vec3::ZERO, Quat::IDENTITY, Vec3::new(2.0, 1.0, 1.0)),
        );
        let child = world.add_empty_node(
            "Rotated child",
            Transform::new(Vec3::ZERO, Quat::from_rotation_z(0.5), Vec3::ONE),
        );
        let original = world.model_group(child).unwrap().transform;

        assert!(
            world
                .reparent_model_group_preserving_world(child, Some(parent))
                .is_err()
        );
        assert_eq!(world.model_group_parent(child), None);
        assert_eq!(
            world.model_group(child).unwrap().transform.rotation,
            original.rotation
        );
    }

    #[test]
    fn removing_model_group_removes_its_nested_groups() {
        let mut world = World::new();
        let parent =
            world.add_model_group("Parent", PathBuf::from("parent.glb"), Transform::default());
        let child =
            world.add_model_group("Child", PathBuf::from("child.glb"), Transform::default());
        world.set_model_group_parent(child, Some(parent)).unwrap();
        let object = world
            .add_group_object(
                child,
                RenderObject::new(Transform::default(), MeshId(0), MaterialId(0)),
            )
            .unwrap();

        world.remove_model_group(parent).unwrap();

        assert!(world.model_group(parent).is_none());
        assert!(world.model_group(child).is_none());
        assert!(world.get_object(object).is_none());
    }

    #[test]
    fn duplicating_model_group_copies_its_nested_groups() {
        let mut world = World::new();
        let parent = world.add_model_group(
            "Parent",
            PathBuf::from("parent.glb"),
            Transform::from_translation(Vec3::new(2.0, 0.0, 0.0)),
        );
        let child = world.add_model_group(
            "Child",
            PathBuf::from("child.glb"),
            Transform::from_translation(Vec3::new(0.0, 3.0, 0.0)),
        );
        world.set_model_group_parent(child, Some(parent)).unwrap();
        world
            .add_group_object(
                child,
                RenderObject::new(Transform::default(), MeshId(0), MaterialId(0)),
            )
            .unwrap();

        let copy = world.duplicate_model_group(parent).unwrap();
        let copied_child = world
            .model_group_ids()
            .iter()
            .copied()
            .find(|&id| world.model_group_parent(id) == Some(copy))
            .unwrap();

        assert_eq!(world.model_group(copy).unwrap().name, "Parent Copy");
        assert_eq!(world.model_group(copied_child).unwrap().name, "Child");
        assert_eq!(
            world
                .model_group_world_matrix(copied_child)
                .transform_point3(Vec3::ZERO),
            Vec3::new(3.0, 3.0, 0.0)
        );
        assert_eq!(world.model_group(copied_child).unwrap().objects.len(), 1);
    }
}
