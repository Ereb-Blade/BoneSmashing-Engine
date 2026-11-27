use glam::{Quat, Vec3};
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{DeviceEvent, DeviceId, ElementState, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorGrabMode, Fullscreen, Window, WindowId},
};

use crate::camera::{Camera, CameraController};
use crate::config::{CAMERA_SENSITIVITY, CAMERA_SPEED};
use crate::core::{
    MATERIAL_CHROME, MATERIAL_DEFAULT, MATERIAL_GLASS, MATERIAL_GOLD, MATERIAL_RED_SHINY,
    MATERIAL_SILVER, MESH_CUBE, MESH_PLANE, MESH_SPHERE,
};
use crate::input::Input;
use crate::renderer::debug_ui::{CameraControlMode, CameraLookButton, DebugUi};
use crate::renderer::gltf::{
    AnimationClipSample, GltfNodeInfo, GltfPrimitiveInstance, LoadedGltfModel,
};
use crate::renderer::state::{LoadedAsset, ParsedAsset, Renderer, RendererSettings};
use crate::scene::{
    AnimationKeyboardControl, ModelAnimationClip, ModelAnimationPlayback, ModelAnimationTransition,
    ModelGroupId, RenderObject, SceneFile, Transform, World, build_world,
};

#[derive(Clone, Debug)]
pub struct BenchmarkConfig {
    pub scene_path: PathBuf,
    pub object_count: Option<usize>,
    pub runs: u32,
    pub width: u32,
    pub height: u32,
    pub render_scale: Option<f32>,
    pub shadows_enabled: Option<bool>,
    pub shadow_pcf_samples: Option<u32>,
    pub msaa_samples: Option<u32>,
    pub fullscreen: bool,
}

