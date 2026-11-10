use crate::core::{MaterialId, MeshId};
use crate::renderer::state::{ParsedAsset, parse_asset};
use crate::scene::{ModelGroupId, RenderObject, Transform, World};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

const SCENE_FORMAT_VERSION: u32 = 2;
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneGroup {
    pub name: String,
    pub path: String,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneObject {
    pub source: SceneObjectSource,
    #[serde(default)]
    pub model_group: Option<usize>,
    #[serde(default)]
    pub name: String,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
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
        let mut stored_asset_paths =
            HashMap::with_capacity(world.objects().len().min(MAX_SCENE_ASSETS));
        for group in world.model_groups() {
            if !stored_asset_paths.contains_key(&group.source_path) {
                let path = fs::canonicalize(&group.source_path).map_err(|error| {
                    format!(
                        "Could not resolve model path {}: {error}",
                        group.source_path.display()
                    )
                })?;
                let stored_path = path
                    .strip_prefix(&scene_directory)
                    .unwrap_or(&path)
                    .to_str()
                    .ok_or_else(|| format!("Model path is not valid UTF-8: {}", path.display()))?
                    .to_owned();
                stored_asset_paths.insert(group.source_path.clone(), stored_path);
            }
            groups.push(SceneGroup {
                name: group.name.clone(),
                path: stored_asset_paths
                    .get(&group.source_path)
                    .ok_or_else(|| "Model path was not prepared for saving".to_owned())?
                    .clone(),
                position: group.transform.position.to_array(),
                rotation: group.transform.rotation.to_array(),
                scale: group.transform.scale.to_array(),
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
            objects.push(SceneObject {
                source,
                model_group: object.model_group().map(|id| id.0),
                name: object.name.clone(),
                position: object.transform.position.to_array(),
                rotation: object.transform.rotation.to_array(),
                scale: object.transform.scale.to_array(),
            });
        }
        let file = Self {
            version: SCENE_FORMAT_VERSION,
            groups,
            objects,
        };
        file.validate()?;
        Ok(file)
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
            if let Some(group_id) = object.model_group {
                match &object.source {
                    SceneObjectSource::Asset { path, .. }
                        if path == &self.groups[group_id].path => {}
                    _ => {
                        return Err(format!(
                            "Scene object {index} model does not match its parent group"
                        ));
                    }
                }
            }
        }
        for (index, group) in self.groups.iter().enumerate() {
            let position = Vec3::from_array(group.position);
            let rotation = Quat::from_array(group.rotation);
            let scale = Vec3::from_array(group.scale);
            if group.name.trim().is_empty()
                || group.path.trim().is_empty()
                || !position.is_finite()
                || !rotation.is_finite()
                || !rotation.length_squared().is_finite()
                || rotation.length_squared() <= f32::EPSILON
                || !scale.is_finite()
            {
                return Err(format!("Model group {index} has invalid data"));
            }
        }
        Ok(())
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
    if legacy_scene {
        file.version = SCENE_FORMAT_VERSION;
    }
    file.validate()?;

    let scene_directory = path
        .parent()
        .ok_or_else(|| format!("Scene path has no parent directory: {}", path.display()))?;
    let mut assets = BTreeMap::new();
    let mut referenced_assets = BTreeMap::<PathBuf, ()>::new();
    for (index, group) in file.groups.iter_mut().enumerate() {
        let resolved_path = resolve_asset_path(&group.path, scene_directory).map_err(|error| {
            format!("Model group {index} references an unavailable model: {error}")
        })?;
        group.path = resolved_path
            .to_str()
            .ok_or_else(|| format!("Model path is not valid UTF-8: {}", resolved_path.display()))?
            .to_owned();
        referenced_assets.insert(resolved_path, ());
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
            ParsedAsset::Obj(_) => 1,
        };
        if *instance_index >= count {
            return Err(format!(
                "Scene object {index} references invalid model instance {instance_index}"
            ));
        }
        if let Some(group_id) = object.model_group {
            if file.groups[group_id].path != *path {
                return Err(format!(
                    "Scene object {index} model does not match its parent group"
                ));
            }
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
                path: path.clone(),
                position: root_position,
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [1.0, 1.0, 1.0],
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
        let group_path = PathBuf::from(&group.path);
        if !assets.contains_key(&group_path) {
            return Err(format!(
                "Model group '{}' references a model that was not loaded",
                group.name
            ));
        }
        world.add_model_group(group.name.clone(), group_path, group.transform());
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
        if let Some(group_id) = object.model_group {
            let group_id = ModelGroupId(group_id);
            if world.add_group_object(group_id, render_object).is_none() {
                return Err(format!(
                    "Scene object {index} references an unknown model group"
                ));
            }
        } else {
            world.add_object(render_object);
        }
    }
    Ok(world)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Transform;

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
                name: String::new(),
                position: [1.0, 2.0, 3.0],
                rotation: Quat::IDENTITY.to_array(),
                scale: [2.0, 2.0, 2.0],
            }],
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
            name: String::new(),
            position: [f32::NAN, 0.0, 0.0],
            rotation: Quat::IDENTITY.to_array(),
            scale: Vec3::ONE.to_array(),
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
                name: String::new(),
                position: Vec3::ZERO.to_array(),
                rotation: Quat::IDENTITY.to_array(),
                scale: Vec3::ONE.to_array(),
            }],
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
