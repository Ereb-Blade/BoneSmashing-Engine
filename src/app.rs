use glam::{Quat, Vec3};
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, ElementState, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorGrabMode, Window, WindowId},
};

use crate::camera::{Camera, CameraController};
use crate::config::{CAMERA_SENSITIVITY, CAMERA_SPEED};
use crate::core::{
    MATERIAL_CHROME, MATERIAL_DEFAULT, MATERIAL_GLASS, MATERIAL_GOLD, MATERIAL_RED_SHINY,
    MATERIAL_SILVER, MESH_CUBE, MESH_PLANE, MESH_SPHERE,
};
use crate::input::Input;
use crate::renderer::debug_ui::{CameraControlMode, CameraLookButton, DebugUi};
use crate::renderer::gltf::{GltfNodeInfo, GltfPrimitiveInstance, LoadedGltfModel};
use crate::renderer::state::{LoadedAsset, ParsedAsset, Renderer, RendererSettings};
use crate::scene::{
    ModelAnimationClip, ModelAnimationPlayback, ModelGroupId, RenderObject, SceneFile, Transform,
    World, build_world,
};

fn advance_animation_playback(playback: &mut ModelAnimationPlayback, duration: f32, dt: f32) {
    if !playback.playing {
        return;
    }
    if duration <= 0.0 {
        playback.time = 0.0;
        playback.playing = false;
        return;
    }
    playback.time += dt * playback.speed;
    if playback.time >= duration {
        if playback.looping {
            playback.time %= duration;
        } else {
            playback.time = duration;
            playback.playing = false;
        }
    }
}

fn create_gltf_node_groups(
    world: &mut World,
    model_group: ModelGroupId,
    nodes: &[GltfNodeInfo],
) -> Result<Vec<ModelGroupId>, String> {
    ensure_gltf_node_groups(world, model_group, nodes)
}

fn ensure_gltf_node_groups(
    world: &mut World,
    model_group: ModelGroupId,
    nodes: &[GltfNodeInfo],
) -> Result<Vec<ModelGroupId>, String> {
    let mut node_ids = vec![None; nodes.len()];
    let mut existing_nodes = vec![false; nodes.len()];
    for (node_index, node_id) in world.gltf_node_groups(model_group) {
        let Some(slot) = node_ids.get_mut(node_index) else {
            return Err(format!(
                "Scene model group references missing glTF node {node_index}"
            ));
        };
        if slot.replace(node_id).is_some() {
            return Err(format!(
                "Scene model group contains duplicate glTF node index {node_index}"
            ));
        }
        existing_nodes[node_index] = true;
    }

    for (index, node) in nodes.iter().enumerate() {
        if node_ids[index].is_none() {
            let id = world.add_empty_node(node.name.clone(), node.transform);
            world
                .set_gltf_node_index(id, Some(index))
                .map_err(|error| format!("Could not tag glTF node {index}: {error}"))?;
            node_ids[index] = Some(id);
        }
    }
    for (index, node) in nodes.iter().enumerate() {
        if existing_nodes[index] {
            continue;
        }
        let parent = match node.parent_index {
            Some(parent_index) => {
                node_ids
                    .get(parent_index)
                    .copied()
                    .flatten()
                    .ok_or_else(|| {
                        format!("glTF node {index} references missing parent {parent_index}")
                    })?
            }
            None => model_group,
        };
        let id = node_ids[index].expect("all glTF nodes were created");
        world
            .set_model_group_parent(id, Some(parent))
            .map_err(|error| format!("Could not build glTF node hierarchy: {error}"))?;
    }
    node_ids
        .into_iter()
        .map(|id| id.ok_or_else(|| "Could not create glTF node".to_owned()))
        .collect()
}