impl BenchmarkConfig {
    pub fn from_args(mut args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut benchmark = false;
        let mut scene_path = None;
        let mut object_count = None;
        let mut runs = 1;
        let mut width = 1280;
        let mut height = 720;
        let mut render_scale = None;
        let mut shadows_enabled = None;
        let mut shadow_pcf_samples = None;
        let mut msaa_samples = None;
        let mut fullscreen = false;

        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--benchmark" => benchmark = true,
                "--benchmark-size" => {
                    benchmark = true;
                    let value = args
                        .next()
                        .ok_or_else(|| "--benchmark-size requires WIDTHxHEIGHT".to_owned())?;
                    let (w, h) = value
                        .split_once('x')
                        .ok_or_else(|| "--benchmark-size must use WIDTHxHEIGHT".to_owned())?;
                    width = w.parse().map_err(|_| {
                        "--benchmark-size width must be a positive integer".to_owned()
                    })?;
                    height = h.parse().map_err(|_| {
                        "--benchmark-size height must be a positive integer".to_owned()
                    })?;
                    if width == 0 || height == 0 {
                        return Err("--benchmark-size dimensions must be positive".to_owned());
                    }
                }
                "--benchmark-scene" => {
                    benchmark = true;
                    let value = args
                        .next()
                        .ok_or_else(|| "--benchmark-scene requires a scene path".to_owned())?;
                    scene_path = Some(PathBuf::from(value));
                }
                "--benchmark-objects" => {
                    benchmark = true;
                    let value = args.next().ok_or_else(|| {
                        "--benchmark-objects requires 1000, 10000, or 100000".to_owned()
                    })?;
                    let count = value.parse::<usize>().map_err(|_| {
                        "--benchmark-objects requires 1000, 10000, or 100000".to_owned()
                    })?;
                    if ![1_000, 10_000, 100_000].contains(&count) {
                        return Err(
                            "--benchmark-objects requires 1000, 10000, or 100000".to_owned()
                        );
                    }
                    object_count = Some(count);
                }
                "--benchmark-runs" => {
                    benchmark = true;
                    let value = args.next().ok_or_else(|| {
                        "--benchmark-runs requires a value from 1 to 10".to_owned()
                    })?;
                    runs = value
                        .parse::<u32>()
                        .map_err(|_| "--benchmark-runs requires a value from 1 to 10".to_owned())?;
                    if !(1..=10).contains(&runs) {
                        return Err("--benchmark-runs requires a value from 1 to 10".to_owned());
                    }
                }
                "--benchmark-render-scale" => {
                    benchmark = true;
                    let value = args.next().ok_or_else(|| {
                        "--benchmark-render-scale requires a value from 0.5 to 1.0".to_owned()
                    })?;
                    let scale = value.parse::<f32>().map_err(|_| {
                        "--benchmark-render-scale must be between 0.5 and 1.0".to_owned()
                    })?;
                    if !scale.is_finite() || !(0.5..=1.0).contains(&scale) {
                        return Err(
                            "--benchmark-render-scale must be between 0.5 and 1.0".to_owned()
                        );
                    }
                    render_scale = Some(scale);
                }
                "--benchmark-shadows" => {
                    benchmark = true;
                    shadows_enabled = Some(true);
                }
                "--benchmark-shadow-pcf-samples" => {
                    benchmark = true;
                    let value = args.next().ok_or_else(|| {
                        "--benchmark-shadow-pcf-samples requires 2, 4, 8, or 16".to_owned()
                    })?;
                    let samples = value.parse::<u32>().map_err(|_| {
                        "--benchmark-shadow-pcf-samples must be 2, 4, 8, or 16".to_owned()
                    })?;
                    if ![2, 4, 8, 16].contains(&samples) {
                        return Err(
                            "--benchmark-shadow-pcf-samples must be 2, 4, 8, or 16".to_owned()
                        );
                    }
                    shadow_pcf_samples = Some(samples);
                }
                "--benchmark-msaa-samples" => {
                    benchmark = true;
                    let value = args.next().ok_or_else(|| {
                        "--benchmark-msaa-samples requires 1, 2, 4, or 8".to_owned()
                    })?;
                    let samples = value
                        .parse::<u32>()
                        .map_err(|_| "--benchmark-msaa-samples must be 1, 2, 4, or 8".to_owned())?;
                    if ![1, 2, 4, 8].contains(&samples) {
                        return Err("--benchmark-msaa-samples must be 1, 2, 4, or 8".to_owned());
                    }
                    msaa_samples = Some(samples);
                }
                "--benchmark-fullscreen" => {
                    benchmark = true;
                    fullscreen = true;
                }
                other if !other.starts_with('-') && benchmark && scene_path.is_none() => {
                    scene_path = Some(PathBuf::from(other));
                }
                other if other.starts_with("--") => {
                    return Err(format!("Unknown benchmark option: {other}"));
                }
                _ => {}
            }
        }

        if !benchmark {
            return Ok(None);
        }
        if object_count.is_some() && scene_path.is_some() {
            return Err("--benchmark-objects cannot be combined with --benchmark-scene".to_owned());
        }
        let scene_path = scene_path.unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets")
                .join("scenes")
                .join("benchmark.bse-scene")
        });
        Ok(Some(Self {
            scene_path,
            object_count,
            runs,
            width,
            height,
            render_scale,
            shadows_enabled,
            shadow_pcf_samples,
            msaa_samples,
            fullscreen,
        }))
    }
}

