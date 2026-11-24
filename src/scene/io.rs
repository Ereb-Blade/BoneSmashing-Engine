use crate::camera::Camera;
use crate::core::{MaterialId, MeshId};
use crate::renderer::material::MaterialParams;
use crate::renderer::state::{ParsedAsset, RendererSettings, parse_asset};
use crate::scene::{ModelGroupId, RenderObject, Transform, World};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const SCENE_FORMAT_VERSION: u32 = 7;
const MAX_SCENE_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SCENE_OBJECTS: usize = 100_000;
const MAX_SCENE_ASSETS: usize = 256;
const MAX_SCENE_GROUPS: usize = 1_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneFile {
    pub version: u32,
    #[serde(default)]
    pub groups: Vec<SceneGroup>,
    pub objects: Vec<SceneObject>,
    #[serde(default)]
    pub camera: Option<SceneCamera>,
    #[serde(default)]
    pub renderer_settings: Option<SceneRendererSettings>,
    #[serde(default)]
    pub environment: Option<SceneEnvironment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneGroup {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_group: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gltf_node_index: Option<usize>,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<SceneAnimationSettings>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneAnimationSettings {
    pub clip_index: usize,
    pub speed: f32,
    pub looping: bool,
    pub playing: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneObject {
    pub source: SceneObjectSource,
    #[serde(default)]
    pub model_group: Option<usize>,
    #[serde(default)]
    pub parent_node: Option<usize>,
    #[serde(default)]
    pub name: String,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    #[serde(default)]
    pub material_override: Option<SceneMaterialOverride>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneCamera {
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub up: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneRendererSettings {
    pub key_light_yaw_degrees: f32,
    pub key_light_elevation_degrees: f32,
    pub key_light_intensity: f32,
    pub fill_light_intensity: f32,
    pub environment_intensity: f32,
    pub exposure_ev: f32,
    pub shadows_enabled: bool,
    pub debug_view: u32,
    pub unlocked_fps: bool,
    pub render_scale: f32,
    pub msaa_samples: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneEnvironment {
    pub path: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneMaterialOverride {
    pub roughness: f32,
    pub metallic: f32,
}

impl From<RendererSettings> for SceneRendererSettings {
    fn from(settings: RendererSettings) -> Self {
        Self {
            key_light_yaw_degrees: settings.key_light_yaw_degrees,
            key_light_elevation_degrees: settings.key_light_elevation_degrees,
            key_light_intensity: settings.key_light_intensity,
            fill_light_intensity: settings.fill_light_intensity,
            environment_intensity: settings.environment_intensity,
            exposure_ev: settings.exposure_ev,
            shadows_enabled: settings.shadows_enabled,
            debug_view: settings.debug_view,
            unlocked_fps: settings.unlocked_fps,
            render_scale: settings.render_scale,
            msaa_samples: settings.msaa_samples,
        }
    }
}

impl From<SceneRendererSettings> for RendererSettings {
    fn from(settings: SceneRendererSettings) -> Self {
        Self {
            key_light_yaw_degrees: settings.key_light_yaw_degrees,
            key_light_elevation_degrees: settings.key_light_elevation_degrees,
            key_light_intensity: settings.key_light_intensity,
            fill_light_intensity: settings.fill_light_intensity,
            environment_intensity: settings.environment_intensity,
            exposure_ev: settings.exposure_ev,
            shadows_enabled: settings.shadows_enabled,
            debug_view: settings.debug_view,
            unlocked_fps: settings.unlocked_fps,
            render_scale: settings.render_scale,
            msaa_samples: settings.msaa_samples,
        }
    }
}

impl SceneCamera {
    pub fn from_camera(camera: &Camera) -> Self {
        Self {
            position: camera.eye.to_array(),
            yaw: camera.yaw,
            pitch: camera.pitch,
            up: camera.up.to_array(),
        }
    }

    pub fn apply_to(&self, camera: &mut Camera) {
        camera.eye = Vec3::from_array(self.position);
        camera.yaw = self.yaw;
        camera.pitch = self.pitch;
        camera.up = Vec3::from_array(self.up).normalize();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SceneObjectSource {
    BuiltIn { mesh_id: usize, material_id: usize },
    Asset { path: String, instance_index: usize },
}

pub struct LoadedScene {
    pub file: SceneFile,
    pub assets: BTreeMap<PathBuf, ParsedAsset>,
}

impl SceneFile {
    pub fn from_world(world: &World, scene_path: &Path) -> Result<Self, String> {
        let scene_directory = scene_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let scene_directory = fs::canonicalize(scene_directory).map_err(|error| {
            format!(
                "Could not resolve scene directory {}: {error}",
                scene_directory.display()
            )
        })?;
        let mut objects = Vec::with_capacity(world.objects().len());
        let mut groups = Vec::with_capacity(world.model_groups().len());
        let group_indices: HashMap<_, _> = world
            .model_group_ids()
            .iter()
            .copied()
            .enumerate()
            .map(|(index, id)| (id, index))
            .collect();
        let mut stored_asset_paths =
            HashMap::with_capacity(world.objects().len().min(MAX_SCENE_ASSETS));
        for (group_id, group) in world
            .model_group_ids()
            .iter()
            .copied()
            .zip(world.model_groups())
        {
            let stored_path = if let Some(source_path) = &group.source_path {
                if !stored_asset_paths.contains_key(source_path) {
                    let path = fs::canonicalize(source_path).map_err(|error| {
                        format!(
                            "Could not resolve model path {}: {error}",
                            source_path.display()
                        )
                    })?;
                    let stored_path = path
                        .strip_prefix(&scene_directory)
                        .unwrap_or(&path)
                        .to_str()
                        .ok_or_else(|| {
                            format!("Model path is not valid UTF-8: {}", path.display())
                        })?
                        .to_owned();
                    stored_asset_paths.insert(source_path.clone(), stored_path);
                }
                Some(
                    stored_asset_paths
                        .get(source_path)
                        .ok_or_else(|| "Model path was not prepared for saving".to_owned())?
                        .clone(),
                )
            } else {
                None
            };
            groups.push(SceneGroup {
                name: group.name.clone(),
                path: stored_path,
                parent_group: world
                    .model_group_parent(group_id)
                    .map(|parent| {
                        group_indices.get(&parent).copied().ok_or_else(|| {
                            format!("Model group '{}' references a missing parent", group.name)
                        })
                    })
                    .transpose()?,
                gltf_node_index: world.gltf_node_index(group_id),
                position: group.transform.position.to_array(),
                rotation: group.transform.rotation.to_array(),
                scale: group.transform.scale.to_array(),
                animation: group.animation.map(|animation| SceneAnimationSettings {
                    clip_index: animation.clip_index,
                    speed: animation.speed,
                    looping: animation.looping,
                    playing: animation.playing,
                }),
            });
        }
        for object in world.objects() {
            let source = if let Some(asset) = object.asset_source() {
                if !stored_asset_paths.contains_key(&asset.path) {
                    let path = fs::canonicalize(&asset.path).map_err(|error| {
                        format!(
                            "Could not resolve model path {}: {error}",
                            asset.path.display()
                        )
                    })?;
                    let stored_path = path
                        .strip_prefix(&scene_directory)
                        .unwrap_or(&path)
                        .to_str()
                        .ok_or_else(|| {
                            format!("Model path is not valid UTF-8: {}", path.display())
                        })?
                        .to_owned();
                    stored_asset_paths.insert(asset.path.clone(), stored_path);
                }
                let stored_path = stored_asset_paths
                    .get(&asset.path)
                    .ok_or_else(|| "Model path was not prepared for saving".to_owned())?;
                SceneObjectSource::Asset {
                    path: stored_path.clone(),
                    instance_index: asset.instance_index,
                }
            } else {
                SceneObjectSource::BuiltIn {
                    mesh_id: object.mesh_id.0,
                    material_id: object.material_id.0,
                }
            };
            let model_group = match object.model_group() {
                Some(id) => Some(*group_indices.get(&id).ok_or_else(|| {
                    format!("Object '{}' references a missing model group", object.name)
                })?),
                None => None,
            };
            let parent_node = object
                .parent_node()
                .map(|id| {
                    group_indices.get(&id).copied().ok_or_else(|| {
                        format!("Object '{}' references a missing parent node", object.name)
                    })
                })
                .transpose()?;
            objects.push(SceneObject {
                source,
                model_group,
                parent_node,
                name: object.name.clone(),
                position: object.transform.position.to_array(),
                rotation: object.transform.rotation.to_array(),
                scale: object.transform.scale.to_array(),
                material_override: None,
            });
        }
        let file = Self {
            version: SCENE_FORMAT_VERSION,
            groups,
            objects,
            camera: None,
            renderer_settings: None,
            environment: None,
        };
        file.validate()?;
        Ok(file)
    }

    pub fn capture_editor_state(
        &mut self,
        world: &World,
        scene_path: &Path,
        camera: &Camera,
        renderer_settings: RendererSettings,
        environment_path: Option<&Path>,
        material_params: &[MaterialParams],
    ) -> Result<(), String> {
        if self.objects.len() != world.objects().len()
            || material_params.len() != world.objects().len()
        {
            return Err("Scene objects and material overrides are out of sync".to_owned());
        }
        let scene_directory = scene_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let scene_directory = fs::canonicalize(scene_directory).map_err(|error| {
            format!(
                "Could not resolve scene directory {}: {error}",
                scene_directory.display()
            )
        })?;
        self.camera = Some(SceneCamera::from_camera(camera));
        self.renderer_settings = Some(renderer_settings.into());
        self.environment = Some(SceneEnvironment {
            path: environment_path
                .map(|path| store_scene_path(path, &scene_directory))
                .transpose()?,
        });
        for (saved_object, params) in self.objects.iter_mut().zip(material_params) {
            saved_object.material_override = Some(SceneMaterialOverride {
                roughness: params.roughness,
                metallic: params.metallic,
            });
        }
        self.validate()?;
        Ok(())
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("Could not encode scene: {error}"))?;
        if bytes.len() as u64 > MAX_SCENE_FILE_BYTES {
            return Err(format!(
                "Scene file exceeds the {} MiB limit",
                MAX_SCENE_FILE_BYTES / (1024 * 1024)
            ));
        }
        fs::write(path, bytes)
            .map_err(|error| format!("Could not save scene to {}: {error}", path.display()))
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != SCENE_FORMAT_VERSION {
            return Err(format!(
                "Unsupported scene version {} (supported version: {SCENE_FORMAT_VERSION})",
                self.version
            ));
        }
        if self.objects.len() > MAX_SCENE_OBJECTS {
            return Err(format!(
                "Scene contains too many objects (maximum: {MAX_SCENE_OBJECTS})"
            ));
        }
        if self.groups.len() > MAX_SCENE_GROUPS {
            return Err(format!(
                "Scene contains too many model groups (maximum: {MAX_SCENE_GROUPS})"
            ));
        }
        if let Some(camera) = &self.camera {
            let position = Vec3::from_array(camera.position);
            let up = Vec3::from_array(camera.up);
            if !position.is_finite()
                || !camera.yaw.is_finite()
                || !camera.pitch.is_finite()
                || !(-89.0_f32.to_radians()..=89.0_f32.to_radians()).contains(&camera.pitch)
                || !up.is_finite()
                || up.length_squared() <= f32::EPSILON
            {
                return Err("Scene camera has invalid data".to_owned());
            }
        }
        if let Some(settings) = self.renderer_settings
            && !settings.is_valid()
        {
            return Err("Scene render settings have invalid data".to_owned());
        }
        if self.environment.as_ref().is_some_and(|environment| {
            environment
                .path
                .as_ref()
                .is_some_and(|path| path.trim().is_empty())
        }) {
            return Err("Scene environment path is empty".to_owned());
        }
        for (index, object) in self.objects.iter().enumerate() {
            let position = Vec3::from_array(object.position);
            let rotation = Quat::from_array(object.rotation);
            let scale = Vec3::from_array(object.scale);
            if !position.is_finite()
                || !rotation.is_finite()
                || !rotation.length_squared().is_finite()
                || rotation.length_squared() <= f32::EPSILON
                || !scale.is_finite()
            {
                return Err(format!("Scene object {index} has an invalid transform"));
            }
            if object.material_override.is_some_and(|material| {
                !material.roughness.is_finite()
                    || !(0.0..=1.0).contains(&material.roughness)
                    || !material.metallic.is_finite()
                    || !(0.0..=1.0).contains(&material.metallic)
            }) {
                return Err(format!(
                    "Scene object {index} has an invalid material override"
                ));
            }
            if let SceneObjectSource::Asset { path, .. } = &object.source {
                let path = Path::new(path);
                if path.as_os_str().is_empty() {
                    return Err(format!("Scene object {index} has an empty model path"));
                }
            }
            if object
                .model_group
                .is_some_and(|group_id| group_id >= self.groups.len())
            {
                return Err(format!(
                    "Scene object {index} references an unknown model group"
                ));
            }
            if object
                .parent_node
                .is_some_and(|parent| parent >= self.groups.len())
            {
                return Err(format!(
                    "Scene object {index} references an unknown parent node"
                ));
            }
            if let Some(group_id) = object.model_group {
                match &object.source {
                    SceneObjectSource::Asset { path, .. }
                        if self.groups[group_id].path.as_deref() == Some(path.as_str()) => {}
                    _ => {
                        return Err(format!(
                            "Scene object {index} model does not match its parent group"
                        ));
                    }
                }
            }
            if let Some(parent_node) = object.parent_node {
                let Some(owner) = object.model_group else {
                    return Err(format!(
                        "Scene object {index} has a parent node but no model group"
                    ));
                };
                let mut current = Some(parent_node);
                let mut belongs_to_model = false;
                let mut visited = 0;
                while let Some(node_index) = current {
                    if node_index == owner {
                        belongs_to_model = true;
                        break;
                    }
                    current = self
                        .groups
                        .get(node_index)
                        .ok_or_else(|| {
                            format!("Scene object {index} references an invalid parent hierarchy")
                        })?
                        .parent_group;
                    visited += 1;
                    if visited > self.groups.len() {
                        return Err("Model group hierarchy contains a cycle".to_owned());
                    }
                }
                if !belongs_to_model {
                    return Err(format!(
                        "Scene object {index} parent is outside its model hierarchy"
                    ));
                }
            }
        }
        for (index, group) in self.groups.iter().enumerate() {
            let position = Vec3::from_array(group.position);
            let rotation = Quat::from_array(group.rotation);
            let scale = Vec3::from_array(group.scale);
            if group.name.trim().is_empty()
                || group
                    .path
                    .as_ref()
                    .is_some_and(|path| path.trim().is_empty())
                || (group.path.is_none() && group.animation.is_some())
                || (group.path.is_some() && group.gltf_node_index.is_some())
                || group
                    .parent_group
                    .is_some_and(|parent| parent >= self.groups.len() || parent == index)
                || !position.is_finite()
                || !rotation.is_finite()
                || !rotation.length_squared().is_finite()
                || rotation.length_squared() <= f32::EPSILON
                || !scale.is_finite()
                || group.animation.is_some_and(|animation| {
                    !animation.speed.is_finite() || !(0.1..=4.0).contains(&animation.speed)
                })
            {
                return Err(format!("Model group {index} has invalid data"));
            }
        }
        for start in 0..self.groups.len() {
            let mut visited = HashSet::new();
            let mut current = Some(start);
            while let Some(index) = current {
                if !visited.insert(index) {
                    return Err("Model group hierarchy contains a cycle".to_owned());
                }
                current = self.groups[index].parent_group;
            }
        }
        Ok(())
    }
}

impl SceneRendererSettings {
    fn is_valid(self) -> bool {
        [
            self.key_light_yaw_degrees,
            self.key_light_elevation_degrees,
            self.key_light_intensity,
            self.fill_light_intensity,
            self.environment_intensity,
            self.exposure_ev,
            self.render_scale,
        ]
        .iter()
        .all(|value| value.is_finite())
            && (-180.0..=180.0).contains(&self.key_light_yaw_degrees)
            && (-89.0..=-5.0).contains(&self.key_light_elevation_degrees)
            && (0.0..=8.0).contains(&self.key_light_intensity)
            && (0.0..=4.0).contains(&self.fill_light_intensity)
            && (0.0..=2.0).contains(&self.environment_intensity)
            && (-10.0..=3.0).contains(&self.exposure_ev)
            && (0.5..=1.0).contains(&self.render_scale)
            && self.debug_view <= 5
            && [1, 2, 4, 8].contains(&self.msaa_samples)
    }
}

impl SceneObject {
    pub fn transform(&self) -> Transform {
        Transform::new(
            Vec3::from_array(self.position),
            Quat::from_array(self.rotation).normalize(),
            Vec3::from_array(self.scale),
        )
    }
}

pub fn load_scene(path: &Path) -> Result<LoadedScene, String> {
    let path = fs::canonicalize(path)
        .map_err(|error| format!("Could not open scene {}: {error}", path.display()))?;
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("Could not read scene metadata {}: {error}", path.display()))?;
    if metadata.len() > MAX_SCENE_FILE_BYTES {
        return Err(format!(
            "Scene file exceeds the {} MiB limit",
            MAX_SCENE_FILE_BYTES / (1024 * 1024)
        ));
    }
    let bytes = fs::read(&path)
        .map_err(|error| format!("Could not read scene {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_SCENE_FILE_BYTES {
        return Err(format!(
            "Scene file exceeds the {} MiB limit",
            MAX_SCENE_FILE_BYTES / (1024 * 1024)
        ));
    }
    let mut file: SceneFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Could not parse scene {}: {error}", path.display()))?;
    let legacy_scene = file.version == 1;
    if matches!(file.version, 1..=6) {
        file.version = SCENE_FORMAT_VERSION;
    }
    file.validate()?;

    let scene_directory = path
        .parent()
        .ok_or_else(|| format!("Scene path has no parent directory: {}", path.display()))?;
    if let Some(environment_path) = file
        .environment
        .as_mut()
        .and_then(|environment| environment.path.as_mut())
    {
        let resolved_path = resolve_asset_path(environment_path, scene_directory)
            .map_err(|error| format!("Scene references an unavailable HDR environment: {error}"))?;
        if !resolved_path.is_file() {
            return Err(format!(
                "Scene HDR environment is not a file: {}",
                resolved_path.display()
            ));
        }
        *environment_path = resolved_path
            .to_str()
            .ok_or_else(|| {
                format!(
                    "HDR environment path is not valid UTF-8: {}",
                    resolved_path.display()
                )
            })?
            .to_owned();
    }
    let mut assets = BTreeMap::new();
    let mut referenced_assets = BTreeMap::<PathBuf, ()>::new();
    for (index, group) in file.groups.iter_mut().enumerate() {
        if let Some(path) = group.path.as_mut() {
            let resolved_path = resolve_asset_path(path, scene_directory).map_err(|error| {
                format!("Model group {index} references an unavailable model: {error}")
            })?;
            *path = resolved_path
                .to_str()
                .ok_or_else(|| {
                    format!("Model path is not valid UTF-8: {}", resolved_path.display())
                })?
                .to_owned();
            referenced_assets.insert(resolved_path, ());
        }
    }
    for (index, object) in file.objects.iter_mut().enumerate() {
        let SceneObjectSource::Asset { path, .. } = &mut object.source else {
            continue;
        };
        let resolved_path = resolve_asset_path(path, scene_directory).map_err(|error| {
            format!("Scene object {index} references an unavailable model: {error}")
        })?;
        *path = resolved_path
            .to_str()
            .ok_or_else(|| format!("Model path is not valid UTF-8: {}", resolved_path.display()))?
            .to_owned();
        referenced_assets.insert(resolved_path, ());
    }
    if legacy_scene {
        upgrade_legacy_model_groups(&mut file);
        file.validate()?;
    }
    for (resolved_path, ()) in referenced_assets {
        if assets.len() >= MAX_SCENE_ASSETS {
            return Err(format!(
                "Scene references too many model files (maximum: {MAX_SCENE_ASSETS})"
            ));
        }
        let parsed = parse_asset(&resolved_path)
            .map_err(|error| format!("Could not load scene model: {error}"))?;
        assets.insert(resolved_path, parsed);
    }
    for (index, object) in file.objects.iter().enumerate() {
        let SceneObjectSource::Asset {
            path,
            instance_index,
        } = &object.source
        else {
            continue;
        };
        let resolved_path = PathBuf::from(path);
        let parsed = assets.get(&resolved_path).ok_or_else(|| {
            format!("Scene object {index} references a model that was not loaded")
        })?;
        let count = match parsed {
            ParsedAsset::Gltf(model) => model.instance_count(),
            ParsedAsset::Obj(model) => model.primitive_count(),
        };
        if *instance_index >= count {
            return Err(format!(
                "Scene object {index} references invalid model instance {instance_index}"
            ));
        }
        if let Some(group_id) = object.model_group
            && file.groups[group_id].path.as_deref() != Some(path.as_str())
        {
            return Err(format!(
                "Scene object {index} model does not match its parent group"
            ));
        }
    }

    Ok(LoadedScene { file, assets })
}

fn upgrade_legacy_model_groups(file: &mut SceneFile) {
    let mut paths = Vec::<(String, Vec<usize>)>::new();
    let mut path_indices = HashMap::<String, usize>::new();
    for (object_index, object) in file.objects.iter().enumerate() {
        let SceneObjectSource::Asset { path, .. } = &object.source else {
            continue;
        };
        if object.model_group.is_some() {
            continue;
        }
        let path_index = *path_indices.entry(path.clone()).or_insert_with(|| {
            let index = paths.len();
            paths.push((path.clone(), Vec::new()));
            index
        });
        paths[path_index].1.push(object_index);
    }

    for (path, object_indices) in paths {
        let mut buckets = Vec::<Vec<usize>>::new();
        let mut occurrences = HashMap::<usize, usize>::new();
        for object_index in object_indices {
            let SceneObjectSource::Asset { instance_index, .. } =
                &file.objects[object_index].source
            else {
                continue;
            };
            let occurrence = occurrences.entry(*instance_index).or_default();
            if buckets.len() <= *occurrence {
                buckets.resize_with(*occurrence + 1, Vec::new);
            }
            buckets[*occurrence].push(object_index);
            *occurrence += 1;
        }

        for (occurrence, bucket) in buckets.into_iter().enumerate() {
            let first_object = &file.objects[bucket[0]];
            let root_position = first_object.position;
            let base_name = Path::new(&path)
                .file_stem()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("Model");
            let name = if occurrence == 0 {
                base_name.to_owned()
            } else {
                format!("{base_name} ({})", occurrence + 1)
            };
            let group_id = file.groups.len();
            file.groups.push(SceneGroup {
                name,
                path: Some(path.clone()),
                parent_group: None,
                gltf_node_index: None,
                position: root_position,
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [1.0, 1.0, 1.0],
                animation: None,
            });
            for object_index in bucket {
                let object = &mut file.objects[object_index];
                object.model_group = Some(group_id);
                object.position = [
                    object.position[0] - root_position[0],
                    object.position[1] - root_position[1],
                    object.position[2] - root_position[2],
                ];
            }
        }
    }
}

fn resolve_asset_path(path: &str, scene_directory: &Path) -> Result<PathBuf, String> {
    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        scene_directory.join(path)
    };
    fs::canonicalize(&path).map_err(|error| format!("{}: {error}", path.display()))
}

fn store_scene_path(path: &Path, scene_directory: &Path) -> Result<String, String> {
    let path = fs::canonicalize(path)
        .map_err(|error| format!("Could not resolve scene asset {}: {error}", path.display()))?;
    path.strip_prefix(scene_directory)
        .unwrap_or(&path)
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("Scene asset path is not valid UTF-8: {}", path.display()))
}

impl SceneGroup {
    fn transform(&self) -> Transform {
        Transform::new(
            Vec3::from_array(self.position),
            Quat::from_array(self.rotation).normalize(),
            Vec3::from_array(self.scale),
        )
    }
}

pub fn build_world(
    file: &SceneFile,
    built_in_mesh_count: usize,
    built_in_material_count: usize,
    assets: HashMap<PathBuf, Vec<RenderObject>>,
) -> Result<World, String> {
    file.validate()?;
    let mut world = World::new();
    for group in &file.groups {
        let id = if let Some(path) = &group.path {
            let group_path = PathBuf::from(path);
            if !assets.contains_key(&group_path) {
                return Err(format!(
                    "Model group '{}' references a model that was not loaded",
                    group.name
                ));
            }
            world.add_model_group(group.name.clone(), group_path, group.transform())
        } else {
            world.add_empty_node(group.name.clone(), group.transform())
        };
        if let Some(node_index) = group.gltf_node_index {
            world
                .set_gltf_node_index(id, Some(node_index))
                .map_err(|error| format!("Invalid glTF node metadata: {error}"))?;
        }
    }
    let group_ids = world.model_group_ids().to_vec();
    for (index, group) in file.groups.iter().enumerate() {
        if let Some(parent_index) = group.parent_group {
            let parent_id = *group_ids
                .get(parent_index)
                .ok_or_else(|| format!("Model group {index} references an unknown parent"))?;
            world
                .set_model_group_parent(group_ids[index], Some(parent_id))
                .map_err(|error| format!("Invalid parent for model group {index}: {error}"))?;
        }
    }
    for (index, object) in file.objects.iter().enumerate() {
        let render_object = match &object.source {
            SceneObjectSource::BuiltIn {
                mesh_id,
                material_id,
            } => {
                if *mesh_id >= built_in_mesh_count || *material_id >= built_in_material_count {
                    return Err(format!(
                        "Scene object {index} references an unknown built-in mesh or material"
                    ));
                }
                RenderObject::new(
                    object.transform(),
                    MeshId(*mesh_id),
                    MaterialId(*material_id),
                )
                .with_name(object.name.clone())
            }
            SceneObjectSource::Asset {
                path,
                instance_index,
            } => {
                let path = PathBuf::from(path);
                let instances = assets.get(&path).ok_or_else(|| {
                    format!("Scene object {index} references a model that was not loaded")
                })?;
                let mut render_object = instances
                    .get(*instance_index)
                    .ok_or_else(|| {
                        format!(
                            "Scene object {index} references invalid model instance {instance_index}"
                        )
                    })?
                    .clone();
                render_object.transform = object.transform();
                if !object.name.is_empty() {
                    render_object.name.clone_from(&object.name);
                }
                render_object
            }
        };
        let object_id = if let Some(group_id) = object.model_group {
            world
                .add_group_object(ModelGroupId(group_id), render_object)
                .ok_or_else(|| format!("Scene object {index} references an unknown model group"))?
        } else {
            world.add_object(render_object)
        };
        if let Some(parent_index) = object.parent_node {
            let parent_id = *group_ids
                .get(parent_index)
                .ok_or_else(|| format!("Scene object {index} references an unknown parent node"))?;
            world
                .set_object_parent_node(object_id, Some(parent_id))
                .map_err(|error| format!("Invalid parent for scene object {index}: {error}"))?;
        }
    }
    Ok(world)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{ModelAnimationPlayback, Transform};

    #[test]
    fn saves_and_validates_model_animation_settings() {
        let scene_directory = std::env::temp_dir().join(format!(
            "bone-smashing-animation-scene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scene_directory).unwrap();
        let model_path = scene_directory.join("animated.glb");
        fs::write(&model_path, b"test model path").unwrap();
        let scene_path = scene_directory.join("scene.bse-scene");
        let mut world = World::new();
        let group = world.add_model_group("Animated", model_path, Transform::default());
        world.model_group_mut(group).unwrap().animation = Some(ModelAnimationPlayback {
            clip_index: 2,
            time: 1.25,
            speed: 1.5,
            looping: false,
            playing: true,
        });

        let file = SceneFile::from_world(&world, &scene_path).unwrap();
        let encoded = serde_json::to_vec(&file).unwrap();
        let decoded: SceneFile = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            decoded.groups[0].animation,
            Some(SceneAnimationSettings {
                clip_index: 2,
                speed: 1.5,
                looping: false,
                playing: true,
            })
        );

        let mut invalid = decoded;
        invalid.groups[0].animation.as_mut().unwrap().speed = 4.1;
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .contains("Model group 0 has invalid data")
        );
        fs::remove_dir_all(scene_directory).unwrap();
    }

    #[test]
    fn saves_nested_model_group_hierarchy_and_rejects_cycles() {
        let scene_directory = std::env::temp_dir().join(format!(
            "bone-smashing-nested-scene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scene_directory).unwrap();
        let model_path = scene_directory.join("model.glb");
        fs::write(&model_path, b"model placeholder").unwrap();
        let mut world = World::new();
        let parent = world.add_empty_node("Parent", Transform::default());
        let child = world.add_model_group("Child", model_path.clone(), Transform::default());
        world.set_model_group_parent(child, Some(parent)).unwrap();
        let gltf_node = world.add_empty_node("glTF node", Transform::default());
        world.set_gltf_node_index(gltf_node, Some(4)).unwrap();
        world
            .set_model_group_parent(gltf_node, Some(child))
            .unwrap();
        let object_id = world
            .add_group_object(
                child,
                RenderObject::new(Transform::default(), MeshId(0), MaterialId(0))
                    .with_asset_source(model_path, 0),
            )
            .unwrap();
        world
            .set_object_parent_node(object_id, Some(gltf_node))
            .unwrap();

        let file = SceneFile::from_world(&world, &scene_directory.join("scene.bse-scene")).unwrap();
        assert_eq!(file.version, SCENE_FORMAT_VERSION);
        assert_eq!(file.groups[0].parent_group, None);
        assert_eq!(file.groups[0].path, None);
        assert_eq!(file.groups[1].parent_group, Some(0));
        assert!(file.groups[1].path.is_some());
        assert_eq!(file.groups[2].parent_group, Some(1));
        assert_eq!(file.groups[2].gltf_node_index, Some(4));
        assert_eq!(file.objects[0].parent_node, Some(2));
        let encoded = serde_json::to_vec(&file).unwrap();
        let decoded: SceneFile = serde_json::from_slice(&encoded).unwrap();
        decoded.validate().unwrap();
        let assets = decoded
            .groups
            .iter()
            .filter_map(|group| {
                group.path.as_ref().map(|path| {
                    let path = PathBuf::from(path);
                    (
                        path.clone(),
                        vec![
                            RenderObject::new(Transform::default(), MeshId(0), MaterialId(0))
                                .with_asset_source(path, 0),
                        ],
                    )
                })
            })
            .collect();
        let rebuilt = build_world(&decoded, 1, 1, assets).unwrap();
        let rebuilt_parent = rebuilt.model_group_ids()[0];
        let rebuilt_child = rebuilt.model_group_ids()[1];
        let rebuilt_gltf_node = rebuilt.model_group_ids()[2];
        assert!(
            rebuilt
                .model_group(rebuilt_parent)
                .unwrap()
                .source_path
                .is_none()
        );
        assert_eq!(
            rebuilt.model_group_parent(rebuilt_child),
            Some(rebuilt_parent)
        );
        assert_eq!(rebuilt.gltf_node_index(rebuilt_gltf_node), Some(4));
        assert_eq!(
            rebuilt.model_group_parent(rebuilt_gltf_node),
            Some(rebuilt_child)
        );
        assert_eq!(rebuilt.objects()[0].parent_node(), Some(rebuilt_gltf_node));

        let mut cyclic = decoded;
        cyclic.groups[0].parent_group = Some(1);
        assert!(
            cyclic
                .validate()
                .unwrap_err()
                .contains("hierarchy contains a cycle")
        );
        fs::remove_dir_all(scene_directory).unwrap();
    }

    #[test]
    fn migrates_version_four_scenes_without_parent_fields() {
        let scene_directory = std::env::temp_dir().join(format!(
            "bone-smashing-v4-scene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scene_directory).unwrap();
        let scene_path = scene_directory.join("legacy.bse-scene");
        fs::write(&scene_path, br#"{"version":4,"objects":[]}"#).unwrap();

        let loaded = load_scene(&scene_path).unwrap();

        assert_eq!(loaded.file.version, SCENE_FORMAT_VERSION);
        assert!(loaded.file.groups.is_empty());
        fs::remove_dir_all(scene_directory).unwrap();
    }

    #[test]
    fn migrates_version_five_scenes_with_model_paths() {
        let scene_directory = std::env::temp_dir().join(format!(
            "bone-smashing-v5-scene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scene_directory).unwrap();
        let model_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("models")
            .join("sample.obj");
        fs::copy(model_path, scene_directory.join("sample.obj")).unwrap();
        let scene_path = scene_directory.join("legacy.bse-scene");
        fs::write(
            &scene_path,
            br#"{"version":5,"groups":[{"name":"Model","path":"sample.obj","parent_group":null,"position":[0,0,0],"rotation":[0,0,0,1],"scale":[1,1,1]}],"objects":[]}"#,
        )
        .unwrap();

        let loaded = load_scene(&scene_path).unwrap();

        assert_eq!(loaded.file.version, SCENE_FORMAT_VERSION);
        assert!(
            loaded.file.groups[0]
                .path
                .as_deref()
                .unwrap()
                .ends_with("sample.obj")
        );
        fs::remove_dir_all(scene_directory).unwrap();
    }

    #[test]
    fn saves_and_loads_scene_with_referenced_obj() {
        let model_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("models")
            .join("sample.obj");
        let scene_directory = std::env::temp_dir().join(format!(
            "bone-smashing-scene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scene_directory).unwrap();
        let copied_model_path = scene_directory.join("sample.obj");
        fs::copy(model_path, &copied_model_path).unwrap();
        let scene_path = scene_directory.join("test.bse-scene");
        let mut world = World::new();
        let group = world.add_model_group(
            "Vehicle",
            copied_model_path.clone(),
            Transform::from_translation(Vec3::new(4.0, 0.0, 0.0)),
        );
        world
            .add_group_object(
                group,
                RenderObject::new(
                    Transform::new(
                        Vec3::new(1.0, 2.0, 3.0),
                        Quat::from_rotation_y(0.5),
                        Vec3::splat(2.0),
                    ),
                    MeshId(7),
                    MaterialId(9),
                )
                .with_asset_source(copied_model_path, 0),
            )
            .unwrap();

        let file = SceneFile::from_world(&world, &scene_path).unwrap();
        assert!(matches!(
            &file.objects[0].source,
            SceneObjectSource::Asset { path, .. } if path == "sample.obj"
        ));
        file.write(&scene_path).unwrap();
        let loaded = load_scene(&scene_path).unwrap();
        let resolved_model_path = match &loaded.file.objects[0].source {
            SceneObjectSource::Asset { path, .. } => PathBuf::from(path),
            SceneObjectSource::BuiltIn { .. } => unreachable!(),
        };
        let mut instances = HashMap::new();
        instances.insert(
            resolved_model_path.clone(),
            vec![
                RenderObject::new(Transform::default(), MeshId(20), MaterialId(8))
                    .with_asset_source(resolved_model_path, 0),
            ],
        );
        let rebuilt = build_world(&loaded.file, 3, 6, instances).unwrap();
        let rebuilt_object = &rebuilt.objects()[0];
        let world_position = rebuilt
            .group_model_matrix(rebuilt_object)
            .transform_point3(Vec3::ZERO);
        fs::remove_file(&scene_path).unwrap();
        fs::remove_file(scene_directory.join("sample.obj")).unwrap();
        fs::remove_dir(scene_directory).unwrap();
        assert_eq!(loaded.assets.len(), 1);
        assert_eq!(loaded.file.objects.len(), 1);
        assert_eq!(loaded.file.groups.len(), 1);
        assert_eq!(loaded.file.groups[0].name, "Vehicle");
        assert_eq!(loaded.file.groups[0].position, [4.0, 0.0, 0.0]);
        assert_eq!(loaded.file.objects[0].position, [1.0, 2.0, 3.0]);
        assert_eq!(
            loaded.file.objects[0].model_group,
            Some(0),
            "The part must stay attached to its model group"
        );
        assert_eq!(rebuilt.objects()[0].mesh_id, MeshId(20));
        assert_eq!(rebuilt.objects()[0].material_id, MaterialId(8));
        assert_eq!(
            rebuilt.objects()[0].transform.position,
            Vec3::new(1.0, 2.0, 3.0)
        );
        assert_eq!(world_position, Vec3::new(5.0, 2.0, 3.0));
        assert!(matches!(
            &loaded.file.objects[0].source,
            SceneObjectSource::Asset {
                instance_index: 0,
                path,
            } if Path::new(path).is_absolute()
        ));
    }

    #[test]
    fn saved_group_indices_remain_valid_after_a_group_is_deleted() {
        let scene_directory = std::env::temp_dir().join(format!(
            "bone-smashing-group-ids-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scene_directory).unwrap();
        let model_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("models")
            .join("sample.obj");
        let copied_model_path = scene_directory.join("sample.obj");
        fs::copy(model_path, &copied_model_path).unwrap();

        let mut world = World::new();
        let removed =
            world.add_model_group("Removed", copied_model_path.clone(), Transform::default());
        world.remove_model_group(removed).unwrap();
        let surviving =
            world.add_model_group("Surviving", copied_model_path.clone(), Transform::default());
        world
            .add_group_object(
                surviving,
                RenderObject::new(Transform::default(), MeshId(0), MaterialId(0))
                    .with_asset_source(copied_model_path, 0),
            )
            .unwrap();

        let file = SceneFile::from_world(&world, &scene_directory.join("test.bse-scene")).unwrap();

        assert_eq!(file.groups.len(), 1);
        assert_eq!(file.objects[0].model_group, Some(0));
        fs::remove_dir_all(scene_directory).unwrap();
    }

    #[test]
    fn saves_and_loads_camera_render_material_and_environment_settings() {
        let scene_directory = std::env::temp_dir().join(format!(
            "bone-smashing-editor-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scene_directory).unwrap();
        let environment_path = scene_directory.join("studio.hdr");
        fs::write(&environment_path, b"test environment path").unwrap();
        let scene_path = scene_directory.join("scene.bse-scene");
        let camera = Camera {
            eye: Vec3::new(1.0, 3.0, 5.0),
            yaw: -1.2,
            pitch: -0.25,
            up: Vec3::Y,
            aspect: 16.0 / 9.0,
        };
        let settings = RendererSettings {
            exposure_ev: 1.25,
            render_scale: 0.75,
            ..RendererSettings::default()
        };
        let params = MaterialParams {
            roughness: 0.37,
            metallic: 0.62,
            ..MaterialParams::default()
        };
        let mut world = World::new();
        world.add_object(RenderObject::new(
            Transform::default(),
            MeshId(0),
            MaterialId(0),
        ));

        let mut file = SceneFile::from_world(&world, &scene_path).unwrap();
        file.capture_editor_state(
            &world,
            &scene_path,
            &camera,
            settings,
            Some(&environment_path),
            &[params],
        )
        .unwrap();
        assert_eq!(
            file.environment.as_ref().unwrap().path.as_deref(),
            Some("studio.hdr")
        );
        file.write(&scene_path).unwrap();

        let loaded = load_scene(&scene_path).unwrap();
        let saved_camera = loaded.file.camera.as_ref().unwrap();
        assert_eq!(saved_camera.position, camera.eye.to_array());
        assert_eq!(saved_camera.yaw, camera.yaw);
        assert_eq!(saved_camera.pitch, camera.pitch);
        assert_eq!(
            loaded.file.renderer_settings.unwrap(),
            SceneRendererSettings::from(settings)
        );
        assert_eq!(
            loaded.file.objects[0].material_override.unwrap().roughness,
            0.37
        );
        assert_eq!(
            loaded.file.objects[0].material_override.unwrap().metallic,
            0.62
        );
        assert_eq!(
            loaded.file.environment.unwrap().path.as_deref(),
            Some(
                fs::canonicalize(environment_path)
                    .unwrap()
                    .to_str()
                    .unwrap()
            )
        );
        fs::remove_dir_all(scene_directory).unwrap();
    }

    #[test]
    fn rebuilds_builtin_objects_from_scene_data() {
        let file = SceneFile {
            version: SCENE_FORMAT_VERSION,
            groups: Vec::new(),
            objects: vec![SceneObject {
                source: SceneObjectSource::BuiltIn {
                    mesh_id: 2,
                    material_id: 5,
                },
                model_group: None,
                parent_node: None,
                name: String::new(),
                position: [1.0, 2.0, 3.0],
                rotation: Quat::IDENTITY.to_array(),
                scale: [2.0, 2.0, 2.0],
                material_override: None,
            }],
            camera: None,
            renderer_settings: None,
            environment: None,
        };
        let world = build_world(&file, 3, 6, HashMap::new()).unwrap();

        assert_eq!(world.objects().len(), 1);
        assert_eq!(world.objects()[0].mesh_id, MeshId(2));
        assert_eq!(world.objects()[0].material_id, MaterialId(5));
        assert_eq!(
            world.objects()[0].transform.position,
            Vec3::new(1.0, 2.0, 3.0)
        );
    }

    #[test]
    fn rejects_unsupported_versions_and_non_finite_transforms() {
        let mut file = SceneFile {
            version: SCENE_FORMAT_VERSION + 1,
            groups: Vec::new(),
            objects: Vec::new(),
            camera: None,
            renderer_settings: None,
            environment: None,
        };
        assert!(
            file.validate()
                .unwrap_err()
                .contains("Unsupported scene version")
        );

        file.version = SCENE_FORMAT_VERSION;
        file.objects.push(SceneObject {
            source: SceneObjectSource::BuiltIn {
                mesh_id: 0,
                material_id: 0,
            },
            model_group: None,
            parent_node: None,
            name: String::new(),
            position: [f32::NAN, 0.0, 0.0],
            rotation: Quat::IDENTITY.to_array(),
            scale: Vec3::ONE.to_array(),
            material_override: None,
        });
        assert!(file.validate().unwrap_err().contains("invalid transform"));
    }

    #[test]
    fn rejects_object_with_missing_parent_group() {
        let file = SceneFile {
            version: SCENE_FORMAT_VERSION,
            groups: Vec::new(),
            objects: vec![SceneObject {
                source: SceneObjectSource::BuiltIn {
                    mesh_id: 0,
                    material_id: 0,
                },
                model_group: Some(0),
                parent_node: None,
                name: String::new(),
                position: Vec3::ZERO.to_array(),
                rotation: Quat::IDENTITY.to_array(),
                scale: Vec3::ONE.to_array(),
                material_override: None,
            }],
            camera: None,
            renderer_settings: None,
            environment: None,
        };
        assert!(file.validate().unwrap_err().contains("unknown model group"));
    }

    #[test]
    fn loads_legacy_scene_without_model_groups() {
        let scene_directory = std::env::temp_dir().join(format!(
            "bone-smashing-legacy-scene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scene_directory).unwrap();
        let scene_path = scene_directory.join("legacy.bse-scene");
        fs::write(
            &scene_path,
            r#"{
                "version": 1,
                "objects": [{
                    "source": {"type": "built_in", "mesh_id": 0, "material_id": 0},
                    "position": [1.0, 2.0, 3.0],
                    "rotation": [0.0, 0.0, 0.0, 1.0],
                    "scale": [1.0, 1.0, 1.0]
                }]
            }"#,
        )
        .unwrap();

        let loaded = load_scene(&scene_path).unwrap();
        fs::remove_file(&scene_path).unwrap();
        fs::remove_dir(&scene_directory).unwrap();

        assert_eq!(loaded.file.version, SCENE_FORMAT_VERSION);
        assert!(loaded.file.groups.is_empty());
        assert_eq!(loaded.file.objects.len(), 1);
        assert_eq!(loaded.file.objects[0].model_group, None);
    }

    #[test]
    fn migrates_version_two_scene_without_editor_state() {
        let scene_directory = std::env::temp_dir().join(format!(
            "bone-smashing-v2-scene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scene_directory).unwrap();
        let scene_path = scene_directory.join("version-2.bse-scene");
        fs::write(&scene_path, r#"{"version":2,"groups":[],"objects":[]}"#).unwrap();

        let loaded = load_scene(&scene_path).unwrap();

        assert_eq!(loaded.file.version, SCENE_FORMAT_VERSION);
        assert!(loaded.file.camera.is_none());
        assert!(loaded.file.renderer_settings.is_none());
        assert!(loaded.file.environment.is_none());
        fs::remove_dir_all(scene_directory).unwrap();
    }

    #[test]
    fn migrates_version_three_groups_without_animation_settings() {
        let scene_directory = std::env::temp_dir().join(format!(
            "bone-smashing-v3-scene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scene_directory).unwrap();
        let model_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("models")
            .join("sample.obj");
        fs::copy(model_path, scene_directory.join("sample.obj")).unwrap();
        let scene_path = scene_directory.join("version-3.bse-scene");
        fs::write(
            &scene_path,
            r#"{
                "version": 3,
                "groups": [{
                    "name": "Sample",
                    "path": "sample.obj",
                    "position": [0.0, 0.0, 0.0],
                    "rotation": [0.0, 0.0, 0.0, 1.0],
                    "scale": [1.0, 1.0, 1.0]
                }],
                "objects": []
            }"#,
        )
        .unwrap();

        let loaded = load_scene(&scene_path).unwrap();

        assert_eq!(loaded.file.version, SCENE_FORMAT_VERSION);
        assert_eq!(loaded.file.groups[0].animation, None);
        fs::remove_dir_all(scene_directory).unwrap();
    }

    #[test]
    fn groups_legacy_asset_parts_and_separates_repeated_imports() {
        let scene_directory = std::env::temp_dir().join(format!(
            "bone-smashing-legacy-model-scene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scene_directory).unwrap();
        let scene_path = scene_directory.join("legacy.bse-scene");
        let model_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("models")
            .join("sample.gltf");
        let objects = [
            (0, [4.0, 1.0, 2.0]),
            (1, [4.0, 3.0, 2.0]),
            (0, [10.0, 1.0, 2.0]),
            (1, [10.0, 3.0, 2.0]),
        ]
        .into_iter()
        .map(|(instance_index, position)| {
            serde_json::json!({
                "source": {
                    "type": "asset",
                    "path": model_path,
                    "instance_index": instance_index
                },
                "position": position,
                "rotation": [0.0, 0.0, 0.0, 1.0],
                "scale": [1.0, 1.0, 1.0]
            })
        })
        .collect::<Vec<_>>();
        let legacy_scene = serde_json::json!({
            "version": 1,
            "objects": objects
        });
        fs::write(&scene_path, serde_json::to_vec(&legacy_scene).unwrap()).unwrap();

        let loaded = load_scene(&scene_path).unwrap();
        fs::remove_file(&scene_path).unwrap();
        fs::remove_dir(&scene_directory).unwrap();

        assert_eq!(loaded.file.groups.len(), 2);
        assert_eq!(loaded.file.groups[0].name, "sample");
        assert_eq!(loaded.file.groups[1].name, "sample (2)");
        assert_eq!(loaded.file.groups[0].position, [4.0, 1.0, 2.0]);
        assert_eq!(loaded.file.groups[1].position, [10.0, 1.0, 2.0]);
        assert_eq!(
            loaded
                .file
                .objects
                .iter()
                .map(|object| object.model_group)
                .collect::<Vec<_>>(),
            vec![Some(0), Some(0), Some(1), Some(1)]
        );
        assert_eq!(loaded.file.objects[1].position, [0.0, 2.0, 0.0]);
        assert_eq!(loaded.file.objects[3].position, [0.0, 2.0, 0.0]);
    }
}