pub struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    input: Input,
    camera: Camera,
    camera_controller: CameraController,
    last_time: Instant,
    world: World,
    animation_models: HashMap<PathBuf, LoadedGltfModel>,
    sampled_animations: HashMap<ModelGroupId, (usize, u32)>,
    animated_instances: Vec<GltfPrimitiveInstance>,
    animated_node_transforms: Vec<(usize, Transform)>,
    debug_ui: Option<DebugUi>,
    fps_frames: u32,
    fps_elapsed: f32,
    displayed_fps: f32,
    displayed_cpu_frame_time: f32,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            input: Input::new(),
            camera: Camera {
                eye: Vec3::new(0.0, 2.0, 6.0),
                yaw: -90.0_f32.to_radians(),
                pitch: -15.0_f32.to_radians(),
                up: Vec3::Y,
                aspect: 1.0,
            },
            camera_controller: CameraController::new(CAMERA_SPEED, CAMERA_SENSITIVITY),
            last_time: Instant::now(),
            world: World::new(),
            animation_models: HashMap::new(),
            sampled_animations: HashMap::new(),
            animated_instances: Vec::new(),
            animated_node_transforms: Vec::new(),
            debug_ui: None,
            fps_frames: 0,
            fps_elapsed: 0.0,
            displayed_fps: 0.0,
            displayed_cpu_frame_time: 0.0,
        }
    }

    fn import_asset(
        renderer: &mut Renderer,
        world: &mut World,
        animation_models: &mut HashMap<PathBuf, LoadedGltfModel>,
        camera: &Camera,
        path: &Path,
        parsed: ParsedAsset,
    ) -> Result<String, String> {
        let position = camera.eye
            + Vec3::new(
                camera.yaw.cos() * camera.pitch.cos(),
                camera.pitch.sin(),
                camera.yaw.sin() * camera.pitch.cos(),
            )
            .normalize()
                * 4.0;

        match renderer.load_parsed_asset(parsed, path)? {
            LoadedAsset::Gltf(model) => {
                let count = model.instances.len();
                let name = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Model");
                let group = world.add_model_group(
                    name,
                    path.to_path_buf(),
                    Transform::from_translation(position),
                );
                let node_ids = create_gltf_node_groups(world, group, &model.node_hierarchy)?;
                if !model.animations.is_empty()
                    && let Some(group) = world.model_group_mut(group)
                {
                    group.animation_clips = model
                        .animations
                        .iter()
                        .map(|animation| ModelAnimationClip {
                            name: animation.name.clone(),
                            duration: animation.duration,
                        })
                        .collect();
                    group.animation = Some(ModelAnimationPlayback::default());
                }
                for (instance_index, instance) in model.instances.iter().enumerate() {
                    let node_id = *node_ids.get(instance.node_index).ok_or_else(|| {
                        format!(
                            "Model instance {instance_index} references missing glTF node {}",
                            instance.node_index
                        )
                    })?;
                    let object_id = world
                        .add_group_object(
                            group,
                            RenderObject::new(
                                Transform::default(),
                                instance.mesh_id,
                                instance.material_id,
                            )
                            .with_name(instance.name.clone())
                            .with_skin_matrices(instance.skin_matrices.clone())
                            .with_morph_weights(instance.morph_weights.clone())
                            .with_morph_target_names(instance.morph_target_names.clone())
                            .with_asset_source(path.to_path_buf(), instance_index),
                        )
                        .ok_or_else(|| {
                            format!("Could not create model group for {}", path.display())
                        })?;
                    world
                        .set_object_parent_node(object_id, Some(node_id))
                        .map_err(|error| {
                            format!("Could not attach model part to its glTF node: {error}")
                        })?;
                }
                animation_models.insert(path.to_path_buf(), model);
                Ok(format!("Imported {count} model parts"))
            }
            LoadedAsset::Obj(model) => {
                let group = world.add_model_group(
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Model"),
                    path.to_path_buf(),
                    Transform::from_translation(position),
                );
                let count = model.instances.len();
                for (instance_index, instance) in model.instances.into_iter().enumerate() {
                    world
                        .add_group_object(
                            group,
                            RenderObject::new(
                                Transform::default(),
                                instance.mesh_id,
                                instance.material_id,
                            )
                            .with_name(instance.name)
                            .with_asset_source(path.to_path_buf(), instance_index),
                        )
                        .ok_or_else(|| {
                            format!("Could not create model group for {}", path.display())
                        })?;
                }
                Ok(format!("Imported OBJ model ({count} parts)"))
            }
        }
    }

    fn restore_scene(
        renderer: &mut Renderer,
        loaded: crate::scene::LoadedScene,
    ) -> Result<(World, SceneFile, HashMap<PathBuf, LoadedGltfModel>), String> {
        let original_mesh_count = renderer.meshes.len();
        let original_material_count = renderer.materials.len();
        let crate::scene::LoadedScene { file, assets } = loaded;
        let result = (|| {
            let mut asset_instances = std::collections::HashMap::with_capacity(assets.len());
            let mut animation_models = HashMap::new();
            for (path, parsed) in assets {
                let instances = match renderer.load_parsed_asset(parsed, &path)? {
                    LoadedAsset::Gltf(model) => {
                        let instances = model
                            .instances
                            .iter()
                            .enumerate()
                            .map(|(index, instance)| {
                                RenderObject::new(
                                    Transform::default(),
                                    instance.mesh_id,
                                    instance.material_id,
                                )
                                .with_name(instance.name.clone())
                                .with_skin_matrices(instance.skin_matrices.clone())
                                .with_morph_weights(instance.morph_weights.clone())
                                .with_morph_target_names(instance.morph_target_names.clone())
                                .with_asset_source(path.clone(), index)
                            })
                            .collect();
                        animation_models.insert(path.clone(), model);
                        instances
                    }
                    LoadedAsset::Obj(model) => model
                        .instances
                        .into_iter()
                        .enumerate()
                        .map(|(index, instance)| {
                            RenderObject::new(
                                Transform::default(),
                                instance.mesh_id,
                                instance.material_id,
                            )
                            .with_name(instance.name)
                            .with_asset_source(path.clone(), index)
                        })
                        .collect(),
                };
                asset_instances.insert(path, instances);
            }
            let mut world = build_world(
                &file,
                crate::core::MESH_SPHERE.0 + 1,
                crate::core::MATERIAL_GLASS.0 + 1,
                asset_instances,
            )?;
            let model_group_ids = world.model_group_ids().to_vec();
            for model_group_id in &model_group_ids {
                let Some(path) = world
                    .model_group(*model_group_id)
                    .and_then(|group| group.source_path.clone())
                else {
                    continue;
                };
                let Some(model) = animation_models.get(&path) else {
                    continue;
                };
                let node_ids =
                    ensure_gltf_node_groups(&mut world, *model_group_id, &model.node_hierarchy)?;
                let object_ids = world
                    .model_group(*model_group_id)
                    .expect("model group must exist")
                    .objects
                    .clone();
                for object_id in object_ids {
                    let Some(asset) = world
                        .get_object(object_id)
                        .and_then(|object| object.asset_source())
                    else {
                        continue;
                    };
                    if asset.path != path
                        || world
                            .get_object(object_id)
                            .is_some_and(|object| object.parent_node().is_some())
                    {
                        continue;
                    }
                    let instance = model.instances.get(asset.instance_index).ok_or_else(|| {
                        format!(
                            "Scene object references invalid model instance {}",
                            asset.instance_index
                        )
                    })?;
                    let parent_node = *node_ids.get(instance.node_index).ok_or_else(|| {
                        format!(
                            "Model instance references missing glTF node {}",
                            instance.node_index
                        )
                    })?;
                    world
                        .set_object_parent_node(object_id, Some(parent_node))
                        .map_err(|error| {
                            format!("Could not restore glTF object hierarchy: {error}")
                        })?;
                }
            }
            let saved_group_ids: Vec<_> = world
                .model_group_ids()
                .iter()
                .take(file.groups.len())
                .copied()
                .collect();
            for (index, group_id) in saved_group_ids.into_iter().enumerate() {
                let group_path = world
                    .model_group(group_id)
                    .ok_or_else(|| format!("Scene model group {index} was not created"))?
                    .source_path
                    .clone();
                let saved_animation = file.groups[index].animation;
                let Some(group_path) = group_path else {
                    continue;
                };
                let Some(model) = animation_models.get(&group_path) else {
                    if saved_animation.is_some() {
                        return Err(format!(
                            "Scene model group '{}' has animation settings but its model has no animation data",
                            file.groups[index].name
                        ));
                    }
                    continue;
                };
                let clips: Vec<_> = model
                    .animations
                    .iter()
                    .map(|animation| ModelAnimationClip {
                        name: animation.name.clone(),
                        duration: animation.duration,
                    })
                    .collect();
                if clips.is_empty() {
                    if saved_animation.is_some() {
                        return Err(format!(
                            "Scene model group '{}' has animation settings but its model has no animation data",
                            file.groups[index].name
                        ));
                    }
                    continue;
                }
                if let Some(saved_animation) = saved_animation
                    && saved_animation.clip_index >= clips.len()
                {
                    return Err(format!(
                        "Scene model group '{}' references a missing animation clip",
                        file.groups[index].name
                    ));
                }
                let mut playback = ModelAnimationPlayback::default();
                if let Some(saved_animation) = saved_animation {
                    playback.clip_index = saved_animation.clip_index;
                    playback.speed = saved_animation.speed;
                    playback.looping = saved_animation.looping;
                    playback.playing = saved_animation.playing;
                }
                let group = world
                    .model_group_mut(group_id)
                    .ok_or_else(|| format!("Scene model group {index} was not created"))?;
                group.animation_clips = clips;
                group.animation = Some(playback);
            }
            renderer.retain_scene_assets(
                world.objects_mut(),
                crate::core::MESH_SPHERE.0 + 1,
                crate::core::MATERIAL_GLASS.0 + 1,
            )?;
            for (saved_object, object) in file.objects.iter().zip(world.objects()) {
                if let Some(material_override) = saved_object.material_override {
                    let mut params =
                        renderer
                            .material_params(object.material_id.0)
                            .ok_or_else(|| {
                                format!(
                                    "Scene object '{}' references a missing material",
                                    saved_object.name
                                )
                            })?;
                    params.roughness = material_override.roughness;
                    params.metallic = material_override.metallic;
                    renderer.set_material_params(object.material_id.0, params);
                }
            }
            Ok((world, file, animation_models))
        })();
        if result.is_err() {
            renderer.discard_assets_after(original_mesh_count, original_material_count);
        }
        result
    }

    fn update_animations(
        world: &mut World,
        animation_models: &mut HashMap<PathBuf, LoadedGltfModel>,
        sampled_animations: &mut HashMap<ModelGroupId, (usize, u32)>,
        animated_instances: &mut Vec<GltfPrimitiveInstance>,
        animated_node_transforms: &mut Vec<(usize, Transform)>,
        dt: f32,
    ) {
        sampled_animations.retain(|group_id, _| world.model_group(*group_id).is_some());
        let group_ids = world.model_group_ids().to_vec();
        for group_id in group_ids {
            let Some((path, clip_index, time, animation_state, should_sample)) = ({
                let Some(group) = world.model_group_mut(group_id) else {
                    continue;
                };
                let Some(path) = group.source_path.clone() else {
                    continue;
                };
                let Some(playback) = group.animation.as_mut() else {
                    continue;
                };
                let Some(clip) = group.animation_clips.get(playback.clip_index) else {
                    eprintln!("Model '{}' references a missing animation clip", group.name);
                    playback.playing = false;
                    continue;
                };
                advance_animation_playback(playback, clip.duration, dt);
                let animation_state = (playback.clip_index, playback.time.to_bits());
                let should_sample =
                    playback.playing || sampled_animations.get(&group_id) != Some(&animation_state);
                Some((
                    path,
                    playback.clip_index,
                    playback.time,
                    animation_state,
                    should_sample,
                ))
            }) else {
                continue;
            };
            if !should_sample {
                continue;
            }
            let Some(model) = animation_models.get_mut(&path) else {
                eprintln!("Animation data for model {} is unavailable", path.display());
                if let Some(group) = world.model_group_mut(group_id)
                    && let Some(playback) = group.animation.as_mut()
                {
                    playback.playing = false;
                }
                continue;
            };
            if let Err(error) = model.sample_animation_with_nodes_into(
                clip_index,
                time,
                animated_instances,
                animated_node_transforms,
            ) {
                eprintln!("Could not update model animation: {error}");
                if let Some(group) = world.model_group_mut(group_id)
                    && let Some(playback) = group.animation.as_mut()
                {
                    playback.playing = false;
                }
                continue;
            }
            sampled_animations.insert(group_id, animation_state);
            let node_groups = world.gltf_node_groups(group_id);
            for &(node_index, transform) in animated_node_transforms.iter() {
                if let Some((_, node_id)) = node_groups
                    .iter()
                    .find(|(candidate_index, _)| *candidate_index == node_index)
                    && let Some(node) = world.model_group_mut(*node_id)
                {
                    node.transform = transform;
                }
            }
            let Some(group) = world.model_group(group_id) else {
                continue;
            };
            let object_ids = group.objects.clone();
            for object_id in object_ids {
                let instance_index = world
                    .get_object(object_id)
                    .and_then(|object| object.asset_source())
                    .filter(|asset| asset.path == path)
                    .map(|asset| asset.instance_index);
                let Some(instance) = instance_index.and_then(|index| animated_instances.get(index))
                else {
                    continue;
                };
                if let Some(object) = world.get_object_mut(object_id) {
                    object.set_skin_matrices(&instance.skin_matrices);
                    object.set_morph_weights(&instance.morph_weights);
                }
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let window = match event_loop
                .create_window(Window::default_attributes().with_title("BoneSmashing Engine"))
            {
                Ok(window) => Arc::new(window),
                Err(error) => {
                    eprintln!("Could not create application window: {error}");
                    event_loop.exit();
                    return;
                }
            };
            let size = window.inner_size();
            if size.width > 0 && size.height > 0 {
                self.camera.aspect = size.width as f32 / size.height as f32;
            }
            self.last_time = Instant::now();
            match pollster::block_on(Renderer::new(window.clone())) {
                Ok(renderer) => {
                    self.debug_ui = Some(DebugUi::new(
                        &window,
                        &renderer.device,
                        renderer.surface_format(),
                    ));
                    self.renderer = Some(renderer);
                    self.window = Some(window.clone());
                }
                Err(error) => {
                    eprintln!("Could not initialize renderer: {error}");
                    event_loop.exit();
                    return;
                }
            }

            self.world.add_object(RenderObject::new(
                Transform::from_translation(Vec3::new(0.0, -1.0, 0.0)),
                MESH_PLANE,
                MATERIAL_DEFAULT,
            ));

            self.world.add_object(RenderObject::new(
                Transform::new(
                    Vec3::new(-2.0, -0.32, 0.0),
                    Quat::IDENTITY,
                    Vec3::splat(0.68),
                ),
                MESH_SPHERE,
                MATERIAL_GOLD,
            ));
            self.world.add_object(RenderObject::new(
                Transform::new(
                    Vec3::new(0.0, -0.32, 0.0),
                    Quat::IDENTITY,
                    Vec3::splat(0.68),
                ),
                MESH_SPHERE,
                MATERIAL_SILVER,
            ));
            self.world.add_object(RenderObject::new(
                Transform::new(
                    Vec3::new(2.0, -0.32, 0.0),
                    Quat::IDENTITY,
                    Vec3::splat(0.68),
                ),
                MESH_SPHERE,
                MATERIAL_GLASS,
            ));

            if std::env::args().any(|argument| argument == "--sample-assets") {
                let rotating_cube_id = self.world.add_object(RenderObject::new(
                    Transform::from_translation(Vec3::new(0.0, 0.0, -2.0)),
                    MESH_CUBE,
                    MATERIAL_DEFAULT,
                ));
                self.world.set_rotating_object(rotating_cube_id);
                self.world.add_object(RenderObject::new(
                    Transform::new(Vec3::new(-3.0, 0.0, -2.0), Quat::IDENTITY, Vec3::splat(0.5)),
                    MESH_CUBE,
                    MATERIAL_RED_SHINY,
                ));
                self.world.add_object(RenderObject::new(
                    Transform::new(
                        Vec3::new(3.0, 0.5, -2.0),
                        Quat::from_rotation_z(45.0_f32.to_radians()),
                        Vec3::new(0.8, 1.8, 0.8),
                    ),
                    MESH_CUBE,
                    MATERIAL_CHROME,
                ));

                let sample_model = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("assets")
                    .join("models")
                    .join("Verity.glb");
                if let Some(renderer) = self.renderer.as_mut() {
                    match renderer.load_gltf(&sample_model) {
                        Ok(model) => {
                            let group = self.world.add_model_group(
                                sample_model
                                    .file_stem()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("Model"),
                                sample_model.clone(),
                                Transform::new(
                                    Vec3::new(0.0, 1.5, -4.0),
                                    Quat::IDENTITY,
                                    Vec3::splat(0.75),
                                ),
                            );
                            match create_gltf_node_groups(
                                &mut self.world,
                                group,
                                &model.node_hierarchy,
                            ) {
                                Ok(node_ids) => {
                                    if !model.animations.is_empty()
                                        && let Some(group) = self.world.model_group_mut(group)
                                    {
                                        group.animation_clips = model
                                            .animations
                                            .iter()
                                            .map(|animation| ModelAnimationClip {
                                                name: animation.name.clone(),
                                                duration: animation.duration,
                                            })
                                            .collect();
                                        group.animation = Some(ModelAnimationPlayback {
                                            playing: true,
                                            ..ModelAnimationPlayback::default()
                                        });
                                    }
                                    for (instance_index, instance) in
                                        model.instances.iter().enumerate()
                                    {
                                        let Some(&node_id) = node_ids.get(instance.node_index)
                                        else {
                                            eprintln!(
                                                "Sample model instance {instance_index} references missing glTF node {}",
                                                instance.node_index
                                            );
                                            continue;
                                        };
                                        let Some(object_id) = self.world.add_group_object(
                                            group,
                                            RenderObject::new(
                                                Transform::default(),
                                                instance.mesh_id,
                                                instance.material_id,
                                            )
                                            .with_name(instance.name.clone())
                                            .with_skin_matrices(instance.skin_matrices.clone())
                                            .with_morph_weights(instance.morph_weights.clone())
                                            .with_morph_target_names(
                                                instance.morph_target_names.clone(),
                                            )
                                            .with_asset_source(
                                                sample_model.clone(),
                                                instance_index,
                                            ),
                                        ) else {
                                            eprintln!("Could not create sample model object");
                                            continue;
                                        };
                                        if let Err(error) = self
                                            .world
                                            .set_object_parent_node(object_id, Some(node_id))
                                        {
                                            eprintln!(
                                                "Could not attach sample model part to its glTF node: {error}"
                                            );
                                        }
                                    }
                                }
                                Err(error) => {
                                    eprintln!("Could not create sample model hierarchy: {error}");
                                }
                            }
                            if let Some(animation) = model.animations.first() {
                                eprintln!("Loaded skeletal animation '{}'", animation.name);
                                self.animation_models.insert(sample_model.clone(), model);
                            }
                        }

                        Err(error) => eprintln!("Не удалось загрузить glTF-модель: {error}"),
                    }

                    let obj_model = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("assets")
                        .join("models")
                        .join("sample.obj");
                    match renderer.load_obj(&obj_model) {
                        Ok(mesh_id) => {
                            let group = self.world.add_model_group(
                                "sample",
                                obj_model.clone(),
                                Transform::new(
                                    Vec3::new(4.0, 0.0, -4.0),
                                    Quat::IDENTITY,
                                    Vec3::splat(0.6),
                                ),
                            );
                            self.world.add_group_object(
                                group,
                                RenderObject::new(Transform::default(), mesh_id, MATERIAL_DEFAULT)
                                    .with_name("sample")
                                    .with_asset_source(obj_model, 0),
                            );
                        }
                        Err(error) => eprintln!("Не удалось загрузить OBJ-модель: {error}"),
                    }
                }
            }
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if self.input.cursor_locked
            && let DeviceEvent::MouseMotion { delta } = event
        {
            self.input.process_mouse_motion(delta);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = &self.window else { return };
        let ui_consumed = self
            .debug_ui
            .as_mut()
            .is_some_and(|debug_ui| debug_ui.on_window_event(window, &event));
        let camera_control_mode = self
            .debug_ui
            .as_ref()
            .map(DebugUi::camera_control_mode)
            .unwrap_or(CameraControlMode::ClickToCapture);
        let camera_look_button = self
            .debug_ui
            .as_ref()
            .map(DebugUi::camera_look_button)
            .unwrap_or(CameraLookButton::Right);
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    renderer.resize(size.width, size.height);
                    self.camera.aspect = size.width as f32 / size.height as f32;
                } else {
                    renderer.resize(0, 0);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let is_pressed = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(keycode) = event.physical_key {
                    if keycode == KeyCode::F1
                        && is_pressed
                        && let Some(debug_ui) = self.debug_ui.as_mut()
                    {
                        debug_ui.toggle();
                    }
                    if keycode == KeyCode::Escape && is_pressed {
                        let _ = window.set_cursor_grab(CursorGrabMode::None);
                        window.set_cursor_visible(true);
                        self.input.cursor_locked = false;
                        self.input.reset_mouse_delta();
                    }
                    if !ui_consumed
                        || keycode == KeyCode::F1
                        || keycode == KeyCode::Escape
                        || !is_pressed
                    {
                        self.input.process_key(keycode, is_pressed);
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let should_capture = match camera_control_mode {
                    CameraControlMode::ClickToCapture => {
                        state == ElementState::Pressed && button == winit::event::MouseButton::Left
                    }
                    CameraControlMode::HoldMouseButton => {
                        button == camera_look_button.mouse_button()
                            && state == ElementState::Pressed
                    }
                };
                let should_release = camera_control_mode == CameraControlMode::HoldMouseButton
                    && button == camera_look_button.mouse_button()
                    && state == ElementState::Released
                    && self.input.cursor_locked;
                if should_release {
                    let _ = window.set_cursor_grab(CursorGrabMode::None);
                    window.set_cursor_visible(true);
                    self.input.cursor_locked = false;
                    self.input.reset_mouse_delta();
                } else if should_capture && !ui_consumed && !self.input.cursor_locked {
                    match window
                        .set_cursor_grab(CursorGrabMode::Confined)
                        .or_else(|_| window.set_cursor_grab(CursorGrabMode::Locked))
                    {
                        Ok(()) => {
                            window.set_cursor_visible(false);
                            self.input.cursor_locked = true;
                            self.input.reset_mouse_delta();
                        }
                        Err(error) => eprintln!("failed to capture cursor: {error}"),
                    }
                }
            }
            WindowEvent::Focused(false) => {
                let _ = window.set_cursor_grab(CursorGrabMode::None);
                window.set_cursor_visible(true);
                self.input.cursor_locked = false;
                self.input.clear();
                self.last_time = Instant::now();
            }
            WindowEvent::RedrawRequested => {
                if !renderer.is_surface_ready() {
                    window.request_redraw();
                    return;
                }
                let now = Instant::now();
                let dt = now.duration_since(self.last_time).as_secs_f32().min(0.1);
                self.last_time = now;
                self.fps_frames += 1;
                self.fps_elapsed += dt;
                if self.fps_elapsed >= 0.25 {
                    self.displayed_fps = self.fps_frames as f32 / self.fps_elapsed;
                    self.fps_frames = 0;
                    self.fps_elapsed = 0.0;
                }

                let camera_before = (self.camera.eye, self.camera.yaw, self.camera.pitch);
                self.camera_controller
                    .update_camera(&mut self.camera, &self.input, dt);
                if camera_before != (self.camera.eye, self.camera.yaw, self.camera.pitch)
                    && let Some(debug_ui) = self.debug_ui.as_mut()
                {
                    debug_ui.mark_scene_dirty();
                }
                self.input.reset_mouse_delta();

                self.world.update(dt);
                App::update_animations(
                    &mut self.world,
                    &mut self.animation_models,
                    &mut self.sampled_animations,
                    &mut self.animated_instances,
                    &mut self.animated_node_transforms,
                    dt,
                );

                if let Some(debug_ui) = self.debug_ui.as_mut() {
                    if let Some(path) = debug_ui.take_scene_save_request() {
                        let result = (|| {
                            let material_params = self
                                .world
                                .objects()
                                .iter()
                                .map(|object| {
                                    renderer.material_params(object.material_id.0).ok_or_else(
                                        || {
                                            format!(
                                                "Object '{}' references a missing material",
                                                object.name
                                            )
                                        },
                                    )
                                })
                                .collect::<Result<Vec<_>, _>>()?;
                            let mut scene = SceneFile::from_world(&self.world, &path)?;
                            scene.capture_editor_state(
                                &self.world,
                                &path,
                                &self.camera,
                                renderer.settings(),
                                renderer.environment_path(),
                                &material_params,
                            )?;
                            scene.write(&path)?;
                            Ok(format!("Saved scene to {}", path.display()))
                        })();
                        debug_ui.scene_save_finished(path, result);
                    }
                    if let Some((path, loaded)) = debug_ui.take_scene_load_request() {
                        match loaded.and_then(|loaded| Self::restore_scene(renderer, loaded)) {
                            Ok((world, file, animation_models)) => {
                                self.world = world;
                                self.animation_models = animation_models;
                                self.sampled_animations.clear();
                                self.animated_instances.clear();
                                if let Some(camera) = &file.camera {
                                    camera.apply_to(&mut self.camera);
                                }
                                let mut status = "Scene loaded".to_owned();
                                if let Some(settings) = file.renderer_settings {
                                    let mut settings: RendererSettings = settings.into();
                                    if !renderer
                                        .supported_msaa_samples()
                                        .contains(&settings.msaa_samples)
                                    {
                                        let requested = settings.msaa_samples;
                                        settings.msaa_samples = renderer
                                            .supported_msaa_samples()
                                            .iter()
                                            .copied()
                                            .filter(|&samples| samples < requested)
                                            .max()
                                            .or_else(|| {
                                                renderer.supported_msaa_samples().first().copied()
                                            })
                                            .unwrap_or(1);
                                        status = format!(
                                            "Scene loaded; MSAA {requested}x unavailable, using {}x",
                                            settings.msaa_samples
                                        );
                                    }
                                    renderer.set_settings(settings);
                                }
                                debug_ui.scene_loaded(path, file.environment.clone());
                                debug_ui.set_scene_status(Ok(status));
                            }
                            Err(error) => debug_ui.set_scene_status(Err(error)),
                        }
                    }
                    if let Some((path, parsed)) = debug_ui.take_asset_request() {
                        let result = match parsed {
                            Ok(parsed) => {
                                let snapshot = self.world.clone();
                                let result = Self::import_asset(
                                    renderer,
                                    &mut self.world,
                                    &mut self.animation_models,
                                    &self.camera,
                                    &path,
                                    parsed,
                                );
                                if result.is_ok() {
                                    debug_ui.record_undo_snapshot(snapshot, renderer);
                                    debug_ui.mark_scene_dirty();
                                }
                                result
                            }
                            Err(error) => Err(error),
                        };
                        debug_ui.set_asset_status(result);
                    }
                    let render_started = Instant::now();
                    renderer.render(
                        &self.camera,
                        &mut self.world,
                        debug_ui,
                        window,
                        dt,
                        self.displayed_fps,
                        self.displayed_cpu_frame_time,
                    );
                    self.displayed_cpu_frame_time = render_started.elapsed().as_secs_f32() * 1000.0;
                }
                window.request_redraw();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looping_animation_wraps_to_the_start() {
        let mut playback = ModelAnimationPlayback {
            time: 0.9,
            speed: 2.0,
            looping: true,
            playing: true,
            ..ModelAnimationPlayback::default()
        };

        advance_animation_playback(&mut playback, 1.0, 0.2);

        assert!((playback.time - 0.3).abs() < 0.0001);
        assert!(playback.playing);
    }

    #[test]
    fn non_looping_animation_stops_at_its_end() {
        let mut playback = ModelAnimationPlayback {
            time: 0.9,
            looping: false,
            playing: true,
            ..ModelAnimationPlayback::default()
        };

        advance_animation_playback(&mut playback, 1.0, 0.2);

        assert_eq!(playback.time, 1.0);
        assert!(!playback.playing);
    }

    #[test]
    fn animation_updates_imported_node_transforms() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("models")
            .join("Verity.glb");
        let mut model = crate::renderer::gltf::GltfModelData::load(&path)
            .unwrap()
            .into_loaded(0, 0)
            .unwrap();
        let animation = model.animations[0].clone();
        let node_hierarchy = model.node_hierarchy.clone();
        let mut world = World::new();
        let model_group = world.add_model_group("Model", path.clone(), Transform::default());
        let node_ids = create_gltf_node_groups(&mut world, model_group, &node_hierarchy).unwrap();

        let animation_time = animation.duration * 0.5;
        if let Some(group) = world.model_group_mut(model_group) {
            group.animation_clips = vec![ModelAnimationClip {
                name: animation.name,
                duration: animation.duration,
            }];
            group.animation = Some(ModelAnimationPlayback {
                time: animation_time,
                playing: false,
                ..ModelAnimationPlayback::default()
            });
        }

        let mut expected = Vec::new();
        model
            .sample_animation_with_nodes_into(0, animation_time, &mut Vec::new(), &mut expected)
            .unwrap();
        assert!(expected.iter().any(|(index, transform)| {
            let base = node_hierarchy[*index].transform.to_matrix();
            !base.abs_diff_eq(transform.to_matrix(), 1e-5)
        }));

        let mut animation_models = HashMap::from([(path, model)]);
        let mut sampled_animations = HashMap::new();
        App::update_animations(
            &mut world,
            &mut animation_models,
            &mut sampled_animations,
            &mut Vec::new(),
            &mut Vec::new(),
            0.0,
        );

        for &(node_index, expected_transform) in &expected {
            let actual = world.model_group(node_ids[node_index]).unwrap();
            assert!(
                actual
                    .transform
                    .to_matrix()
                    .abs_diff_eq(expected_transform.to_matrix(), 1e-5)
            );
        }

        let edited_node = expected[0].0;
        let edited_transform = Transform::from_translation(Vec3::splat(3.0));
        world
            .model_group_mut(node_ids[edited_node])
            .unwrap()
            .transform = edited_transform;
        App::update_animations(
            &mut world,
            &mut animation_models,
            &mut sampled_animations,
            &mut Vec::new(),
            &mut Vec::new(),
            0.0,
        );
        assert!(
            world
                .model_group(node_ids[edited_node])
                .unwrap()
                .transform
                .to_matrix()
                .abs_diff_eq(edited_transform.to_matrix(), 1e-5)
        );
    }
}