fn build_object_benchmark_world(object_count: usize) -> World {
    let mut world = World::new();
    let columns = 100;
    let spacing = 0.75;
    for index in 0..object_count {
        let column = index % columns;
        let row = index / columns;
        let position = Vec3::new(
            column as f32 * spacing - (columns as f32 - 1.0) * spacing * 0.5,
            0.0,
            -(row as f32 * spacing + 3.0),
        );
        world.add_object(RenderObject::new(
            Transform::new(position, Quat::IDENTITY, Vec3::splat(0.25)),
            MESH_CUBE,
            MATERIAL_DEFAULT,
        ));
    }
    world
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnimationSampleState {
    clip_index: usize,
    time_bits: u32,
    transition: Option<(usize, u32, u32)>,
}

fn advance_clip_time(
    time: &mut f32,
    playing: &mut bool,
    speed: f32,
    looping: bool,
    duration: f32,
    dt: f32,
) {
    if !*playing {
        return;
    }
    if duration <= 0.0 {
        *time = 0.0;
        *playing = false;
        return;
    }
    *time += dt * speed;
    if *time >= duration {
        if looping {
            *time %= duration;
        } else {
            *time = duration;
            *playing = false;
        }
    }
}

fn advance_animation_playback(
    playback: &mut ModelAnimationPlayback,
    clips: &[ModelAnimationClip],
    dt: f32,
) {
    let Some(clip) = clips.get(playback.clip_index) else {
        playback.playing = false;
        playback.transition = None;
        return;
    };
    let was_playing = playback.playing;
    advance_clip_time(
        &mut playback.time,
        &mut playback.playing,
        playback.speed,
        playback.looping,
        clip.duration,
        dt,
    );

    let Some(mut transition) = playback.transition else {
        return;
    };
    if was_playing && let Some(from_clip) = clips.get(transition.from_clip_index) {
        let mut from_playing = true;
        advance_clip_time(
            &mut transition.from_time,
            &mut from_playing,
            playback.speed,
            playback.looping,
            from_clip.duration,
            dt,
        );
    }
    transition.elapsed = (transition.elapsed + dt).min(transition.duration);
    playback.transition = (transition.duration > 0.0 && transition.elapsed < transition.duration)
        .then_some(transition);
}

fn animation_sample_state(playback: ModelAnimationPlayback) -> AnimationSampleState {
    let transition = playback.transition.map(|transition| {
        (
            transition.from_clip_index,
            transition.from_time.to_bits(),
            ModelAnimationTransition::progress(transition).to_bits(),
        )
    });
    AnimationSampleState {
        clip_index: playback.clip_index,
        time_bits: playback.time.to_bits(),
        transition,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyboardAnimationControllerState {
    group_id: ModelGroupId,
    active: bool,
}

fn animation_controller_target_clip(
    settings: AnimationKeyboardControl,
    walk_pressed: bool,
    run_pressed: bool,
    controller_active: bool,
) -> Option<usize> {
    if run_pressed {
        Some(settings.run_clip_index)
    } else if walk_pressed {
        Some(settings.walk_clip_index)
    } else if controller_active {
        Some(settings.idle_clip_index)
    } else {
        None
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
    benchmark: Option<BenchmarkConfig>,
    benchmark_capture_started: bool,
    benchmark_run_index: u32,
    benchmark_finished: bool,
    input: Input,
    camera: Camera,
    camera_controller: CameraController,
    last_time: Instant,
    world: World,
    animation_models: HashMap<PathBuf, LoadedGltfModel>,
    sampled_animations: HashMap<ModelGroupId, AnimationSampleState>,
    animated_instances: Vec<GltfPrimitiveInstance>,
    animated_node_transforms: Vec<(usize, Transform)>,
    debug_ui: Option<DebugUi>,
    keyboard_animation_controller: Option<KeyboardAnimationControllerState>,
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
            benchmark: None,
            benchmark_capture_started: false,
            benchmark_run_index: 0,
            benchmark_finished: false,
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
            keyboard_animation_controller: None,
            fps_frames: 0,
            fps_elapsed: 0.0,
            displayed_fps: 0.0,
            displayed_cpu_frame_time: 0.0,
        }
    }

    pub fn with_benchmark(benchmark: BenchmarkConfig) -> Self {
        let mut app = Self::new();
        app.benchmark = Some(benchmark);
        app
    }

    pub fn benchmark_completed(&self) -> bool {
        self.benchmark.is_none() || self.benchmark_finished
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
                    group.animation = Some(ModelAnimationPlayback {
                        keyboard_control: AnimationKeyboardControl::for_clips(
                            &group.animation_clips,
                        ),
                        ..ModelAnimationPlayback::default()
                    });
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
                let mut playback = ModelAnimationPlayback {
                    keyboard_control: AnimationKeyboardControl::for_clips(&clips),
                    ..ModelAnimationPlayback::default()
                };
                if let Some(saved_animation) = saved_animation {
                    playback.clip_index = saved_animation.clip_index;
                    playback.speed = saved_animation.speed;
                    playback.looping = saved_animation.looping;
                    playback.playing = saved_animation.playing;
                    playback.crossfade_duration = saved_animation.crossfade_duration;
                    if let Some(keyboard_control) = saved_animation.keyboard_control {
                        if [
                            keyboard_control.idle_clip_index,
                            keyboard_control.walk_clip_index,
                            keyboard_control.run_clip_index,
                        ]
                        .iter()
                        .any(|clip_index| *clip_index >= clips.len())
                        {
                            return Err(format!(
                                "Scene model group '{}' keyboard controller references a missing animation clip",
                                file.groups[index].name
                            ));
                        }
                        playback.keyboard_control = keyboard_control;
                    }
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
        sampled_animations: &mut HashMap<ModelGroupId, AnimationSampleState>,
        animated_instances: &mut Vec<GltfPrimitiveInstance>,
        animated_node_transforms: &mut Vec<(usize, Transform)>,
        dt: f32,
    ) {
        sampled_animations.retain(|group_id, _| world.model_group(*group_id).is_some());
        let group_ids = world.model_group_ids().to_vec();
        for group_id in group_ids {
            let Some((path, clip_index, time, transition, animation_state, should_sample)) = ({
                let Some(group) = world.model_group_mut(group_id) else {
                    continue;
                };
                let Some(path) = group.source_path.clone() else {
                    continue;
                };
                let Some(playback) = group.animation.as_mut() else {
                    continue;
                };
                if group.animation_clips.get(playback.clip_index).is_none() {
                    eprintln!("Model '{}' references a missing animation clip", group.name);
                    playback.playing = false;
                    playback.transition = None;
                    continue;
                }

                advance_animation_playback(playback, &group.animation_clips, dt);
                let transition = playback.transition.map(|transition| {
                    (
                        transition.from_clip_index,
                        transition.from_time,
                        ModelAnimationTransition::progress(transition),
                    )
                });
                let animation_state = animation_sample_state(*playback);
                let should_sample = playback.playing
                    || transition.is_some()
                    || sampled_animations.get(&group_id) != Some(&animation_state);
                Some((
                    path,
                    playback.clip_index,
                    playback.time,
                    transition,
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
            let sample_result = if let Some((from_clip_index, from_time, weight)) = transition {
                model.sample_blended_animations_with_nodes_into(
                    AnimationClipSample {
                        clip_index: from_clip_index,
                        time: from_time,
                    },
                    AnimationClipSample { clip_index, time },
                    weight,
                    animated_instances,
                    animated_node_transforms,
                )
            } else {
                model.sample_animation_with_nodes_into(
                    clip_index,
                    time,
                    animated_instances,
                    animated_node_transforms,
                )
            };
            if let Err(error) = sample_result {
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

    fn update_keyboard_animation_controller(
        controller: &mut Option<KeyboardAnimationControllerState>,
        input: &Input,
        world: &mut World,
        debug_ui: Option<&mut DebugUi>,
    ) -> bool {
        let group_id = debug_ui
            .as_deref()
            .and_then(|debug_ui| debug_ui.animation_controller_target(world));
        let Some(group_id) = group_id else {
            *controller = None;
            return false;
        };
        let Some(group) = world.model_group(group_id) else {
            *controller = None;
            return false;
        };
        let Some(playback) = group.animation else {
            *controller = None;
            return false;
        };
        let settings = playback.keyboard_control;
        if !settings.enabled
            || [
                settings.idle_clip_index,
                settings.walk_clip_index,
                settings.run_clip_index,
            ]
            .iter()
            .any(|clip_index| *clip_index >= group.animation_clips.len())
        {
            *controller = None;
            return false;
        }

        let controller_active =
            controller.is_some_and(|state| state.group_id == group_id && state.active);
        let walk_pressed = input.is_key_pressed(KeyCode::KeyW);
        let run_pressed =
            input.is_key_pressed(KeyCode::ShiftLeft) || input.is_key_pressed(KeyCode::ShiftRight);
        let movement_key_pressed = walk_pressed || run_pressed;
        let target_clip = animation_controller_target_clip(
            settings,
            walk_pressed,
            run_pressed,
            controller_active,
        );
        let Some(target_clip) = target_clip else {
            *controller = Some(KeyboardAnimationControllerState {
                group_id,
                active: false,
            });
            return false;
        };

        let mut active = controller_active || movement_key_pressed;
        let mut scene_changed = false;
        if let Some(group) = world.model_group_mut(group_id)
            && let Some(playback) = group.animation.as_mut()
        {
            if movement_key_pressed && !playback.playing {
                playback.playing = true;
                scene_changed = true;
            }
            scene_changed |= playback.switch_to_clip(target_clip);
            if !movement_key_pressed
                && target_clip == settings.idle_clip_index
                && playback.clip_index == settings.idle_clip_index
                && playback.transition.is_none()
            {
                active = false;
            }
        } else {
            active = false;
        }
        if scene_changed && let Some(debug_ui) = debug_ui {
            debug_ui.mark_scene_dirty();
        }
        *controller = Some(KeyboardAnimationControllerState { group_id, active });
        active
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let mut window_attributes =
                Window::default_attributes().with_title("BoneSmashing Engine");
            if let Some(benchmark) = &self.benchmark {
                if benchmark.fullscreen {
                    window_attributes = window_attributes
                        .with_decorations(false)
                        .with_fullscreen(Some(Fullscreen::Borderless(None)));
                } else {
                    window_attributes = window_attributes
                        .with_inner_size(PhysicalSize::new(benchmark.width, benchmark.height));
                }
            }
            let window = match event_loop.create_window(window_attributes) {
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

            if let (Some(benchmark), Some(renderer), Some(debug_ui)) = (
                self.benchmark.clone(),
                self.renderer.as_mut(),
                self.debug_ui.as_mut(),
            ) && benchmark.object_count.is_none()
            {
                match crate::scene::load_scene(&benchmark.scene_path)
                    .and_then(|loaded| Self::restore_scene(renderer, loaded))
                {
                    Ok((world, file, animation_models)) => {
                        self.world = world;
                        self.animation_models = animation_models;
                        self.sampled_animations.clear();
                        self.keyboard_animation_controller = None;
                        self.animated_instances.clear();
                        if let Some(camera) = &file.camera {
                            camera.apply_to(&mut self.camera);
                        }
                        if let Some(settings) = file.renderer_settings {
                            let mut settings: RendererSettings = settings.into();
                            if let Some(render_scale) = benchmark.render_scale {
                                settings.render_scale = render_scale;
                            }
                            if let Some(shadows_enabled) = benchmark.shadows_enabled {
                                settings.shadows_enabled = shadows_enabled;
                            }
                            if let Some(shadow_pcf_samples) = benchmark.shadow_pcf_samples {
                                settings.shadow_pcf_samples = shadow_pcf_samples;
                            }
                            if let Some(msaa_samples) = benchmark.msaa_samples {
                                settings.msaa_samples = msaa_samples;
                            }
                            renderer.set_settings(settings);
                            if let Some(requested) = benchmark.msaa_samples
                                && renderer.settings().msaa_samples != requested
                            {
                                eprintln!(
                                    "Requested benchmark MSAA {requested}x is unsupported; using {}x",
                                    renderer.settings().msaa_samples
                                );
                            }
                        } else if benchmark.render_scale.is_some()
                            || benchmark.shadows_enabled.is_some()
                            || benchmark.shadow_pcf_samples.is_some()
                            || benchmark.msaa_samples.is_some()
                        {
                            let mut settings = renderer.settings();
                            if let Some(render_scale) = benchmark.render_scale {
                                settings.render_scale = render_scale;
                            }
                            if let Some(shadows_enabled) = benchmark.shadows_enabled {
                                settings.shadows_enabled = shadows_enabled;
                            }
                            if let Some(shadow_pcf_samples) = benchmark.shadow_pcf_samples {
                                settings.shadow_pcf_samples = shadow_pcf_samples;
                            }
                            if let Some(msaa_samples) = benchmark.msaa_samples {
                                settings.msaa_samples = msaa_samples;
                            }
                            renderer.set_settings(settings);
                            if let Some(requested) = benchmark.msaa_samples
                                && renderer.settings().msaa_samples != requested
                            {
                                eprintln!(
                                    "Requested benchmark MSAA {requested}x is unsupported; using {}x",
                                    renderer.settings().msaa_samples
                                );
                            }
                        }
                        debug_ui
                            .scene_loaded(benchmark.scene_path.clone(), file.environment.clone());
                        eprintln!("Benchmark scene loaded: {}", benchmark.scene_path.display());
                    }
                    Err(error) => {
                        eprintln!(
                            "Could not load benchmark scene {}: {error}",
                            benchmark.scene_path.display()
                        );
                        event_loop.exit();
                        return;
                    }
                }
            } else if let (Some(benchmark), Some(renderer), Some(debug_ui)) = (
                self.benchmark.clone(),
                self.renderer.as_mut(),
                self.debug_ui.as_mut(),
            ) && let Some(object_count) = benchmark.object_count
            {
                self.world = build_object_benchmark_world(object_count);
                let mut settings = renderer.settings();
                settings.render_scale = benchmark.render_scale.unwrap_or(1.0);
                settings.shadows_enabled = benchmark.shadows_enabled.unwrap_or(false);
                settings.shadow_pcf_samples = benchmark.shadow_pcf_samples.unwrap_or(8);
                settings.msaa_samples = benchmark.msaa_samples.unwrap_or(1);
                settings.unlocked_fps = true;
                renderer.set_settings(settings);
                let scene_path = PathBuf::from(format!("synthetic://objects/{object_count}"));
                debug_ui.scene_loaded(scene_path, None);
                eprintln!("Synthetic benchmark scene loaded: {object_count} objects");
            } else {
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
            }

            if self.benchmark.is_none()
                && std::env::args().any(|argument| argument == "--sample-assets")
            {
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
                                            keyboard_control: AnimationKeyboardControl::for_clips(
                                                &group.animation_clips,
                                            ),
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

                let profile_capture_active = self
                    .debug_ui
                    .as_ref()
                    .is_some_and(DebugUi::profile_capture_active);
                let keyboard_controller_active = !profile_capture_active
                    && Self::update_keyboard_animation_controller(
                        &mut self.keyboard_animation_controller,
                        &self.input,
                        &mut self.world,
                        self.debug_ui.as_mut(),
                    );
                let camera_before = (self.camera.eye, self.camera.yaw, self.camera.pitch);
                if !profile_capture_active && !keyboard_controller_active {
                    self.camera_controller
                        .update_camera(&mut self.camera, &self.input, dt);
                }
                if camera_before != (self.camera.eye, self.camera.yaw, self.camera.pitch)
                    && let Some(debug_ui) = self.debug_ui.as_mut()
                {
                    debug_ui.mark_scene_dirty();
                }
                self.input.reset_mouse_delta();

                let world_update_started = Instant::now();
                if !profile_capture_active {
                    self.world.update(dt);
                }
                let world_update_cpu_ms = world_update_started.elapsed().as_secs_f32() * 1000.0;
                let animation_started = Instant::now();
                if !profile_capture_active {
                    App::update_animations(
                        &mut self.world,
                        &mut self.animation_models,
                        &mut self.sampled_animations,
                        &mut self.animated_instances,
                        &mut self.animated_node_transforms,
                        dt,
                    );
                }
                let animation_cpu_ms = animation_started.elapsed().as_secs_f32() * 1000.0;
                renderer.record_update_timings(world_update_cpu_ms, animation_cpu_ms);

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
                                self.keyboard_animation_controller = None;
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
                    if self.benchmark.is_some()
                        && !self.benchmark_capture_started
                        && self
                            .debug_ui
                            .as_ref()
                            .is_some_and(|debug_ui| debug_ui.profile_environment_ready(renderer))
                        && let Some(debug_ui) = self.debug_ui.as_mut()
                    {
                        debug_ui.start_profile_capture(
                            &self.camera,
                            renderer,
                            &self.world,
                            renderer.current_timings(),
                        );
                        self.benchmark_capture_started = true;
                    }
                    if let Some(benchmark) = self.benchmark.as_ref()
                        && self.benchmark_capture_started
                        && !self
                            .debug_ui
                            .as_ref()
                            .is_some_and(DebugUi::profile_capture_active)
                        && !self.benchmark_finished
                        && let Some(debug_ui) = self.debug_ui.as_mut()
                        && let Some(status) = debug_ui.take_profile_status()
                    {
                        match status {
                            Ok(path) => {
                                self.benchmark_run_index += 1;
                                println!(
                                    "Benchmark run {}/{}: {path}",
                                    self.benchmark_run_index, benchmark.runs
                                );
                                if self.benchmark_run_index < benchmark.runs {
                                    debug_ui.start_profile_capture(
                                        &self.camera,
                                        renderer,
                                        &self.world,
                                        renderer.current_timings(),
                                    );
                                } else {
                                    self.benchmark_finished = true;
                                    event_loop.exit();
                                }
                            }
                            Err(error) => {
                                eprintln!("Benchmark capture failed: {error}");
                                event_loop.exit();
                            }
                        }
                    }
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
    fn benchmark_arguments_accept_scene_and_fixed_window_size() {
        let config = BenchmarkConfig::from_args(
            [
                "--benchmark-scene".to_owned(),
                "assets/scenes/test.bse-scene".to_owned(),
                "--benchmark-size".to_owned(),
                "1920x1080".to_owned(),
                "--benchmark-render-scale".to_owned(),
                "0.5".to_owned(),
                "--benchmark-shadows".to_owned(),
                "--benchmark-shadow-pcf-samples".to_owned(),
                "4".to_owned(),
                "--benchmark-msaa-samples".to_owned(),
                "4".to_owned(),
                "--benchmark-fullscreen".to_owned(),
            ]
            .into_iter(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            config.scene_path,
            PathBuf::from("assets/scenes/test.bse-scene")
        );
        assert_eq!(config.object_count, None);
        assert_eq!(config.runs, 1);
        assert_eq!(config.width, 1920);
        assert_eq!(config.height, 1080);
        assert_eq!(config.render_scale, Some(0.5));
        assert_eq!(config.shadows_enabled, Some(true));
        assert_eq!(config.shadow_pcf_samples, Some(4));
        assert_eq!(config.msaa_samples, Some(4));
        assert!(config.fullscreen);
    }

    #[test]
    fn benchmark_arguments_accept_default_scene() {
        let config = BenchmarkConfig::from_args(["--benchmark".to_owned()].into_iter())
            .unwrap()
            .unwrap();

        assert_eq!(config.width, 1280);
        assert_eq!(config.height, 720);
        assert_eq!(config.object_count, None);
        assert_eq!(config.runs, 1);
        assert_eq!(config.render_scale, None);
        assert_eq!(config.shadows_enabled, None);
        assert_eq!(config.shadow_pcf_samples, None);
        assert_eq!(config.msaa_samples, None);
        assert!(!config.fullscreen);
        assert_eq!(
            config.scene_path.file_name().and_then(|name| name.to_str()),
            Some("benchmark.bse-scene")
        );
    }

    #[test]
    fn benchmark_arguments_reject_out_of_range_render_scale() {
        let result = BenchmarkConfig::from_args(
            ["--benchmark-render-scale".to_owned(), "0.25".to_owned()].into_iter(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn benchmark_arguments_accept_fixed_object_counts_and_multiple_runs() {
        let config = BenchmarkConfig::from_args(
            [
                "--benchmark-objects".to_owned(),
                "10000".to_owned(),
                "--benchmark-runs".to_owned(),
                "3".to_owned(),
            ]
            .into_iter(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(config.object_count, Some(10_000));
        assert_eq!(config.runs, 3);
    }

    #[test]
    fn benchmark_arguments_reject_unsupported_object_counts_and_run_counts() {
        for arguments in [
            vec!["--benchmark-objects".to_owned(), "5000".to_owned()],
            vec!["--benchmark-runs".to_owned(), "0".to_owned()],
            vec!["--benchmark-runs".to_owned(), "11".to_owned()],
            vec![
                "--benchmark-objects".to_owned(),
                "1000".to_owned(),
                "--benchmark-scene".to_owned(),
                "scene.bse-scene".to_owned(),
            ],
        ] {
            assert!(BenchmarkConfig::from_args(arguments.into_iter()).is_err());
        }
    }

    #[test]
    fn synthetic_benchmark_worlds_have_the_requested_population() {
        for object_count in [1_000, 10_000, 100_000] {
            let world = build_object_benchmark_world(object_count);
            assert_eq!(world.objects().len(), object_count);
        }
    }

    #[test]
    fn benchmark_arguments_reject_unsupported_shadow_pcf_samples() {
        let result = BenchmarkConfig::from_args(
            ["--benchmark-shadow-pcf-samples".to_owned(), "6".to_owned()].into_iter(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn benchmark_arguments_reject_unsupported_msaa_samples() {
        let result = BenchmarkConfig::from_args(
            ["--benchmark-msaa-samples".to_owned(), "3".to_owned()].into_iter(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn keyboard_controller_maps_keys_and_release_to_configured_clips() {
        let settings = AnimationKeyboardControl {
            enabled: true,
            idle_clip_index: 2,
            walk_clip_index: 1,
            run_clip_index: 0,
        };

        assert_eq!(
            animation_controller_target_clip(settings, false, false, false),
            None
        );
        assert_eq!(
            animation_controller_target_clip(settings, true, false, false),
            Some(1)
        );
        assert_eq!(
            animation_controller_target_clip(settings, false, true, false),
            Some(0)
        );
        assert_eq!(
            animation_controller_target_clip(settings, false, false, true),
            Some(2)
        );
        assert_eq!(
            animation_controller_target_clip(settings, true, true, false),
            Some(0)
        );
    }

    fn test_animation_clips() -> [ModelAnimationClip; 2] {
        [
            ModelAnimationClip {
                name: "Source".to_owned(),
                duration: 1.0,
            },
            ModelAnimationClip {
                name: "Target".to_owned(),
                duration: 2.0,
            },
        ]
    }

    #[test]
    fn looping_animation_wraps_to_the_start() {
        let mut playback = ModelAnimationPlayback {
            time: 0.9,
            speed: 2.0,
            looping: true,
            playing: true,
            ..ModelAnimationPlayback::default()
        };
        let clips = test_animation_clips();

        advance_animation_playback(&mut playback, &clips, 0.2);

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
        let clips = test_animation_clips();

        advance_animation_playback(&mut playback, &clips, 0.2);

        assert_eq!(playback.time, 1.0);
        assert!(!playback.playing);
    }

    #[test]
    fn animation_crossfade_advances_both_clips_and_finishes() {
        let clips = test_animation_clips();
        let mut playback = ModelAnimationPlayback {
            clip_index: 1,
            playing: true,
            transition: Some(ModelAnimationTransition {
                from_clip_index: 0,
                from_time: 0.2,
                elapsed: 0.0,
                duration: 0.5,
            }),
            ..ModelAnimationPlayback::default()
        };

        advance_animation_playback(&mut playback, &clips, 0.1);

        assert!((playback.time - 0.1).abs() < 0.0001);
        let transition = playback.transition.unwrap();
        assert!((transition.from_time - 0.3).abs() < 0.0001);
        assert!((transition.elapsed - 0.1).abs() < 0.0001);

        advance_animation_playback(&mut playback, &clips, 0.4);

        assert!((playback.time - 0.5).abs() < 0.0001);
        assert!(playback.transition.is_none());
    }

    #[test]
    fn paused_animation_crossfade_keeps_clip_times_and_completes() {
        let clips = test_animation_clips();
        let mut playback = ModelAnimationPlayback {
            clip_index: 1,
            time: 0.8,
            playing: false,
            transition: Some(ModelAnimationTransition {
                from_clip_index: 0,
                from_time: 0.4,
                elapsed: 0.0,
                duration: 0.5,
            }),
            ..ModelAnimationPlayback::default()
        };

        advance_animation_playback(&mut playback, &clips, 0.25);

        assert_eq!(playback.time, 0.8);
        let transition = playback.transition.unwrap();
        assert_eq!(transition.from_time, 0.4);
        assert_eq!(transition.elapsed, 0.25);

        advance_animation_playback(&mut playback, &clips, 0.25);

        assert!(playback.transition.is_none());
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

    #[test]
    fn animation_update_applies_crossfaded_node_transforms() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("models")
            .join("Fox")
            .join("Fox.glb");
        let mut model = crate::renderer::gltf::GltfModelData::load(&path)
            .unwrap()
            .into_loaded(0, 0)
            .unwrap();
        let clips = model
            .animations
            .iter()
            .map(|animation| ModelAnimationClip {
                name: animation.name.clone(),
                duration: animation.duration,
            })
            .collect::<Vec<_>>();
        let from_time = clips[0].duration * 0.37;
        let to_time = clips[2].duration * 0.61;
        let node_hierarchy = model.node_hierarchy.clone();
        let mut world = World::new();
        let model_group = world.add_model_group("Fox", path.clone(), Transform::default());
        let node_ids = create_gltf_node_groups(&mut world, model_group, &node_hierarchy).unwrap();
        world.model_group_mut(model_group).unwrap().animation_clips = clips;
        world.model_group_mut(model_group).unwrap().animation = Some(ModelAnimationPlayback {
            clip_index: 2,
            time: to_time,
            transition: Some(ModelAnimationTransition {
                from_clip_index: 0,
                from_time,
                elapsed: 0.0,
                duration: 1.0,
            }),
            ..ModelAnimationPlayback::default()
        });

        let mut expected = Vec::new();
        model
            .sample_blended_animations_with_nodes_into(
                AnimationClipSample {
                    clip_index: 0,
                    time: from_time,
                },
                AnimationClipSample {
                    clip_index: 2,
                    time: to_time,
                },
                0.5,
                &mut Vec::new(),
                &mut expected,
            )
            .unwrap();
        let mut animation_models = HashMap::from([(path, model)]);
        let mut sampled_animations = HashMap::new();
        App::update_animations(
            &mut world,
            &mut animation_models,
            &mut sampled_animations,
            &mut Vec::new(),
            &mut Vec::new(),
            0.5,
        );

        assert!(expected.iter().all(|(node_index, expected_transform)| {
            world
                .model_group(node_ids[*node_index])
                .is_some_and(|node| {
                    node.transform
                        .to_matrix()
                        .abs_diff_eq(expected_transform.to_matrix(), 1e-5)
                })
        }));
    }
}
