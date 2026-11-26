#![allow(clippy::too_many_arguments, clippy::collapsible_if)]

use crate::camera::Camera;
use crate::config::Z_NEAR;
use crate::core::{MATERIAL_DEFAULT, MESH_CUBE};
use crate::renderer::material::MaterialParams;
use crate::renderer::state::{ParsedAsset, RenderTimings, Renderer, RendererSettings, parse_asset};
use crate::scene::{
    LoadedScene, LocalLight, LocalLightKind, MAX_ANIMATION_CROSSFADE_SECONDS, MAX_LOCAL_LIGHTS,
    ModelAnimationClip, ModelAnimationPlayback, ModelAnimationTransition, ModelGroupId, ObjectId,
    RenderObject, SceneEnvironment, Transform, World, load_scene,
};
use glam::{EulerRot, Mat4, Quat, Vec3};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use winit::window::Window;

struct PreparedFrame {
    paint_jobs: Vec<egui::ClippedPrimitive>,
    textures_delta: egui::TexturesDelta,
    screen: egui_wgpu::ScreenDescriptor,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CameraControlMode {
    ClickToCapture,
    HoldMouseButton,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CameraLookButton {
    Left,
    Right,
}

impl CameraLookButton {
    pub fn mouse_button(self) -> winit::event::MouseButton {
        match self {
            Self::Left => winit::event::MouseButton::Left,
            Self::Right => winit::event::MouseButton::Right,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SceneSelection {
    Model(ModelGroupId),
    Part(ObjectId),
    Object(ObjectId),
    Light(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    Scene,
    Lights,
    Animation,
    Render,
}

fn animation_clip_selector(
    ui: &mut egui::Ui,
    group_id: ModelGroupId,
    control: &str,
    clips: &[ModelAnimationClip],
    selected_clip: &mut usize,
) {
    let selected_name = clips
        .get(*selected_clip)
        .map(|clip| clip.name.as_str())
        .unwrap_or("Missing animation");
    egui::ComboBox::from_id_salt(("keyboard-animation-clip", group_id.0, control))
        .selected_text(selected_name)
        .show_ui(ui, |ui| {
            for (index, clip) in clips.iter().enumerate() {
                ui.selectable_value(selected_clip, index, &clip.name);
            }
        });
}

fn draw_animation_panel(
    ui: &mut egui::Ui,
    selection: Option<SceneSelection>,
    world: &mut World,
    morph_filter: &mut String,
    animation_change: &mut Option<(ModelGroupId, ModelAnimationPlayback)>,
    pause_morph_animation: &mut Option<ModelGroupId>,
    morph_weights_change: &mut Option<(SceneSelection, Vec<f32>)>,
    scene_modified: &mut bool,
) {
    let animation_group = match selection {
        Some(SceneSelection::Model(group_id)) => Some(group_id),
        Some(SceneSelection::Part(object_id) | SceneSelection::Object(object_id)) => world
            .get_object(object_id)
            .and_then(RenderObject::model_group),
        Some(SceneSelection::Light(_)) => None,
        None => None,
    };

    ui.heading("Clip playback");
    if let Some(group_id) = animation_group {
        let group_name = world
            .model_group(group_id)
            .map(|group| group.name.as_str())
            .unwrap_or("Model");
        ui.label(format!("Model: {group_name}"));
        if let Some(group) = world.model_group_mut(group_id) {
            if let Some(playback) = group.animation.as_mut() {
                let before = *playback;
                let selected_clip_name = group
                    .animation_clips
                    .get(playback.clip_index)
                    .map(|clip| clip.name.as_str())
                    .unwrap_or("Missing animation");
                let mut selected_clip_index = playback.clip_index;
                ui.add_enabled_ui(playback.transition.is_none(), |ui| {
                    egui::ComboBox::from_id_salt(("model-animation-clip", group_id.0))
                        .selected_text(selected_clip_name)
                        .show_ui(ui, |ui| {
                            for (index, clip) in group.animation_clips.iter().enumerate() {
                                ui.selectable_value(&mut selected_clip_index, index, &clip.name);
                            }
                        });
                });
                if selected_clip_index != playback.clip_index {
                    playback.switch_to_clip(selected_clip_index);
                }
                ui.horizontal(|ui| {
                    if ui
                        .button(if playback.playing { "Pause" } else { "Play" })
                        .clicked()
                    {
                        if !playback.playing
                            && !playback.looping
                            && group
                                .animation_clips
                                .get(playback.clip_index)
                                .is_some_and(|clip| playback.time >= clip.duration)
                        {
                            playback.time = 0.0;
                        }
                        playback.playing = !playback.playing;
                    }
                    if ui.button("Restart").clicked() {
                        playback.time = 0.0;
                    }
                });
                ui.checkbox(&mut playback.looping, "Loop");
                ui.add(egui::Slider::new(&mut playback.speed, 0.1..=4.0).text("Speed"));
                ui.collapsing("Keyboard controller", |ui| {
                    ui.add_enabled_ui(group.animation_clips.len() > 1, |ui| {
                        ui.checkbox(
                            &mut playback.keyboard_control.enabled,
                            "Enable for this model",
                        );
                    });
                    if group.animation_clips.len() <= 1 {
                        ui.weak("At least two animation clips are required.");
                    }
                    ui.weak("Click the viewport; W selects Walk, Shift selects Run.");
                    ui.weak("Releasing both keys returns to Idle.");
                    ui.horizontal(|ui| {
                        ui.label("Idle / released");
                        animation_clip_selector(
                            ui,
                            group_id,
                            "idle",
                            &group.animation_clips,
                            &mut playback.keyboard_control.idle_clip_index,
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("W");
                        animation_clip_selector(
                            ui,
                            group_id,
                            "walk",
                            &group.animation_clips,
                            &mut playback.keyboard_control.walk_clip_index,
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Shift");
                        animation_clip_selector(
                            ui,
                            group_id,
                            "run",
                            &group.animation_clips,
                            &mut playback.keyboard_control.run_clip_index,
                        );
                    });
                });
                ui.add(
                    egui::Slider::new(
                        &mut playback.crossfade_duration,
                        0.0..=MAX_ANIMATION_CROSSFADE_SECONDS,
                    )
                    .text("Cross-fade (s)"),
                );
                if let Some(transition) = playback.transition {
                    ui.add(
                        egui::ProgressBar::new(ModelAnimationTransition::progress(transition))
                            .text(format!("Cross-fading to {selected_clip_name}")),
                    );
                }
                if let Some(clip) = group.animation_clips.get(playback.clip_index)
                    && clip.duration > 0.0
                {
                    ui.add(egui::Slider::new(&mut playback.time, 0.0..=clip.duration).text("Time"));
                }
                if playback.clip_index != before.clip_index
                    || playback.speed != before.speed
                    || playback.looping != before.looping
                    || playback.playing != before.playing
                    || playback.crossfade_duration != before.crossfade_duration
                    || playback.keyboard_control != before.keyboard_control
                {
                    *animation_change = Some((group_id, before));
                    *scene_modified = true;
                }
            } else {
                ui.weak("This model has no animation clips.");
            }
        }
    } else {
        ui.weak("Select an animated model or model part in the Scene tab.");
    }

    ui.separator();
    ui.heading("Morph targets");
    if let Some(selection @ (SceneSelection::Part(object_id) | SceneSelection::Object(object_id))) =
        selection
    {
        let morph_animation_group = world
            .get_object(object_id)
            .and_then(RenderObject::model_group);
        let (animation_playing, animation_transitioning) = morph_animation_group
            .and_then(|id| world.model_group(id))
            .and_then(|group| group.animation)
            .map_or((false, false), |playback| {
                (playback.playing, playback.transition.is_some())
            });
        let animation_active = animation_playing || animation_transitioning;
        if let Some(object) = world.get_object_mut(object_id) {
            let morph_before = object.morph_weights().to_vec();
            if morph_before.is_empty() {
                ui.weak("This object has no morph targets.");
            } else {
                let active_count = morph_before.iter().filter(|weight| **weight != 0.0).count();
                ui.label(format!(
                    "{} targets, {active_count} active",
                    morph_before.len()
                ));
                let mut reset_weights = false;
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!animation_active, egui::Button::new("Reset all"))
                        .clicked()
                    {
                        reset_weights = true;
                    }
                    if animation_playing && ui.button("Pause animation").clicked() {
                        *pause_morph_animation = morph_animation_group;
                    }
                });
                if animation_playing {
                    ui.weak("Pause animation before editing weights.");
                } else if animation_transitioning {
                    ui.weak("Wait for the cross-fade to finish before editing weights.");
                }
                ui.horizontal(|ui| {
                    ui.label("Filter");
                    ui.text_edit_singleline(morph_filter);
                    if ui.button("Clear").clicked() {
                        morph_filter.clear();
                    }
                });
                let target_names = object.morph_target_names().to_vec();
                let filter = morph_filter.trim().to_lowercase();
                let mut matched_target = false;
                ui.add_enabled_ui(!animation_active, |ui| {
                    if reset_weights {
                        object.morph_weights_mut().fill(0.0);
                    }
                    for (index, weight) in object.morph_weights_mut().iter_mut().enumerate() {
                        let label = target_names
                            .get(index)
                            .map(String::as_str)
                            .filter(|name| !name.is_empty())
                            .map(|name| format!("{}. {}", index + 1, name))
                            .unwrap_or_else(|| format!("Target {}", index + 1));
                        if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                            continue;
                        }
                        matched_target = true;
                        if ui
                            .add(egui::Slider::new(weight, 0.0..=1.0).text(label))
                            .changed()
                        {
                            *scene_modified = true;
                        }
                    }
                });
                if !filter.is_empty() && !matched_target {
                    ui.weak("No targets match this filter.");
                }
                if reset_weights && morph_before.iter().any(|weight| *weight != 0.0) {
                    *scene_modified = true;
                }
                if object.morph_weights() != morph_before {
                    *morph_weights_change = Some((selection, morph_before));
                }
            }
        }
    } else {
        ui.weak("Select a model part or object in the Scene tab to edit morph targets.");
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GizmoMode {
    Move,
    Rotate,
    Scale,
}

#[derive(Clone, Copy)]
struct GizmoDrag {
    mode: GizmoMode,
    axis: usize,
    screen_direction: egui::Vec2,
    world_per_pixel: f32,
    last_pointer: egui::Pos2,
}

struct SceneSnapshot {
    world: World,
    selection: Option<SceneSelection>,
    render_state: RenderSnapshot,
}

#[derive(Clone)]
struct RenderSnapshot {
    settings: RendererSettings,
    materials: Vec<MaterialParams>,
}

const PROFILE_WARMUP: Duration = Duration::from_secs(2);
const PROFILE_DURATION: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Serialize)]
struct ProfileCamera {
    position: [f32; 3],
    yaw_radians: f32,
    pitch_radians: f32,
    aspect_ratio: f32,
}

#[derive(Clone, Copy, Serialize)]
struct ProfileRenderSettings {
    surface_width: u32,
    surface_height: u32,
    render_width: u32,
    render_height: u32,
    render_scale: f32,
    msaa_samples: u32,
    shadows_enabled: bool,
    shadow_pcf_samples: u32,
    unlocked_fps: bool,
    key_light_intensity: f32,
    fill_light_intensity: f32,
    environment_intensity: f32,
    exposure_ev: f32,
    debug_view: u32,
}

#[derive(Clone, Serialize)]
struct ProfileSceneSnapshot {
    draw_calls: usize,
    triangles: usize,
    meshes: usize,
    materials: usize,
    batches: usize,
    instances: usize,
    asset_paths: Vec<String>,
    estimated_asset_bounds_screen_percent: f32,
}

#[derive(Clone, Serialize)]
struct ProfileSample {
    elapsed_ms: f32,
    fps: f32,
    cpu_frame_ms: f32,
    cpu_ui_ms: f32,
    cpu_scene_ms: f32,
    cpu_acquire_ms: f32,
    cpu_submit_ms: f32,
    gpu_sample_id: Option<u64>,
    gpu_render_ms: Option<f32>,
    gpu_shadow_map_ms: Option<f32>,
    gpu_opaque_ms: Option<f32>,
    draw_calls: usize,
    triangles: usize,
    meshes: usize,
    materials: usize,
    batches: usize,
    instances: usize,
}

#[derive(Serialize)]
struct ProfileMetricSummary {
    sample_count: usize,
    median: Option<f32>,
    p95: Option<f32>,
}

#[derive(Serialize)]
struct ProfileSummary {
    cpu_frame_ms: ProfileMetricSummary,
    cpu_acquire_ms: ProfileMetricSummary,
    gpu_render_ms: ProfileMetricSummary,
    gpu_shadow_map_ms: ProfileMetricSummary,
    gpu_opaque_ms: ProfileMetricSummary,
}

#[derive(Serialize)]
struct ProfileLog {
    schema_version: u32,
    app_version: String,
    written_at_unix_ms: u64,
    scene_path: String,
    gpu_adapter: String,
    warmup_seconds: u64,
    capture_seconds: u64,
    camera: ProfileCamera,
    render: ProfileRenderSettings,
    scene: ProfileSceneSnapshot,
    summary: ProfileSummary,
    samples: Vec<ProfileSample>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfileCapturePhase {
    Warmup,
    Recording,
}

struct ProfileCapture {
    phase: ProfileCapturePhase,
    phase_started_at: Instant,
    last_gpu_sample_id: u64,
    scene_path: String,
    gpu_adapter: String,
    camera: ProfileCamera,
    render: ProfileRenderSettings,
    scene: ProfileSceneSnapshot,
    samples: Vec<ProfileSample>,
}

impl ProfileCapture {
    fn new(
        camera: &Camera,
        renderer: &Renderer,
        world: &World,
        scene_path: String,
        timings: RenderTimings,
    ) -> Self {
        let settings = renderer.settings();
        let (surface_width, surface_height) = renderer.surface_dimensions();
        let (render_width, render_height) = renderer.render_dimensions();
        Self {
            phase: ProfileCapturePhase::Warmup,
            phase_started_at: Instant::now(),
            last_gpu_sample_id: timings.gpu_sample_id,
            scene_path,
            gpu_adapter: renderer.adapter_name().to_owned(),
            camera: ProfileCamera {
                position: camera.eye.to_array(),
                yaw_radians: camera.yaw,
                pitch_radians: camera.pitch,
                aspect_ratio: camera.aspect,
            },
            render: ProfileRenderSettings {
                surface_width,
                surface_height,
                render_width,
                render_height,
                render_scale: settings.render_scale,
                msaa_samples: settings.msaa_samples,
                shadows_enabled: settings.shadows_enabled,
                shadow_pcf_samples: settings.shadow_pcf_samples,
                unlocked_fps: settings.unlocked_fps,
                key_light_intensity: settings.key_light_intensity,
                fill_light_intensity: settings.fill_light_intensity,
                environment_intensity: settings.environment_intensity,
                exposure_ev: settings.exposure_ev,
                debug_view: settings.debug_view,
            },
            scene: ProfileSceneSnapshot {
                draw_calls: timings.draw_calls,
                triangles: timings.triangle_count,
                meshes: timings.active_meshes,
                materials: timings.active_materials,
                batches: timings.render_batches,
                instances: timings.active_instances,
                asset_paths: world
                    .objects()
                    .iter()
                    .filter_map(|object| object.asset_source())
                    .map(|source| source.path.display().to_string())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                estimated_asset_bounds_screen_percent: estimated_screen_coverage_percent(
                    camera, renderer, world,
                ),
            },
            samples: Vec::new(),
        }
    }

    fn tick(&mut self, timings: RenderTimings, cpu_frame_ms: f32, fps: f32) -> bool {
        let now = Instant::now();
        if self.phase == ProfileCapturePhase::Warmup {
            if now.duration_since(self.phase_started_at) < PROFILE_WARMUP {
                return false;
            }
            self.phase = ProfileCapturePhase::Recording;
            self.phase_started_at = now;
            self.last_gpu_sample_id = timings.gpu_sample_id;
            return false;
        }

        let elapsed = now.duration_since(self.phase_started_at);
        let has_new_gpu_sample = timings.gpu_sample_id != self.last_gpu_sample_id;
        if has_new_gpu_sample {
            self.last_gpu_sample_id = timings.gpu_sample_id;
        }
        self.samples.push(ProfileSample {
            elapsed_ms: elapsed.as_secs_f32() * 1000.0,
            fps,
            cpu_frame_ms,
            cpu_ui_ms: timings.ui_cpu_ms,
            cpu_scene_ms: timings.scene_cpu_ms,
            cpu_acquire_ms: timings.acquire_cpu_ms,
            cpu_submit_ms: timings.encode_submit_cpu_ms,
            gpu_sample_id: has_new_gpu_sample.then_some(timings.gpu_sample_id),
            gpu_render_ms: has_new_gpu_sample.then_some(timings.gpu_frame_ms).flatten(),
            gpu_shadow_map_ms: has_new_gpu_sample
                .then_some(timings.gpu_shadow_map_ms)
                .flatten(),
            gpu_opaque_ms: has_new_gpu_sample
                .then_some(timings.gpu_opaque_pass_ms)
                .flatten(),
            draw_calls: timings.draw_calls,
            triangles: timings.triangle_count,
            meshes: timings.active_meshes,
            materials: timings.active_materials,
            batches: timings.render_batches,
            instances: timings.active_instances,
        });
        elapsed >= PROFILE_DURATION
    }

    fn progress_text(&self) -> String {
        let elapsed = self.phase_started_at.elapsed().as_secs_f32();
        match self.phase {
            ProfileCapturePhase::Warmup => format!(
                "Warming up: {:.1} s remaining; camera and animation are frozen",
                (PROFILE_WARMUP.as_secs_f32() - elapsed).max(0.0)
            ),
            ProfileCapturePhase::Recording => format!(
                "Recording: {:.1}/{:.0} s; {} frame samples",
                elapsed.min(PROFILE_DURATION.as_secs_f32()),
                PROFILE_DURATION.as_secs_f32(),
                self.samples.len()
            ),
        }
    }

    fn write_log(self) -> Result<String, String> {
        if self.samples.is_empty() {
            return Err("Profile capture finished without samples".to_owned());
        }
        let written_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("Could not read the system clock: {error}"))?
            .as_millis();
        let written_at_unix_ms = u64::try_from(written_at_unix_ms)
            .map_err(|error| format!("Profile timestamp is out of range: {error}"))?;
        let directory = profile_log_directory()?;
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("Could not create {}: {error}", directory.display()))?;
        let path = directory.join(format!("profile-{written_at_unix_ms}.json"));
        let log = ProfileLog {
            schema_version: 1,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            written_at_unix_ms,
            scene_path: self.scene_path,
            gpu_adapter: self.gpu_adapter,
            warmup_seconds: PROFILE_WARMUP.as_secs(),
            capture_seconds: PROFILE_DURATION.as_secs(),
            camera: self.camera,
            render: self.render,
            scene: self.scene,
            summary: ProfileSummary::from_samples(&self.samples),
            samples: self.samples,
        };
        let bytes = serde_json::to_vec_pretty(&log)
            .map_err(|error| format!("Could not serialize profile capture: {error}"))?;
        std::fs::write(&path, bytes)
            .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
        Ok(format!("Profile saved: {}", path.display()))
    }
}

impl ProfileSummary {
    fn from_samples(samples: &[ProfileSample]) -> Self {
        Self {
            cpu_frame_ms: summarize_profile_metric(
                samples.iter().map(|sample| sample.cpu_frame_ms),
            ),
            cpu_acquire_ms: summarize_profile_metric(
                samples.iter().map(|sample| sample.cpu_acquire_ms),
            ),
            gpu_render_ms: summarize_profile_metric(
                samples.iter().filter_map(|sample| sample.gpu_render_ms),
            ),
            gpu_shadow_map_ms: summarize_profile_metric(
                samples.iter().filter_map(|sample| sample.gpu_shadow_map_ms),
            ),
            gpu_opaque_ms: summarize_profile_metric(
                samples.iter().filter_map(|sample| sample.gpu_opaque_ms),
            ),
        }
    }
}

fn summarize_profile_metric(values: impl Iterator<Item = f32>) -> ProfileMetricSummary {
    let mut values: Vec<_> = values.filter(|value| value.is_finite()).collect();
    values.sort_by(f32::total_cmp);
    if values.is_empty() {
        return ProfileMetricSummary {
            sample_count: 0,
            median: None,
            p95: None,
        };
    }
    let middle = values.len() / 2;
    let median = if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) * 0.5
    } else {
        values[middle]
    };
    let p95_index = ((values.len() as f32 * 0.95).ceil() as usize).saturating_sub(1);
    ProfileMetricSummary {
        sample_count: values.len(),
        median: Some(median),
        p95: Some(values[p95_index]),
    }
}

fn profile_log_directory() -> Result<PathBuf, String> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("MyEngine").join("profile_logs"))
        .ok_or_else(|| {
            "APPDATA is unavailable; could not choose a profile log directory".to_owned()
        })
}

fn estimated_screen_coverage_percent(camera: &Camera, renderer: &Renderer, world: &World) -> f32 {
    let tan_half_fov = (45.0_f32.to_radians() * 0.5).tan();
    if !camera.aspect.is_finite() || camera.aspect <= 0.0 || tan_half_fov <= 0.0 {
        return 0.0;
    }
    let forward = Vec3::new(
        camera.yaw.cos() * camera.pitch.cos(),
        camera.pitch.sin(),
        camera.yaw.sin() * camera.pitch.cos(),
    )
    .normalize_or_zero();
    let view_projection = camera.build_view_projection_matrix();
    let mut rectangles = Vec::new();
    for object in world.objects() {
        if object.asset_source().is_none() {
            continue;
        }
        let Some(mesh) = renderer.meshes.get(object.mesh_id.0) else {
            continue;
        };
        let model_matrix = world.group_model_matrix(object);
        let center = model_matrix.transform_point3(mesh.bounds_center);
        let scale = model_matrix
            .x_axis
            .truncate()
            .length()
            .max(model_matrix.y_axis.truncate().length())
            .max(model_matrix.z_axis.truncate().length());
        let radius = mesh.bounds_radius * scale;
        let depth = (center - camera.eye).dot(forward);
        if !radius.is_finite() || !depth.is_finite() || depth + radius < Z_NEAR {
            continue;
        }
        if depth - radius <= Z_NEAR {
            return 100.0;
        }

        let clip = view_projection * center.extend(1.0);
        if !clip.w.is_finite() || clip.w <= 0.0 {
            continue;
        }
        let center_x = (clip.x / clip.w + 1.0) * 0.5;
        let center_y = (1.0 - clip.y / clip.w) * 0.5;
        let radius_x = radius / (2.0 * depth * tan_half_fov * camera.aspect);
        let radius_y = radius / (2.0 * depth * tan_half_fov);
        let rect = (
            (center_x - radius_x).clamp(0.0, 1.0),
            (center_y - radius_y).clamp(0.0, 1.0),
            (center_x + radius_x).clamp(0.0, 1.0),
            (center_y + radius_y).clamp(0.0, 1.0),
        );
        if rect.2 > rect.0 && rect.3 > rect.1 {
            rectangles.push(rect);
        }
    }
    (rectangle_union_area(&rectangles) * 100.0).clamp(0.0, 100.0)
}

fn rectangle_union_area(rectangles: &[(f32, f32, f32, f32)]) -> f32 {
    let mut x_edges: Vec<_> = rectangles
        .iter()
        .flat_map(|rect| [rect.0, rect.2])
        .collect();
    x_edges.sort_by(f32::total_cmp);
    x_edges.dedup_by(|left, right| (*left - *right).abs() <= f32::EPSILON);

    let mut area = 0.0;
    for edge_pair in x_edges.windows(2) {
        let (left, right) = (edge_pair[0], edge_pair[1]);
        if right <= left {
            continue;
        }
        let mut y_intervals: Vec<_> = rectangles
            .iter()
            .filter(|rect| rect.0 < right && rect.2 > left)
            .map(|rect| (rect.1, rect.3))
            .collect();
        y_intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut covered_height = 0.0;
        let mut current = None;
        for (top, bottom) in y_intervals {
            match current {
                Some((start, end)) if top <= end => current = Some((start, end.max(bottom))),
                Some((start, end)) => {
                    covered_height += end - start;
                    current = Some((top, bottom));
                }
                None => current = Some((top, bottom)),
            }
        }
        if let Some((start, end)) = current {
            covered_height += end - start;
        }
        area += (right - left) * covered_height;
    }
    area.clamp(0.0, 1.0)
}

impl RenderSnapshot {
    fn capture(renderer: &Renderer) -> Self {
        let materials = (0..renderer.material_count())
            .map(|index| {
                renderer
                    .material_params(index)
                    .expect("material index must be valid")
            })
            .collect();
        Self {
            settings: renderer.settings(),
            materials,
        }
    }

    fn apply(&self, renderer: &mut Renderer) {
        renderer.set_settings(self.settings);
        for (index, params) in self.materials.iter().copied().enumerate() {
            renderer.set_material_params(index, params);
        }
    }
}

#[derive(Default)]
struct UndoHistory {
    undo: Vec<SceneSnapshot>,
    redo: Vec<SceneSnapshot>,
    active_transform_selection: Option<SceneSelection>,
    active_local_light: Option<usize>,
}

impl UndoHistory {
    const MAX_STEPS: usize = 32;

    fn record(
        &mut self,
        world: &World,
        selection: Option<SceneSelection>,
        render_state: &RenderSnapshot,
    ) {
        self.record_snapshot(world.clone(), selection, render_state.clone());
    }

    fn record_snapshot(
        &mut self,
        world: World,
        selection: Option<SceneSelection>,
        render_state: RenderSnapshot,
    ) {
        Self::push_snapshot(
            &mut self.undo,
            SceneSnapshot {
                world,
                selection,
                render_state,
            },
        );
        self.redo.clear();
        self.active_transform_selection = None;
        self.active_local_light = None;
    }

    fn record_local_light(
        &mut self,
        world: &World,
        selection: Option<SceneSelection>,
        light_index: usize,
        pointer_down: bool,
        render_state: &RenderSnapshot,
    ) {
        if self.active_local_light != Some(light_index) {
            Self::push_snapshot(
                &mut self.undo,
                SceneSnapshot {
                    world: world.clone(),
                    selection,
                    render_state: render_state.clone(),
                },
            );
            self.redo.clear();
        }
        self.active_local_light = pointer_down.then_some(light_index);
    }

    fn record_transform(
        &mut self,
        world: &World,
        selection: Option<SceneSelection>,
        transform_selection: SceneSelection,
        pointer_down: bool,
        render_state: &RenderSnapshot,
    ) {
        if !pointer_down || self.active_transform_selection != Some(transform_selection) {
            Self::push_snapshot(
                &mut self.undo,
                SceneSnapshot {
                    world: world.clone(),
                    selection,
                    render_state: render_state.clone(),
                },
            );
            self.redo.clear();
        }
        self.active_transform_selection = pointer_down.then_some(transform_selection);
    }

    fn undo(
        &mut self,
        world: &mut World,
        selection: &mut Option<SceneSelection>,
        render_state: &mut RenderSnapshot,
    ) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        let id_sequences = world.id_sequences();
        Self::push_snapshot(
            &mut self.redo,
            SceneSnapshot {
                world: world.clone(),
                selection: *selection,
                render_state: render_state.clone(),
            },
        );
        *world = snapshot.world;
        world.preserve_id_sequences(id_sequences);
        *selection = snapshot
            .selection
            .filter(|selected| selection_is_valid(world, *selected));
        *render_state = snapshot.render_state;
        self.active_transform_selection = None;
        self.active_local_light = None;
        true
    }

    fn redo(
        &mut self,
        world: &mut World,
        selection: &mut Option<SceneSelection>,
        render_state: &mut RenderSnapshot,
    ) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        let id_sequences = world.id_sequences();
        Self::push_snapshot(
            &mut self.undo,
            SceneSnapshot {
                world: world.clone(),
                selection: *selection,
                render_state: render_state.clone(),
            },
        );
        *world = snapshot.world;
        world.preserve_id_sequences(id_sequences);
        *selection = snapshot
            .selection
            .filter(|selected| selection_is_valid(world, *selected));
        *render_state = snapshot.render_state;
        self.active_transform_selection = None;
        self.active_local_light = None;
        true
    }

    fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.active_transform_selection = None;
        self.active_local_light = None;
    }

    fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn push_snapshot(stack: &mut Vec<SceneSnapshot>, snapshot: SceneSnapshot) {
        if stack.len() == Self::MAX_STEPS {
            stack.remove(0);
        }
        stack.push(snapshot);
    }
}

pub struct DebugUi {
    context: egui::Context,
    platform: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    visible: bool,
    active_tab: EditorTab,
    show_camera_settings: bool,
    camera_control_mode: CameraControlMode,
    camera_look_button: CameraLookButton,
    environment_path: String,
    status: Option<Result<String, String>>,
    environment_dialog: Option<mpsc::Receiver<Option<PathBuf>>>,
    selected_material: usize,
    asset_path: String,
    asset_status: Option<Result<String, String>>,
    asset_request: Option<(PathBuf, Result<ParsedAsset, String>)>,
    asset_dialog: Option<mpsc::Receiver<Option<PathBuf>>>,
    asset_import: Option<mpsc::Receiver<(PathBuf, Result<ParsedAsset, String>)>>,
    scene_path: String,
    scene_dirty: bool,
    scene_status: Option<Result<String, String>>,
    profile_capture: Option<ProfileCapture>,
    profile_status: Option<Result<String, String>>,
    scene_save_dialog: Option<mpsc::Receiver<Option<PathBuf>>>,
    scene_load_dialog: Option<mpsc::Receiver<Option<PathBuf>>>,
    scene_loading: Option<(PathBuf, mpsc::Receiver<Result<LoadedScene, String>>)>,
    scene_save_request: Option<PathBuf>,
    scene_load_request: Option<(PathBuf, Result<LoadedScene, String>)>,
    scene_environment_request: Option<Option<PathBuf>>,
    scene_environment_load_pending: bool,
    selected_scene_item: Option<SceneSelection>,
    morph_filter: String,
    history: UndoHistory,
    gizmo_mode: GizmoMode,
    gizmo_drag: Option<GizmoDrag>,
    viewport_rect: Option<egui::Rect>,
    last_pointer_pos: Option<egui::Pos2>,
    prepared: Option<PreparedFrame>,
}

fn edit_transform(ui: &mut egui::Ui, transform: &mut Transform) -> bool {
    let (mut pitch, mut yaw, mut roll) = transform.rotation.to_euler(EulerRot::XYZ);
    pitch = pitch.to_degrees();
    yaw = yaw.to_degrees();
    roll = roll.to_degrees();
    let mut changed = false;
    let mut rotation_changed = false;
    egui::Grid::new("transform-inspector")
        .num_columns(4)
        .spacing(egui::vec2(6.0, 5.0))
        .show(ui, |ui| {
            ui.weak("");
            ui.weak("X");
            ui.weak("Y");
            ui.weak("Z");
            ui.end_row();

            ui.label("Position");
            changed |= ui
                .add_sized(
                    [72.0, 26.0],
                    egui::DragValue::new(&mut transform.position.x)
                        .speed(0.1)
                        .range(-100_000.0..=100_000.0),
                )
                .changed();
            changed |= ui
                .add_sized(
                    [72.0, 26.0],
                    egui::DragValue::new(&mut transform.position.y)
                        .speed(0.1)
                        .range(-100_000.0..=100_000.0),
                )
                .changed();
            changed |= ui
                .add_sized(
                    [72.0, 26.0],
                    egui::DragValue::new(&mut transform.position.z)
                        .speed(0.1)
                        .range(-100_000.0..=100_000.0),
                )
                .changed();
            ui.end_row();

            ui.label("Rotation");
            let value_changed = ui
                .add_sized(
                    [72.0, 26.0],
                    egui::DragValue::new(&mut pitch)
                        .speed(1.0)
                        .range(-36_000.0..=36_000.0)
                        .suffix("°"),
                )
                .changed();
            rotation_changed |= value_changed;
            changed |= value_changed;
            let value_changed = ui
                .add_sized(
                    [72.0, 26.0],
                    egui::DragValue::new(&mut yaw)
                        .speed(1.0)
                        .range(-36_000.0..=36_000.0)
                        .suffix("°"),
                )
                .changed();
            rotation_changed |= value_changed;
            changed |= value_changed;
            let value_changed = ui
                .add_sized(
                    [72.0, 26.0],
                    egui::DragValue::new(&mut roll)
                        .speed(1.0)
                        .range(-36_000.0..=36_000.0)
                        .suffix("°"),
                )
                .changed();
            rotation_changed |= value_changed;
            changed |= value_changed;
            ui.end_row();

            ui.label("Scale");
            changed |= ui
                .add_sized(
                    [72.0, 26.0],
                    egui::DragValue::new(&mut transform.scale.x)
                        .speed(0.05)
                        .range(-1_000.0..=1_000.0),
                )
                .changed();
            changed |= ui
                .add_sized(
                    [72.0, 26.0],
                    egui::DragValue::new(&mut transform.scale.y)
                        .speed(0.05)
                        .range(-1_000.0..=1_000.0),
                )
                .changed();
            changed |= ui
                .add_sized(
                    [72.0, 26.0],
                    egui::DragValue::new(&mut transform.scale.z)
                        .speed(0.05)
                        .range(-1_000.0..=1_000.0),
                )
                .changed();
            ui.end_row();
        });
    if rotation_changed {
        transform.rotation = Quat::from_euler(
            EulerRot::XYZ,
            pitch.to_radians(),
            yaw.to_radians(),
            roll.to_radians(),
        );
    }
    changed
}

fn edit_vec3_fields(ui: &mut egui::Ui, label: &str, values: &mut [f32; 3]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        for (axis, value) in ["X", "Y", "Z"].into_iter().zip(values) {
            ui.weak(axis);
            changed |= ui
                .add(
                    egui::DragValue::new(value)
                        .speed(0.05)
                        .range(-1000.0..=1000.0),
                )
                .changed();
        }
    });
    changed
}

fn project_to_screen(
    position: Vec3,
    view_projection: Mat4,
    screen_rect: egui::Rect,
) -> Option<egui::Pos2> {
    let clip = view_projection * position.extend(1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate().truncate() / clip.w;
    Some(egui::pos2(
        screen_rect.left() + (ndc.x + 1.0) * 0.5 * screen_rect.width(),
        screen_rect.top() + (1.0 - ndc.y) * 0.5 * screen_rect.height(),
    ))
}

fn axis_vector(axis: usize) -> Vec3 {
    match axis {
        0 => Vec3::X,
        1 => Vec3::Y,
        _ => Vec3::Z,
    }
}

fn rotation_ring_basis(axis: usize) -> (Vec3, Vec3) {
    match axis {
        0 => (Vec3::Y, Vec3::Z),
        1 => (Vec3::Z, Vec3::X),
        _ => (Vec3::X, Vec3::Y),
    }
}

fn selected_transform_mut(
    world: &mut World,
    selected: Option<SceneSelection>,
) -> Option<&mut Transform> {
    match selected {
        Some(SceneSelection::Model(id)) => {
            world.model_group_mut(id).map(|group| &mut group.transform)
        }
        Some(SceneSelection::Part(id) | SceneSelection::Object(id)) => {
            world.get_object_mut(id).map(|object| &mut object.transform)
        }
        Some(SceneSelection::Light(_)) | None => None,
    }
}

fn selected_name_mut(world: &mut World, selected: SceneSelection) -> Option<&mut String> {
    match selected {
        SceneSelection::Model(id) => world.model_group_mut(id).map(|group| &mut group.name),
        SceneSelection::Part(id) | SceneSelection::Object(id) => {
            world.get_object_mut(id).map(|object| &mut object.name)
        }
        SceneSelection::Light(index) => world.local_light_mut(index).map(|light| &mut light.name),
    }
}

fn selection_is_valid(world: &World, selected: SceneSelection) -> bool {
    match selected {
        SceneSelection::Model(id) => world.model_group(id).is_some(),
        SceneSelection::Part(id) | SceneSelection::Object(id) => world.get_object(id).is_some(),
        SceneSelection::Light(index) => index < world.local_lights().len(),
    }
}

fn show_local_light_tree(
    ui: &mut egui::Ui,
    world: &World,
    parent: Option<ModelGroupId>,
    selected: &mut Option<SceneSelection>,
) {
    for (index, light) in world.local_lights().iter().enumerate() {
        if world.local_light_parent(index) != parent {
            continue;
        }
        let kind = match light.kind {
            LocalLightKind::Point => "Point",
            LocalLightKind::Spot => "Spot",
        };
        if ui
            .selectable_label(
                *selected == Some(SceneSelection::Light(index)),
                format!("{kind} light · {}", light.name),
            )
            .clicked()
        {
            *selected = Some(SceneSelection::Light(index));
        }
    }
}

fn show_model_group_tree(
    ui: &mut egui::Ui,
    world: &World,
    children: &HashMap<Option<ModelGroupId>, Vec<ModelGroupId>>,
    parent: Option<ModelGroupId>,
    selected: &mut Option<SceneSelection>,
) {
    let Some(group_ids) = children.get(&parent) else {
        return;
    };
    for &group_id in group_ids {
        let Some(group) = world.model_group(group_id) else {
            continue;
        };
        let is_selected = match *selected {
            Some(SceneSelection::Model(selected_id)) => selected_id == group_id,
            Some(SceneSelection::Part(object_id) | SceneSelection::Object(object_id)) => {
                world.get_object(object_id).is_some_and(|object| {
                    object.model_group() == Some(group_id) || object.parent_node() == Some(group_id)
                })
            }
            Some(SceneSelection::Light(index)) => world.local_light_parent(index) == Some(group_id),
            None => false,
        };
        let label = if group.source_path.is_some() {
            format!("{} ({} parts)", group.name, group.objects.len())
        } else if world.gltf_node_index(group_id).is_some() {
            format!("{} (glTF node)", group.name)
        } else {
            format!("{} (empty node)", group.name)
        };
        let has_children = children
            .get(&Some(group_id))
            .is_some_and(|group_children| !group_children.is_empty())
            || world
                .local_lights()
                .iter()
                .enumerate()
                .any(|(index, _)| world.local_light_parent(index) == Some(group_id));
        if has_children {
            let id = ui.make_persistent_id(("model-group-tree", group_id.0));
            let (_, header, _) = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                false,
            )
            .show_header(ui, |ui| ui.selectable_label(is_selected, label))
            .body(|ui| {
                show_model_group_tree(ui, world, children, Some(group_id), selected);
                show_local_light_tree(ui, world, Some(group_id), selected);
            });
            if header.inner.clicked() {
                *selected = Some(SceneSelection::Model(group_id));
            }
        } else if ui.selectable_label(is_selected, label).clicked() {
            *selected = Some(SceneSelection::Model(group_id));
        }
    }
}

fn selected_world_position(world: &World, selected: Option<SceneSelection>) -> Option<Vec3> {
    match selected {
        Some(SceneSelection::Model(id)) => world.model_group(id).map(|_| {
            world
                .model_group_world_matrix(id)
                .transform_point3(Vec3::ZERO)
        }),
        Some(SceneSelection::Part(id) | SceneSelection::Object(id)) => {
            world.get_object(id).map(|object| {
                world
                    .group_model_matrix(object)
                    .transform_point3(Vec3::ZERO)
            })
        }
        Some(SceneSelection::Light(index)) => world.local_light_world_position(index),
        None => None,
    }
}

fn selected_parent_world_matrix(world: &World, selected: Option<SceneSelection>) -> Mat4 {
    let parent = match selected {
        Some(SceneSelection::Model(id)) => world.model_group_parent(id),
        Some(SceneSelection::Part(id) | SceneSelection::Object(id)) => world
            .get_object(id)
            .and_then(|object| object.parent_node().or_else(|| object.model_group())),
        Some(SceneSelection::Light(index)) => world.local_light_parent(index),
        None => None,
    };
    parent.map_or(Mat4::IDENTITY, |id| world.model_group_world_matrix(id))
}

fn pick_scene_object(
    pointer: egui::Pos2,
    screen_rect: egui::Rect,
    camera: &Camera,
    world: &World,
) -> Option<SceneSelection> {
    let view_projection = camera.build_view_projection_matrix();
    world
        .objects()
        .iter()
        .enumerate()
        .filter_map(|(index, object)| {
            let model_matrix = world.group_model_matrix(object);
            let position = model_matrix.transform_point3(Vec3::ZERO);
            let screen = project_to_screen(position, view_projection, screen_rect)?;
            let distance_squared = screen.distance_sq(pointer);
            let world_scale = model_matrix
                .to_scale_rotation_translation()
                .0
                .abs()
                .max_element();
            let radius = (screen_rect.height() * 1.207 * world_scale
                / camera.eye.distance(position).max(0.1))
            .clamp(18.0, 120.0);
            (distance_squared <= radius * radius)
                .then(|| world.object_id_at(index))
                .flatten()
                .map(|id| (distance_squared, SceneSelection::Part(id)))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, selection)| selection)
}

fn distance_to_segment(point: egui::Pos2, start: egui::Pos2, end: egui::Pos2) -> f32 {
    let segment = end - start;
    let length_squared = segment.length_sq();
    if length_squared <= f32::EPSILON {
        return point.distance(start);
    }
    let t = ((point - start).dot(segment) / length_squared).clamp(0.0, 1.0);
    point.distance(start + segment * t)
}

fn rotation_ring_points(
    position: Vec3,
    axis: usize,
    radius: f32,
    view_projection: Mat4,
    screen_rect: egui::Rect,
) -> Option<Vec<egui::Pos2>> {
    let (u, v) = rotation_ring_basis(axis);
    (0..=96)
        .map(|step| {
            let angle = std::f32::consts::TAU * step as f32 / 96.0;
            let point = position + (u * angle.cos() + v * angle.sin()) * radius;
            project_to_screen(point, view_projection, screen_rect)
        })
        .collect()
}

fn drag_local_light(
    world: &mut World,
    index: usize,
    mode: GizmoMode,
    axis: usize,
    amount: f32,
) -> bool {
    if amount == 0.0 || index >= world.local_lights().len() {
        return false;
    }
    let parent_matrix = selected_parent_world_matrix(world, Some(SceneSelection::Light(index)));
    let determinant = parent_matrix.determinant();
    if !determinant.is_finite() || determinant.abs() <= f32::EPSILON {
        return false;
    }
    let inverse_parent = parent_matrix.inverse();
    match mode {
        GizmoMode::Move => {
            let local_delta = inverse_parent.transform_vector3(axis_vector(axis) * amount);
            if !local_delta.is_finite() {
                return false;
            }
            let Some(light) = world.local_light_mut(index) else {
                return false;
            };
            let position = Vec3::from_array(light.position) + local_delta;
            if !position.is_finite() {
                return false;
            }
            light.position = position.to_array();
            true
        }
        GizmoMode::Rotate => {
            let Some(direction) = world.local_light_world_direction(index) else {
                return false;
            };
            let rotated = Quat::from_axis_angle(axis_vector(axis), amount) * direction;
            let local_direction = inverse_parent
                .transform_vector3(rotated)
                .normalize_or_zero();
            if !local_direction.is_finite() || local_direction.length_squared() <= f32::EPSILON {
                return false;
            }
            if let Some(light) = world.local_light_mut(index) {
                light.direction = local_direction.to_array();
                true
            } else {
                false
            }
        }
        GizmoMode::Scale => {
            if let Some(light) = world.local_light_mut(index) {
                light.range = (light.range + amount).clamp(0.1, 1000.0);
                true
            } else {
                false
            }
        }
    }
}

fn paint_scene_gizmo(
    ui: &egui::Ui,
    viewport_rect: egui::Rect,
    screen_rect: egui::Rect,
    camera: &Camera,
    world: &mut World,
    selected: &mut Option<SceneSelection>,
    mode: GizmoMode,
    drag_state: &mut Option<GizmoDrag>,
) -> bool {
    let mut changed = false;
    let context = ui.ctx();
    let pointer = context.input(|input| input.pointer.latest_pos());
    let primary_pressed =
        context.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary));
    let primary_down =
        context.input(|input| input.pointer.button_down(egui::PointerButton::Primary));
    let primary_released =
        context.input(|input| input.pointer.button_released(egui::PointerButton::Primary));
    let view_projection = camera.build_view_projection_matrix();
    let pointer_is_in_viewport = pointer.is_some_and(|position| viewport_rect.contains(position));

    if primary_pressed && pointer_is_in_viewport {
        if let Some(pointer_position) = pointer {
            let selected_center = selected_world_position(world, *selected)
                .and_then(|position| project_to_screen(position, view_projection, screen_rect));
            let mut hit_axis = None;
            if let Some(center) = selected_center {
                if mode == GizmoMode::Rotate {
                    if let Some(position) = selected_world_position(world, *selected) {
                        let ring_radius = camera.eye.distance(position).max(0.1) * 0.828_427 * 72.0
                            / screen_rect.height().max(1.0);
                        for axis in 0..3 {
                            let Some(points) = rotation_ring_points(
                                position,
                                axis,
                                ring_radius,
                                view_projection,
                                screen_rect,
                            ) else {
                                continue;
                            };
                            for segment in points.windows(2) {
                                let tangent = segment[1] - segment[0];
                                let distance =
                                    distance_to_segment(pointer_position, segment[0], segment[1]);
                                if distance <= 8.0 {
                                    hit_axis = Some((
                                        axis,
                                        tangent.normalized(),
                                        1.0 / pointer_position.distance(center).max(10.0),
                                    ));
                                    break;
                                }
                            }
                            if hit_axis.is_some() {
                                break;
                            }
                        }
                    }
                } else if let Some(position) = selected_world_position(world, *selected) {
                    let world_length = camera.eye.distance(position).max(0.1) * 0.828_427 * 72.0
                        / screen_rect.height().max(1.0);
                    for axis in 0..3 {
                        let direction = axis_vector(axis);
                        let Some(end) = project_to_screen(
                            position + direction * world_length,
                            view_projection,
                            screen_rect,
                        ) else {
                            continue;
                        };
                        let projected_axis = end - center;
                        let projected_length = projected_axis.length();
                        if projected_length < 1.0 {
                            continue;
                        }
                        if pointer_position.distance(center) >= 14.0
                            && distance_to_segment(pointer_position, center, end) <= 10.0
                        {
                            hit_axis = Some((
                                axis,
                                projected_axis / projected_length,
                                world_length / projected_length,
                            ));
                            break;
                        }
                    }
                }
            }

            if let Some((axis, screen_direction, world_per_pixel)) = hit_axis {
                *drag_state = Some(GizmoDrag {
                    mode,
                    axis,
                    screen_direction,
                    world_per_pixel,
                    last_pointer: pointer_position,
                });
            } else if let Some(selection) =
                pick_scene_object(pointer_position, screen_rect, camera, world)
            {
                *selected = Some(selection);
                *drag_state = None;
            }
        }
    }

    if let (Some(pointer_position), Some(mut drag)) = (pointer, *drag_state) {
        if primary_down {
            let pointer_delta = pointer_position - drag.last_pointer;
            let amount = pointer_delta.dot(drag.screen_direction) * drag.world_per_pixel;
            let parent_matrix = (drag.mode == GizmoMode::Move)
                .then(|| selected_parent_world_matrix(world, *selected));
            if let Some(SceneSelection::Light(index)) = *selected {
                changed |= drag_local_light(world, index, drag.mode, drag.axis, amount);
            } else if let Some(transform) = selected_transform_mut(world, *selected) {
                match drag.mode {
                    GizmoMode::Move => {
                        let parent_matrix = parent_matrix.unwrap_or(Mat4::IDENTITY);
                        let determinant = parent_matrix.determinant();
                        if amount != 0.0
                            && determinant.is_finite()
                            && determinant.abs() > f32::EPSILON
                        {
                            let local_delta = parent_matrix
                                .inverse()
                                .transform_vector3(axis_vector(drag.axis) * amount);
                            if local_delta.is_finite() {
                                transform.position += local_delta;
                                changed = true;
                            }
                        }
                    }
                    GizmoMode::Rotate => {
                        changed |= amount != 0.0;
                        let rotation = Quat::from_axis_angle(axis_vector(drag.axis), amount);
                        transform.rotation = rotation * transform.rotation;
                    }
                    GizmoMode::Scale => {
                        changed |= amount != 0.0;
                        transform.scale[drag.axis] =
                            (transform.scale[drag.axis] + amount).clamp(-1_000.0, 1_000.0);
                    }
                }
            }
            drag.last_pointer = pointer_position;
            *drag_state = Some(drag);
        }
    }
    if primary_released || !primary_down {
        *drag_state = None;
    }

    let Some(position) = selected_world_position(world, *selected) else {
        return changed;
    };
    let Some(center) = project_to_screen(position, view_projection, screen_rect) else {
        return changed;
    };
    if !viewport_rect.contains(center) {
        return changed;
    }

    let painter = ui.painter().with_clip_rect(viewport_rect);
    let colors = [
        egui::Color32::from_rgb(235, 75, 65),
        egui::Color32::from_rgb(110, 220, 100),
        egui::Color32::from_rgb(75, 145, 245),
    ];
    if mode == GizmoMode::Rotate {
        let ring_radius = camera.eye.distance(position).max(0.1) * 0.828_427 * 72.0
            / screen_rect.height().max(1.0);
        for (axis, color) in colors.into_iter().enumerate() {
            if let Some(points) =
                rotation_ring_points(position, axis, ring_radius, view_projection, screen_rect)
            {
                painter.add(egui::Shape::line(points, egui::Stroke::new(2.5_f32, color)));
            }
        }
        return changed;
    }

    let world_length =
        camera.eye.distance(position).max(0.1) * 0.828_427 * 72.0 / screen_rect.height().max(1.0);
    for (axis, color) in colors.into_iter().enumerate() {
        let Some(end) = project_to_screen(
            position + axis_vector(axis) * world_length,
            view_projection,
            screen_rect,
        ) else {
            continue;
        };
        let offset = end - center;
        if offset.length_sq() < 1.0 {
            continue;
        }
        let direction = offset.normalized();
        painter.line_segment([center, end], egui::Stroke::new(3.0_f32, color));
        if mode == GizmoMode::Move {
            let tip = end;
            let base = tip - direction * 12.0;
            let side = egui::vec2(-direction.y, direction.x) * 5.0;
            painter.add(egui::Shape::convex_polygon(
                vec![tip, base + side, base - side],
                color,
                egui::Stroke::NONE,
            ));
        } else {
            painter.rect_filled(
                egui::Rect::from_center_size(end, egui::vec2(10.0, 10.0)),
                1.0,
                color,
            );
        }
    }
    painter.circle_filled(center, 4.0, egui::Color32::WHITE);
    changed
}

impl DebugUi {
    pub fn new(window: &Arc<Window>, device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let context = egui::Context::default();
        let mut visuals = egui::Visuals::dark();
        visuals.override_text_color = Some(egui::Color32::from_rgb(222, 222, 222));
        visuals.weak_text_color = Some(egui::Color32::from_rgb(150, 150, 150));
        visuals.panel_fill = egui::Color32::from_rgb(30, 30, 30);
        visuals.window_fill = egui::Color32::from_rgb(38, 38, 38);
        visuals.extreme_bg_color = egui::Color32::from_rgb(24, 24, 24);
        visuals.faint_bg_color = egui::Color32::from_rgb(44, 44, 44);
        visuals.selection.bg_fill = egui::Color32::from_rgb(68, 68, 68);
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(115));
        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(36, 36, 36);
        visuals.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(53));
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(48, 48, 48);
        visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(42, 42, 42);
        visuals.widgets.inactive.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(72));
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(61, 61, 61);
        visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(56, 56, 56);
        visuals.widgets.hovered.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(90));
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(68, 68, 68);
        visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(62, 62, 62);
        visuals.widgets.open.bg_fill = egui::Color32::from_rgb(48, 48, 48);
        visuals.widgets.open.weak_bg_fill = egui::Color32::from_rgb(48, 48, 48);
        visuals.collapsing_header_frame = true;
        visuals.button_frame = true;
        context.set_visuals(visuals);
        let mut style = (*context.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 7.0);
        style.spacing.button_padding = egui::vec2(10.0, 7.0);
        style.spacing.window_margin = egui::Margin::same(12);
        style.spacing.menu_margin = egui::Margin::same(8);
        context.set_style(style);
        let platform = egui_winit::State::new(
            context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        Self {
            context,
            platform,
            renderer: egui_wgpu::Renderer::new(device, format, None, 1, false),
            visible: true,
            active_tab: EditorTab::Scene,
            show_camera_settings: false,
            camera_control_mode: CameraControlMode::HoldMouseButton,
            camera_look_button: CameraLookButton::Right,
            environment_path: String::new(),
            status: None,
            environment_dialog: None,
            selected_material: 0,
            asset_path: String::new(),
            asset_status: None,
            asset_request: None,
            asset_dialog: None,
            asset_import: None,
            scene_path: String::new(),
            scene_dirty: false,
            scene_status: None,
            profile_capture: None,
            profile_status: None,
            scene_save_dialog: None,
            scene_load_dialog: None,
            scene_loading: None,
            scene_save_request: None,
            scene_load_request: None,
            scene_environment_request: None,
            scene_environment_load_pending: false,
            selected_scene_item: None,
            morph_filter: String::new(),
            history: UndoHistory::default(),
            gizmo_mode: GizmoMode::Move,
            gizmo_drag: None,
            viewport_rect: None,
            last_pointer_pos: None,
            prepared: None,
        }
    }

    pub fn on_window_event(&mut self, window: &Window, event: &winit::event::WindowEvent) -> bool {
        let consumed = self.platform.on_window_event(window, event).consumed;
        if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
            let pixels_per_point = window.scale_factor() as f32;
            self.last_pointer_pos = Some(egui::pos2(
                position.x as f32 / pixels_per_point,
                position.y as f32 / pixels_per_point,
            ));
        }
        let pointer_in_viewport = self.last_pointer_pos.is_some_and(|position| {
            self.viewport_rect
                .is_some_and(|rect| rect.contains(position))
        });
        if self.visible
            && pointer_in_viewport
            && self.camera_control_mode == CameraControlMode::HoldMouseButton
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
        {
            return true;
        }
        if self.visible
            && pointer_in_viewport
            && self.camera_control_mode == CameraControlMode::ClickToCapture
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
        {
            return false;
        }
        if self.visible
            && pointer_in_viewport
            && self.camera_control_mode == CameraControlMode::HoldMouseButton
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    button,
                    ..
                } if *button == self.camera_look_button.mouse_button()
            )
        {
            return false;
        }
        consumed
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub fn camera_control_mode(&self) -> CameraControlMode {
        self.camera_control_mode
    }

    pub fn camera_look_button(&self) -> CameraLookButton {
        self.camera_look_button
    }

    pub fn profile_capture_active(&self) -> bool {
        self.profile_capture.is_some()
    }

    pub fn animation_controller_target(&self, world: &World) -> Option<ModelGroupId> {
        if self.active_tab != EditorTab::Animation {
            return None;
        }
        match self.selected_scene_item {
            Some(SceneSelection::Model(group_id)) => Some(group_id),
            Some(SceneSelection::Part(object_id) | SceneSelection::Object(object_id)) => world
                .get_object(object_id)
                .and_then(RenderObject::model_group),
            Some(SceneSelection::Light(_)) => None,
            None => None,
        }
    }

    fn update_profile_capture(&mut self, timings: RenderTimings, cpu_frame_ms: f32, fps: f32) {
        let finished = self
            .profile_capture
            .as_mut()
            .is_some_and(|capture| capture.tick(timings, cpu_frame_ms, fps));
        if finished && let Some(capture) = self.profile_capture.take() {
            self.profile_status = Some(capture.write_log());
        }
    }

    pub fn take_asset_request(&mut self) -> Option<(PathBuf, Result<ParsedAsset, String>)> {
        self.asset_request.take()
    }

    pub fn set_asset_status(&mut self, status: Result<String, String>) {
        self.asset_status = Some(status);
    }

    pub fn take_scene_save_request(&mut self) -> Option<PathBuf> {
        self.scene_save_request.take()
    }

    pub fn take_scene_load_request(&mut self) -> Option<(PathBuf, Result<LoadedScene, String>)> {
        self.scene_load_request.take()
    }

    pub fn set_scene_status(&mut self, status: Result<String, String>) {
        self.scene_status = Some(status);
    }

    pub fn scene_save_finished(&mut self, path: PathBuf, result: Result<String, String>) {
        if result.is_ok() {
            self.scene_path = path.display().to_string();
            self.scene_dirty = false;
        }
        self.scene_status = Some(result);
    }

    pub fn mark_scene_dirty(&mut self) {
        self.scene_dirty = true;
    }

    pub fn scene_loaded(&mut self, path: PathBuf, environment: Option<SceneEnvironment>) {
        self.scene_path = path.display().to_string();
        self.scene_dirty = false;
        self.selected_scene_item = None;
        self.history.clear();
        self.scene_environment_load_pending = false;
        self.scene_environment_request =
            environment.map(|environment| environment.path.map(PathBuf::from));
        if let Some(path) = self.scene_environment_request.as_ref() {
            self.environment_path = path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
        }
    }

    pub fn record_undo_snapshot(&mut self, world: World, renderer: &Renderer) {
        self.history.record_snapshot(
            world,
            self.selected_scene_item,
            RenderSnapshot::capture(renderer),
        );
    }

    pub fn prepare_frame(
        &mut self,
        camera: &Camera,
        window: &Window,
        frame_time: f32,
        fps: f32,
        cpu_frame_time: f32,
        timings: RenderTimings,
        renderer: &mut Renderer,
        world: &mut World,
    ) {
        self.update_profile_capture(timings, cpu_frame_time, fps);
        if let Some(result) = renderer.poll_environment_load() {
            match result {
                Ok(Some(_)) => {
                    self.status = Some(Ok("HDR environment loaded".to_owned()));
                    if self.scene_environment_load_pending
                        || self.scene_environment_request.is_some()
                    {
                        self.scene_environment_load_pending = false;
                    } else {
                        self.mark_scene_dirty();
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.scene_environment_load_pending = false;
                    self.status = Some(Err(error));
                }
            }
        }
        if !renderer.environment_is_loading()
            && let Some(path) = self.scene_environment_request.take()
        {
            let result = match path {
                Some(path) => renderer.begin_environment_load(path),
                None => renderer.begin_default_environment_load(),
            };
            match result {
                Ok(()) => self.scene_environment_load_pending = true,
                Err(error) => {
                    self.scene_environment_load_pending = false;
                    self.status = Some(Err(error));
                }
            }
        }

        let mut settings = renderer.settings();
        let material_count = renderer.material_count();
        self.selected_material = self.selected_material.min(material_count.saturating_sub(1));
        let mut selected_params = renderer
            .material_params(self.selected_material)
            .unwrap_or_default();
        let settings_before = settings;
        let selected_params_before = selected_params;
        let mut load_environment = false;
        let mut pick_environment = false;
        let mut pick_asset = false;
        let mut pick_scene_to_save = false;
        let mut pick_scene_to_load = false;
        let mut add_cube = false;
        let mut add_empty_node = false;
        let mut duplicate_selection = false;
        let mut delete_selection = false;
        let mut undo_requested = false;
        let mut redo_requested = false;
        let mut save_requested = false;
        let mut save_current = false;
        let mut scene_modified = false;
        let mut add_local_light = None;
        let mut transform_change = None;
        let mut name_change = None;
        let mut animation_change = None;
        let mut parent_change = None;
        let mut morph_weights_change = None;
        let mut pause_morph_animation = None;
        let selection_is_valid = self
            .selected_scene_item
            .is_some_and(|selected| selection_is_valid(world, selected));
        let mut selected_scene_item = if selection_is_valid {
            self.selected_scene_item
        } else {
            None
        };
        let local_lights_before = world.local_lights().to_vec();
        if let Some(import) = &self.asset_import {
            match import.try_recv() {
                Ok(request) => {
                    self.asset_request = Some(request);
                    self.asset_import = None;
                    self.asset_status = Some(Ok("Preparing model for GPU upload...".to_owned()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.asset_status =
                        Some(Err("Model import worker failed unexpectedly".to_owned()));
                    self.asset_import = None;
                }
            }
        }
        if let Some(dialog) = &self.environment_dialog {
            match dialog.try_recv() {
                Ok(Some(path)) => {
                    self.environment_path = path.display().to_string();
                    load_environment = true;
                    self.environment_dialog = None;
                }
                Ok(None) => self.environment_dialog = None,
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.status = Some(Err("HDR file dialog failed unexpectedly".to_owned()));
                    self.environment_dialog = None;
                }
            }
        }
        if let Some(dialog) = &self.asset_dialog {
            match dialog.try_recv() {
                Ok(Some(path)) => {
                    self.asset_path = path.display().to_string();
                    match Self::parse_asset_in_background(path) {
                        Ok(receiver) => {
                            self.asset_import = Some(receiver);
                            self.asset_status = Some(Ok("Reading model data...".to_owned()));
                        }
                        Err(error) => {
                            self.asset_status =
                                Some(Err(format!("Could not start model import: {error}")));
                        }
                    }
                    self.asset_dialog = None;
                }
                Ok(None) => self.asset_dialog = None,
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.asset_status =
                        Some(Err("Model file dialog failed unexpectedly".to_owned()));
                    self.asset_dialog = None;
                }
            }
        }
        if let Some(dialog) = &self.scene_save_dialog {
            match dialog.try_recv() {
                Ok(Some(path)) => {
                    self.scene_save_request = Some(path);
                    self.scene_save_dialog = None;
                }
                Ok(None) => self.scene_save_dialog = None,
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.scene_status =
                        Some(Err("Scene save dialog failed unexpectedly".to_owned()));
                    self.scene_save_dialog = None;
                }
            }
        }
        if let Some(dialog) = &self.scene_load_dialog {
            match dialog.try_recv() {
                Ok(Some(path)) => {
                    match Self::load_scene_in_background(path.clone()) {
                        Ok(receiver) => {
                            self.scene_loading = Some((path, receiver));
                            self.scene_status =
                                Some(Ok("Reading scene and model files...".to_owned()));
                        }
                        Err(error) => {
                            self.scene_status =
                                Some(Err(format!("Could not start scene loading: {error}")));
                        }
                    }
                    self.scene_load_dialog = None;
                }
                Ok(None) => self.scene_load_dialog = None,
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.scene_status =
                        Some(Err("Scene open dialog failed unexpectedly".to_owned()));
                    self.scene_load_dialog = None;
                }
            }
        }
        if let Some((path, loading)) = &self.scene_loading {
            match loading.try_recv() {
                Ok(result) => {
                    self.scene_load_request = Some((path.clone(), result));
                    self.scene_loading = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.scene_status =
                        Some(Err("Scene loading worker failed unexpectedly".to_owned()));
                    self.scene_loading = None;
                }
            }
        }
        let input = self.platform.take_egui_input(window);
        let output = self.context.run(input, |context| {
            if !self.visible {
                return;
            }
            let wants_keyboard_input = context.wants_keyboard_input();
            context.input_mut(|input| {
                if !wants_keyboard_input {
                    undo_requested |= input.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
                    redo_requested |= input.consume_key(egui::Modifiers::COMMAND, egui::Key::Y)
                        || input.consume_key(
                            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                            egui::Key::Z,
                        );
                }
                save_requested |= input.consume_key(egui::Modifiers::COMMAND, egui::Key::S);
            });
            let scene_io_enabled = self.scene_loading.is_none()
                && self.scene_save_dialog.is_none()
                && self.scene_load_dialog.is_none()
                && self.scene_save_request.is_none()
                && self.scene_load_request.is_none()
                && self.asset_dialog.is_none()
                && self.asset_import.is_none()
                && self.asset_request.is_none();
            if save_requested && scene_io_enabled {
                if self.scene_path.is_empty() {
                    pick_scene_to_save = true;
                } else {
                    save_current = true;
                }
            }
            let import_enabled = self.asset_dialog.is_none()
                && self.asset_import.is_none()
                && self.scene_loading.is_none()
                && self.scene_save_dialog.is_none()
                && self.scene_load_dialog.is_none()
                && self.scene_save_request.is_none()
                && self.scene_load_request.is_none();
            egui::TopBottomPanel::top("editor-toolbar")
                .exact_height(42.0)
                .show(context, |ui| {
                    egui::MenuBar::new().ui(ui, |ui| {
                        ui.menu_button("File", |ui| {
                            if ui
                                .add_enabled(scene_io_enabled, egui::Button::new("Open scene..."))
                                .clicked()
                            {
                                pick_scene_to_load = true;
                                ui.close();
                            }
                            if ui
                                .add_enabled(
                                    scene_io_enabled,
                                    egui::Button::new("Save scene").shortcut_text("Ctrl+S"),
                                )
                                .clicked()
                            {
                                if self.scene_path.is_empty() {
                                    pick_scene_to_save = true;
                                } else {
                                    save_current = true;
                                }
                                ui.close();
                            }
                            if ui
                                .add_enabled(
                                    scene_io_enabled,
                                    egui::Button::new("Save scene as..."),
                                )
                                .clicked()
                            {
                                pick_scene_to_save = true;
                                ui.close();
                            }
                        });
                        if ui
                            .add_enabled(
                                self.history.can_undo(),
                                egui::Button::new("Undo").shortcut_text("Ctrl+Z"),
                            )
                            .clicked()
                        {
                            undo_requested = true;
                        }
                        if ui
                            .add_enabled(
                                self.history.can_redo(),
                                egui::Button::new("Redo").shortcut_text("Ctrl+Y"),
                            )
                            .clicked()
                        {
                            redo_requested = true;
                        }
                        if ui
                            .add_enabled(import_enabled, egui::Button::new("Import model"))
                            .clicked()
                        {
                            pick_asset = true;
                        }
                        if ui
                            .add_enabled(scene_io_enabled, egui::Button::new("Add cube"))
                            .clicked()
                        {
                            add_cube = true;
                        }
                        if ui
                            .add_enabled(
                                scene_io_enabled,
                                egui::Button::new("Add empty node"),
                            )
                            .clicked()
                        {
                            add_empty_node = true;
                        }
                        let has_selection = selected_scene_item.is_some();
                        if ui
                            .add_enabled(
                                scene_io_enabled && has_selection,
                                egui::Button::new("Duplicate"),
                            )
                            .clicked()
                        {
                            duplicate_selection = true;
                        }
                        if ui
                            .add_enabled(
                                scene_io_enabled && has_selection,
                                egui::Button::new("Delete"),
                            )
                            .clicked()
                        {
                            delete_selection = true;
                        }
                        if ui
                            .add_enabled(
                                !renderer.environment_is_loading()
                                    && self.environment_dialog.is_none(),
                                egui::Button::new("Load HDR"),
                            )
                            .clicked()
                        {
                            pick_environment = true;
                        }
                        ui.separator();
                        for (mode, label) in [
                            (GizmoMode::Move, "Move"),
                            (GizmoMode::Rotate, "Rotate"),
                            (GizmoMode::Scale, "Scale"),
                        ] {
                            if ui
                                .selectable_label(self.gizmo_mode == mode, label)
                                .clicked()
                            {
                                self.gizmo_mode = mode;
                            }
                        }
                        ui.menu_button("View", |ui| {
                            if ui.button("Camera controls...").clicked() {
                                self.show_camera_settings = true;
                                ui.close();
                            }
                        });
                        ui.separator();
                        ui.label(format!("{:.0} FPS", fps));
                    });
                });
            egui::SidePanel::right("editor-sidebar")
                .default_width(400.0)
                .width_range(320.0..=520.0)
                .resizable(true)
                .show(context, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(self.active_tab == EditorTab::Scene, "Scene")
                            .clicked()
                        {
                            self.active_tab = EditorTab::Scene;
                        }
                        if ui
                            .selectable_label(self.active_tab == EditorTab::Lights, "Lights")
                            .clicked()
                        {
                            self.active_tab = EditorTab::Lights;
                        }
                        if ui
                            .selectable_label(
                                self.active_tab == EditorTab::Animation,
                                "Animation",
                            )
                            .clicked()
                        {
                            self.active_tab = EditorTab::Animation;
                        }
                        if ui
                            .selectable_label(self.active_tab == EditorTab::Render, "Render")
                            .clicked()
                        {
                            self.active_tab = EditorTab::Render;
                        }
                    });
                    ui.separator();

                    egui::ScrollArea::vertical()
                        .id_salt("editor-sidebar-content")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if self.active_tab == EditorTab::Scene {
                                egui::CollapsingHeader::new("Hierarchy")
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        let scene_name = Path::new(&self.scene_path)
                                            .file_name()
                                            .and_then(|name| name.to_str())
                                            .filter(|_| !self.scene_path.is_empty())
                                            .unwrap_or("Untitled scene");
                                        ui.horizontal(|ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(format!(
                                                        "Scene: {scene_name}"
                                                    ))
                                                    .small(),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(&self.scene_path);
                                            if self.scene_dirty {
                                                let (rect, response) = ui.allocate_exact_size(
                                                    egui::vec2(8.0, 8.0),
                                                    egui::Sense::hover(),
                                                );
                                                ui.painter().circle_filled(
                                                    rect.center(),
                                                    3.0,
                                                    egui::Color32::YELLOW,
                                                );
                                                response.on_hover_text(
                                                    "Unsaved changes — press Ctrl+S to save",
                                                );
                                            }
                                        });
                                        if self.scene_loading.is_some()
                                            || self.scene_load_dialog.is_some()
                                            || self.scene_save_dialog.is_some()
                                        {
                                            ui.label("Working with scene...");
                                        }
                                        if let Some(status) = &self.scene_status {
                                            match status {
                                                Ok(message) => {
                                                    ui.colored_label(
                                                        egui::Color32::LIGHT_GREEN,
                                                        message,
                                                    );
                                                }
                                                Err(message) => {
                                                    ui.colored_label(
                                                        egui::Color32::LIGHT_RED,
                                                        message,
                                                    );
                                                }
                                            }
                                        }
                                        if !self.asset_path.is_empty() {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(format!(
                                                        "Model: {}",
                                                        Path::new(&self.asset_path)
                                                            .file_name()
                                                            .and_then(|name| name.to_str())
                                                            .unwrap_or(&self.asset_path)
                                                    ))
                                                    .small(),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(&self.asset_path);
                                        }
                                        if self.asset_dialog.is_some() {
                                            ui.label("Waiting for file selection...");
                                        } else if self.asset_import.is_some() {
                                            ui.label("Reading model data...");
                                        }
                                        if let Some(status) = &self.asset_status {
                                            match status {
                                                Ok(message) => {
                                                    ui.colored_label(
                                                        egui::Color32::LIGHT_GREEN,
                                                        message,
                                                    );
                                                }
                                                Err(message) => {
                                                    ui.colored_label(
                                                        egui::Color32::LIGHT_RED,
                                                        message,
                                                    );
                                                }
                                            }
                                        }
                                        ui.small("Supported formats: .glb, .gltf, .obj");

                                        if world.objects().is_empty()
                                            && world.model_groups().is_empty()
                                            && world.local_lights().is_empty()
                                        {
                                            ui.label("The scene is empty");
                                        } else {
                                            let row_height =
                                                ui.text_style_height(&egui::TextStyle::Button);
                                            ui.label(
                                                egui::RichText::new("HIERARCHY")
                                                    .small()
                                                    .strong(),
                                            );
                                            egui::ScrollArea::vertical()
                                                .id_salt("model-list")
                                                .max_height(180.0)
                                                .show(ui, |ui| {
                                                    let mut children = HashMap::new();
                                                    for &id in world.model_group_ids() {
                                                        children
                                                            .entry(world.model_group_parent(id))
                                                            .or_insert_with(Vec::new)
                                                            .push(id);
                                                    }
                                                    show_model_group_tree(
                                                        ui,
                                                        world,
                                                        &children,
                                                        None,
                                                        &mut selected_scene_item,
                                                    );
                                                    show_local_light_tree(
                                                        ui,
                                                        world,
                                                        None,
                                                        &mut selected_scene_item,
                                                    );
                                                });

                                            let active_group = match selected_scene_item {
                                                Some(SceneSelection::Model(group_id)) => {
                                                    Some(group_id)
                                                }
                                                Some(SceneSelection::Part(object_id)) => world
                                                    .get_object(object_id)
                                                    .and_then(|object| object.model_group()),
                                                Some(SceneSelection::Light(_)) => None,
                                                _ => None,
                                            };
                                            if let Some(group_id) = active_group {
                                                if let Some(group) = world.model_group(group_id) {
                                                    ui.label(format!("{} parts", group.name));
                                                    egui::ScrollArea::vertical()
                                                        .id_salt("model-part-list")
                                                        .max_height(120.0)
                                                        .show_rows(
                                                            ui,
                                                            row_height,
                                                            group.objects.len(),
                                                            |ui, rows| {
                                                                for part_index in rows {
                                                                    let object_id =
                                                                        group.objects[part_index];
                                                                    let Some(object) =
                                                                        world.get_object(object_id)
                                                                    else {
                                                                        continue;
                                                                    };
                                                                    let label =
                                                                        if object.name.is_empty() {
                                                                            object
                                                            .asset_source()
                                                            .map(|source| {
                                                                format!(
                                                                    "Part {}",
                                                                    source.instance_index + 1
                                                                )
                                                            })
                                                            .unwrap_or_else(|| {
                                                                format!("Part {}", part_index + 1)
                                                            })
                                                                        } else {
                                                                            object.name.clone()
                                                                        };
                                                                    if ui
                                                        .selectable_label(
                                                            selected_scene_item
                                                                == Some(SceneSelection::Part(
                                                                    object_id,
                                                                )),
                                                            format!("  {label}"),
                                                        )
                                                        .clicked()
                                                    {
                                                        selected_scene_item =
                                                            Some(SceneSelection::Part(object_id));
                                                    }
                                                                }
                                                            },
                                                        );
                                                }
                                            }

                                            ui.add_space(4.0);
                                            ui.label(
                                                egui::RichText::new("SCENE OBJECTS")
                                                    .small()
                                                    .strong(),
                                            );
                                            egui::ScrollArea::vertical()
                                                .id_salt("standalone-object-list")
                                                .max_height(140.0)
                                                .show_rows(
                                                    ui,
                                                    row_height,
                                                    world.standalone_objects().len(),
                                                    |ui, rows| {
                                                        for row in rows {
                                                            let id =
                                                                world.standalone_objects()[row];
                                                            let Some(object) = world.get_object(id)
                                                            else {
                                                                continue;
                                                            };
                                                            if ui
                                                                .selectable_label(
                                                                    selected_scene_item
                                                                        == Some(
                                                                            SceneSelection::Object(
                                                                                id,
                                                                            ),
                                                                        ),
                                                                    format!(
                                                                        "{} (mesh {}, material {})",
                                                                        if object.name.is_empty() {
                                                                            format!(
                                                                                "Object {}",
                                                                                id.0 + 1
                                                                            )
                                                                        } else {
                                                                            object.name.clone()
                                                                        },
                                                                        object.mesh_id.0,
                                                                        object.material_id.0
                                                                    ),
                                                                )
                                                                .clicked()
                                                            {
                                                                selected_scene_item = Some(
                                                                    SceneSelection::Object(id),
                                                                );
                                                            }
                                                        }
                                                    },
                                                );

                                            ui.add_space(6.0);
                                            egui::CollapsingHeader::new("Inspector")
                                                .default_open(true)
                                                .show(ui, |ui| match selected_scene_item {
                                                    Some(SceneSelection::Model(group_id)) => {
                                                        let parent_before =
                                                            world.model_group_parent(group_id);
                                                        let transform_before = world
                                                            .model_group(group_id)
                                                            .expect("selected model group must exist")
                                                            .transform;
                                                        let mut selected_parent = parent_before;
                                                        let parent_name = selected_parent
                                                            .and_then(|id| {
                                                                world
                                                                    .model_group(id)
                                                                    .map(|group| group.name.clone())
                                                            })
                                                            .unwrap_or_else(|| "Root".to_owned());
                                                        ui.label("Parent node");
                                                        egui::ComboBox::from_id_salt((
                                                            "model-parent",
                                                            group_id.0,
                                                        ))
                                                        .selected_text(parent_name)
                                                        .show_ui(ui, |ui| {
                                                            ui.selectable_value(
                                                                &mut selected_parent,
                                                                None,
                                                                "Root",
                                                            );
                                                            for &candidate in
                                                                world.model_group_ids()
                                                            {
                                                                if world
                                                                    .can_set_model_group_parent(
                                                                        group_id,
                                                                        Some(candidate),
                                                                    )
                                                                {
                                                                    let label = world
                                                                        .model_group(candidate)
                                                                        .map(|group| {
                                                                            group.name.as_str()
                                                                        })
                                                                        .unwrap_or("Model");
                                                                    ui.selectable_value(
                                                                        &mut selected_parent,
                                                                        Some(candidate),
                                                                        label,
                                                                    );
                                                                }
                                                            }
                                                        });
                                                        if selected_parent != parent_before {
                                                            match world
                                                                .reparent_model_group_preserving_world(
                                                                    group_id,
                                                                    selected_parent,
                                                                )
                                                            {
                                                                Ok(()) => {
                                                                    parent_change = Some((
                                                                        group_id,
                                                                        parent_before,
                                                                        transform_before,
                                                                    ));
                                                                    scene_modified = true;
                                                                }
                                                                Err(error) => {
                                                                    self.scene_status =
                                                                        Some(Err(error));
                                                                }
                                                            }
                                                        }
                                                        let is_gltf_node =
                                                            world.gltf_node_index(group_id).is_some();
                                                        if let Some(group) =
                                                            world.model_group_mut(group_id)
                                                        {
                                                            ui.label(if group.source_path.is_some() {
                                                                "Model"
                                                            } else if is_gltf_node {
                                                                "glTF node"
                                                            } else {
                                                                "Empty node"
                                                            });
                                                            let before = group.name.clone();
                                                            if ui
                                                                .text_edit_singleline(
                                                                    &mut group.name,
                                                                )
                                                                .changed()
                                                            {
                                                                name_change = Some((
                                                                    SceneSelection::Model(group_id),
                                                                    before,
                                                                ));
                                                                scene_modified = true;
                                                            }
                                                            ui.separator();
                                                            let before = group.transform;
                                                            if edit_transform(
                                                                ui,
                                                                &mut group.transform,
                                                            ) {
                                                                transform_change = Some((
                                                                    SceneSelection::Model(group_id),
                                                                    before,
                                                                ));
                                                                scene_modified = true;
                                                            }
                                                        }
                                                    }
                                                    Some(
                                                        selection @ (SceneSelection::Part(
                                                            object_id,
                                                        )
                                                        | SceneSelection::Object(
                                                            object_id,
                                                        )),
                                                    ) => {
                                                        if let Some(object) =
                                                            world.get_object_mut(object_id)
                                                        {
                                                            ui.label("Object");
                                                            let before = object.name.clone();
                                                            if ui
                                                                .text_edit_singleline(
                                                                    &mut object.name,
                                                                )
                                                                .changed()
                                                            {
                                                                name_change =
                                                                    Some((selection, before));
                                                                scene_modified = true;
                                                            }
                                                            ui.separator();
                                                            let before = object.transform;
                                                            if edit_transform(
                                                                ui,
                                                                &mut object.transform,
                                                            ) {
                                                                transform_change =
                                                                    Some((selection, before));
                                                                scene_modified = true;
                                                            }
                                                        }
                                                    }
                                                    Some(SceneSelection::Light(_)) => {
                                                        ui.weak(
                                                            "Edit this source in the Lights tab.",
                                                        );
                                                    }
                                                    None => {
                                                        ui.weak("Select an object to inspect it.");
                                                    }
                                                });
                                            if self.selected_scene_item != selected_scene_item {
                                                self.morph_filter.clear();
                                            }
                                            self.selected_scene_item = selected_scene_item;
                                        }
                                    });
                            } else if self.active_tab == EditorTab::Lights {
                                ui.heading("Light sources");
                                if let Some(Err(message)) = &self.scene_status {
                                    ui.colored_label(egui::Color32::LIGHT_RED, message);
                                }
                                ui.label(format!(
                                    "{}/{} in scene",
                                    world.local_lights().len(),
                                    MAX_LOCAL_LIGHTS
                                ));
                                ui.horizontal(|ui| {
                                    if ui
                                        .add_enabled(
                                            world.local_lights().len() < MAX_LOCAL_LIGHTS,
                                            egui::Button::new("Add point"),
                                        )
                                        .clicked()
                                    {
                                        add_local_light = Some(LocalLightKind::Point);
                                    }
                                    if ui
                                        .add_enabled(
                                            world.local_lights().len() < MAX_LOCAL_LIGHTS,
                                            egui::Button::new("Add spot"),
                                        )
                                        .clicked()
                                    {
                                        add_local_light = Some(LocalLightKind::Spot);
                                    }
                                });
                                ui.small(
                                    "Select a light here or in the Scene hierarchy. Move it with the viewport gizmo.",
                                );
                                ui.separator();
                                if world.local_lights().is_empty() {
                                    ui.weak("No lights yet. Add a point light or a spotlight.");
                                }
                                for index in 0..world.local_lights().len() {
                                    let (name, kind) = {
                                        let light = &world.local_lights()[index];
                                        (light.name.clone(), light.kind)
                                    };
                                    ui.horizontal(|ui| {
                                        let kind = match kind {
                                            LocalLightKind::Point => "Point",
                                            LocalLightKind::Spot => "Spot",
                                        };
                                        if ui
                                            .selectable_label(
                                                selected_scene_item
                                                    == Some(SceneSelection::Light(index)),
                                                format!("{kind}: {name}"),
                                            )
                                            .clicked()
                                        {
                                            selected_scene_item =
                                                Some(SceneSelection::Light(index));
                                        }
                                        if let Some(light) = world.local_light_mut(index) {
                                            ui.checkbox(&mut light.enabled, "Enabled");
                                        }
                                    });
                                }

                                if let Some(SceneSelection::Light(index)) = selected_scene_item {
                                    if index < world.local_lights().len() {
                                        ui.separator();
                                        let parent_before = world.local_light_parent(index);
                                        let mut selected_parent = parent_before;
                                        let parent_options: Vec<_> = world
                                            .model_group_ids()
                                            .iter()
                                            .filter_map(|&id| {
                                                world.model_group(id).map(|group| {
                                                    let kind = if group.source_path.is_some() {
                                                        "Model"
                                                    } else if world.gltf_node_index(id).is_some() {
                                                        "glTF node"
                                                    } else {
                                                        "Empty node"
                                                    };
                                                    (id, format!("{kind}: {}", group.name))
                                                })
                                            })
                                            .collect();
                                        let parent_label = parent_before
                                            .and_then(|id| {
                                                parent_options
                                                    .iter()
                                                    .find(|(candidate, _)| *candidate == id)
                                                    .map(|(_, label)| label.as_str())
                                            })
                                            .unwrap_or("World (unparented)");
                                        ui.label("Attach to");
                                        egui::ComboBox::from_id_salt(("light-parent", index))
                                            .selected_text(parent_label)
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut selected_parent,
                                                    None,
                                                    "World (unparented)",
                                                );
                                                for (id, label) in &parent_options {
                                                    ui.selectable_value(
                                                        &mut selected_parent,
                                                        Some(*id),
                                                        label,
                                                    );
                                                }
                                            });
                                        if selected_parent != parent_before {
                                            if let Err(error) = world
                                                .set_local_light_parent(index, selected_parent)
                                            {
                                                selected_parent = parent_before;
                                                self.scene_status = Some(Err(error));
                                            }
                                        }

                                        let mut aim_at_origin = false;
                                        {
                                            let light = world
                                                .local_light_mut(index)
                                                .expect("selected local light must exist");
                                            ui.horizontal(|ui| {
                                                ui.checkbox(&mut light.enabled, "Enabled");
                                                if ui.button("Delete").clicked() {
                                                    delete_selection = true;
                                                }
                                            });
                                            ui.label("Name");
                                            ui.text_edit_singleline(&mut light.name);
                                            egui::ComboBox::from_id_salt(("light-kind", index))
                                                .selected_text(match light.kind {
                                                    LocalLightKind::Point => "Point light",
                                                    LocalLightKind::Spot => "Spotlight",
                                                })
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(
                                                        &mut light.kind,
                                                        LocalLightKind::Point,
                                                        "Point light",
                                                    );
                                                    ui.selectable_value(
                                                        &mut light.kind,
                                                        LocalLightKind::Spot,
                                                        "Spotlight",
                                                    );
                                                });
                                            let position_label = if selected_parent.is_some() {
                                                "Local position"
                                            } else {
                                                "World position"
                                            };
                                            if edit_vec3_fields(
                                                ui,
                                                position_label,
                                                &mut light.position,
                                            ) {
                                                scene_modified = true;
                                            }
                                            ui.horizontal(|ui| {
                                                ui.label("Color");
                                                if ui
                                                    .color_edit_button_rgb(&mut light.color)
                                                    .changed()
                                                {
                                                    scene_modified = true;
                                                }
                                            });
                                            if ui
                                                .add(
                                                    egui::Slider::new(
                                                        &mut light.intensity,
                                                        0.0..=1000.0,
                                                    )
                                                    .text("Intensity"),
                                                )
                                                .changed()
                                            {
                                                scene_modified = true;
                                            }
                                            if ui
                                                .add(
                                                    egui::Slider::new(
                                                        &mut light.range,
                                                        0.1..=1000.0,
                                                    )
                                                    .text("Range"),
                                                )
                                                .changed()
                                            {
                                                scene_modified = true;
                                            }
                                            if light.kind == LocalLightKind::Spot {
                                                if edit_vec3_fields(
                                                    ui,
                                                    "Direction",
                                                    &mut light.direction,
                                                ) {
                                                    scene_modified = true;
                                                }
                                                let direction = Vec3::from_array(light.direction);
                                                if !direction.is_finite()
                                                    || direction.length_squared() <= f32::EPSILON
                                                {
                                                    light.direction = [0.0, -1.0, 0.0];
                                                    ui.colored_label(
                                                        egui::Color32::LIGHT_RED,
                                                        "Zero direction reset downward.",
                                                    );
                                                }
                                                if ui.button("Aim at scene origin").clicked() {
                                                    aim_at_origin = true;
                                                }
                                                let outer_angle = light.outer_cone_degrees;
                                                if ui
                                                    .add(
                                                        egui::Slider::new(
                                                            &mut light.inner_cone_degrees,
                                                            0.0..=outer_angle,
                                                        )
                                                        .text("Inner cone"),
                                                    )
                                                    .changed()
                                                {
                                                    scene_modified = true;
                                                }
                                                let inner_angle = light.inner_cone_degrees;
                                                if ui
                                                    .add(
                                                        egui::Slider::new(
                                                            &mut light.outer_cone_degrees,
                                                            inner_angle..=89.0,
                                                        )
                                                        .text("Outer cone"),
                                                    )
                                                    .changed()
                                                {
                                                    scene_modified = true;
                                                }
                                            }
                                            if !light.is_valid() {
                                                ui.colored_label(
                                                    egui::Color32::LIGHT_RED,
                                                    "Invalid light settings. Check the name and direction.",
                                                );
                                            }
                                        }
                                        if aim_at_origin {
                                            match world.aim_local_light_at(index, Vec3::ZERO) {
                                                Ok(()) => scene_modified = true,
                                                Err(error) => self.scene_status = Some(Err(error)),
                                            }
                                        }
                                        ui.small(
                                            "Move gizmo: position | Rotate gizmo: spotlight direction | Scale gizmo: range",
                                        );
                                    }
                                } else {
                                    ui.weak("Choose a light from the list or Scene hierarchy to edit it.");
                                }
                            } else if self.active_tab == EditorTab::Animation {
                                draw_animation_panel(
                                    ui,
                                    selected_scene_item,
                                    world,
                                    &mut self.morph_filter,
                                    &mut animation_change,
                                    &mut pause_morph_animation,
                                    &mut morph_weights_change,
                                    &mut scene_modified,
                                );
                            } else {
                                egui::CollapsingHeader::new("Performance")
                                    .default_open(false)
                                    .show(ui, |ui| {
                                        ui.small(format!(
                                            "Frame {:.2} ms  |  {:.1} FPS",
                                            frame_time * 1000.0,
                                            fps
                                        ));
                                        ui.small(format!(
                                            "CPU {:.2} ms  |  UI {:.2} ms  |  Scene {:.2} ms",
                                            cpu_frame_time, timings.ui_cpu_ms, timings.scene_cpu_ms
                                        ));
                                        ui.small(format!(
                                            "Acquire {:.2} ms  |  Submit {:.2} ms",
                                            timings.acquire_cpu_ms, timings.encode_submit_cpu_ms
                                        ));
                                        let gpu_text = timings
                                            .gpu_frame_ms
                                            .map_or("unavailable".to_owned(), |time| {
                                                format!("{time:.2} ms")
                                            });
                                        let shadow_text = timings
                                            .gpu_shadow_map_ms
                                            .map_or("unavailable".to_owned(), |time| {
                                                format!("{time:.2} ms")
                                            });
                                        let opaque_text = timings
                                            .gpu_opaque_pass_ms
                                            .map_or("unavailable".to_owned(), |time| {
                                                format!("{time:.2} ms")
                                            });
                                        ui.small(format!(
                                            "GPU render: {gpu_text} | Draw calls: {}",
                                            timings.draw_calls
                                        ));
                                        ui.small(format!(
                                            "GPU passes: shadow map {shadow_text} | opaque {opaque_text}"
                                        ));
                                        ui.small(format!(
                                            "Triangles: {} | Meshes: {} | Materials: {}",
                                            timings.triangle_count,
                                            timings.active_meshes,
                                            timings.active_materials
                                        ));
                                        ui.small(format!(
                                            "Batches: {} | Instances: {}",
                                            timings.render_batches,
                                            timings.active_instances
                                        ));
                                        if let Some(progress) = self
                                            .profile_capture
                                            .as_ref()
                                            .map(ProfileCapture::progress_text)
                                        {
                                            ui.small(progress);
                                            if ui.button("Cancel profile capture").clicked() {
                                                self.profile_capture = None;
                                                self.profile_status =
                                                    Some(Ok("Profile capture cancelled".to_owned()));
                                            }
                                        } else if ui.button("Capture current view (10 s)").clicked()
                                        {
                                            self.profile_capture = Some(ProfileCapture::new(
                                                camera,
                                                renderer,
                                                world,
                                                self.scene_path.clone(),
                                                timings,
                                            ));
                                            self.profile_status =
                                                Some(Ok("Profile capture started".to_owned()));
                                        }
                                        if let Some(status) = &self.profile_status {
                                            match status {
                                                Ok(message) => {
                                                    ui.small(message);
                                                }
                                                Err(error) => {
                                                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                                                }
                                            }
                                        }
                                    });
                                ui.collapsing("Render settings", |ui| {
                                    ui.add(
                                        egui::Slider::new(
                                            &mut settings.key_light_yaw_degrees,
                                            -180.0..=180.0,
                                        )
                                        .text("Key light yaw"),
                                    );
                                    ui.add(
                                        egui::Slider::new(
                                            &mut settings.key_light_elevation_degrees,
                                            -89.0..=-5.0,
                                        )
                                        .text("Key light elevation"),
                                    );
                                    ui.add(
                                        egui::Slider::new(
                                            &mut settings.key_light_intensity,
                                            0.0..=8.0,
                                        )
                                        .text("Key light"),
                                    );
                                    ui.add(
                                        egui::Slider::new(
                                            &mut settings.fill_light_intensity,
                                            0.0..=4.0,
                                        )
                                        .text("Fill light"),
                                    );
                                    ui.add(
                                        egui::Slider::new(
                                            &mut settings.environment_intensity,
                                            0.0..=2.0,
                                        )
                                        .text("Environment"),
                                    );
                                    ui.add(
                                        egui::Slider::new(&mut settings.exposure_ev, -10.0..=3.0)
                                            .text("Exposure (EV)"),
                                    );
                                    ui.checkbox(
                                        &mut settings.shadows_enabled,
                                        "Directional shadows",
                                    );
                                    egui::ComboBox::from_label("Shadow filtering")
                                        .selected_text(match settings.shadow_pcf_samples {
                                            2 => "2 taps (fastest)",
                                            4 => "4 taps (faster)",
                                            8 => "8 taps (balanced)",
                                            _ => "16 taps (smoothest)",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut settings.shadow_pcf_samples,
                                                2,
                                                "2 taps (fastest)",
                                            );
                                            ui.selectable_value(
                                                &mut settings.shadow_pcf_samples,
                                                4,
                                                "4 taps (faster)",
                                            );
                                            ui.selectable_value(
                                                &mut settings.shadow_pcf_samples,
                                                8,
                                                "8 taps (balanced)",
                                            );
                                            ui.selectable_value(
                                                &mut settings.shadow_pcf_samples,
                                                16,
                                                "16 taps (smoothest)",
                                            );
                                        });
                                    ui.checkbox(
                                        &mut settings.unlocked_fps,
                                        "Unlock FPS (disable VSync)",
                                    );
                                    ui.add(
                                        egui::Slider::new(&mut settings.render_scale, 0.5..=1.0)
                                            .step_by(0.05)
                                            .text("Render resolution scale"),
                                    );
                                    egui::ComboBox::from_label("Anti-aliasing (MSAA)")
                                        .selected_text(if settings.msaa_samples == 1 {
                                            "Off".to_owned()
                                        } else {
                                            format!("{}x", settings.msaa_samples)
                                        })
                                        .show_ui(ui, |ui| {
                                            for &sample_count in renderer.supported_msaa_samples() {
                                                let label = if sample_count == 1 {
                                                    "Off".to_owned()
                                                } else {
                                                    format!("{sample_count}x")
                                                };
                                                ui.selectable_value(
                                                    &mut settings.msaa_samples,
                                                    sample_count,
                                                    label,
                                                );
                                            }
                                        });
                                    if ui.button("Reset lighting").clicked() {
                                        let defaults =
                                            crate::renderer::state::RendererSettings::default();
                                        settings.key_light_yaw_degrees =
                                            defaults.key_light_yaw_degrees;
                                        settings.key_light_elevation_degrees =
                                            defaults.key_light_elevation_degrees;
                                        settings.key_light_intensity = defaults.key_light_intensity;
                                        settings.fill_light_intensity =
                                            defaults.fill_light_intensity;
                                        settings.environment_intensity =
                                            defaults.environment_intensity;
                                        settings.exposure_ev = defaults.exposure_ev;
                                        settings.shadows_enabled = defaults.shadows_enabled;
                                        settings.shadow_pcf_samples =
                                            defaults.shadow_pcf_samples;
                                        settings.debug_view = defaults.debug_view;
                                        settings.render_scale = defaults.render_scale;
                                        if renderer
                                            .supported_msaa_samples()
                                            .contains(&defaults.msaa_samples)
                                        {
                                            settings.msaa_samples = defaults.msaa_samples;
                                        }
                                    }
                                    egui::ComboBox::from_label("Debug view")
                                        .selected_text(
                                            [
                                                "Lit",
                                                "World normals",
                                                "Base color",
                                                "Roughness / metallic",
                                                "Diffuse IBL",
                                                "Specular IBL",
                                            ]
                                            .get(settings.debug_view as usize)
                                            .copied()
                                            .unwrap_or("Lit"),
                                        )
                                        .show_ui(ui, |ui| {
                                            for (index, label) in [
                                                "Lit",
                                                "World normals",
                                                "Base color",
                                                "Roughness / metallic",
                                                "Diffuse IBL",
                                                "Specular IBL",
                                            ]
                                            .iter()
                                            .enumerate()
                                            {
                                                ui.selectable_value(
                                                    &mut settings.debug_view,
                                                    index as u32,
                                                    *label,
                                                );
                                            }
                                        });
                                });

                                ui.collapsing("Environment & lighting", |ui| {
                                    ui.label(format!(
                                        "Active: {} ({} mips)",
                                        renderer.environment_name(),
                                        renderer.environment_mip_count()
                                    ));
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add_enabled(
                                                self.environment_dialog.is_none()
                                                    && !renderer.environment_is_loading(),
                                                egui::Button::new("Browse..."),
                                            )
                                            .clicked()
                                        {
                                            pick_environment = true;
                                        }
                                        if ui
                                            .add_enabled(
                                                !renderer.environment_is_loading(),
                                                egui::Button::new("Load .hdr"),
                                            )
                                            .clicked()
                                        {
                                            load_environment = true;
                                        }
                                    });
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.environment_path)
                                            .desired_width(ui.available_width())
                                            .hint_text("Path to .hdr / .exr environment"),
                                    );
                                    if renderer.environment_is_loading() {
                                        ui.spinner();
                                        ui.label("Preparing HDR environment...");
                                    } else if self.environment_dialog.is_some() {
                                        ui.label("Waiting for file selection...");
                                    }
                                    ui.label("Equirectangular .hdr / .exr environment (2:1)");
                                    if let Some(status) = &self.status {
                                        match status {
                                            Ok(message) => {
                                                ui.colored_label(
                                                    egui::Color32::LIGHT_GREEN,
                                                    message,
                                                );
                                            }
                                            Err(message) => {
                                                ui.colored_label(egui::Color32::LIGHT_RED, message);
                                            }
                                        }
                                    }
                                });

                                ui.collapsing("Material inspector", |ui| {
                                    if material_count == 0 {
                                        ui.label("No materials loaded");
                                    } else {
                                        egui::ComboBox::from_label("Material")
                                            .selected_text(
                                                renderer
                                                    .material_label(self.selected_material)
                                                    .unwrap_or(""),
                                            )
                                            .show_ui(ui, |ui| {
                                                for index in 0..material_count {
                                                    let label = renderer
                                                        .material_label(index)
                                                        .unwrap_or("");
                                                    ui.selectable_value(
                                                        &mut self.selected_material,
                                                        index,
                                                        format!("{index}: {label}"),
                                                    );
                                                }
                                            });
                                        ui.add(
                                            egui::Slider::new(
                                                &mut selected_params.roughness,
                                                0.045..=1.0,
                                            )
                                            .text("Roughness"),
                                        );
                                        ui.add(
                                            egui::Slider::new(
                                                &mut selected_params.metallic,
                                                0.0..=1.0,
                                            )
                                            .text("Metallic"),
                                        );
                                        if selected_params.transmission_factor > 0.0 {
                                            ui.label(
                                    "Glass material: screen-space refraction; metallic is ignored",
                                );
                                        } else if selected_params.unlit > 0.5 {
                                            ui.label("Unlit material: PBR controls have no effect");
                                        }
                                    }
                                });
                            }
                        });
                    ui.separator();
                    ui.small("LMB: select/drag | RMB: look | Esc: release camera");
                });

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(context, |ui| {
                    let viewport_rect = ui.max_rect();
                    self.viewport_rect = Some(viewport_rect);
                    let had_gizmo_drag = self.gizmo_drag.is_some();
                    let gizmo_changed = paint_scene_gizmo(
                        ui,
                        viewport_rect,
                        context.screen_rect(),
                        camera,
                        world,
                        &mut selected_scene_item,
                        self.gizmo_mode,
                        &mut self.gizmo_drag,
                    );
                    scene_modified |= gizmo_changed;
                    if !had_gizmo_drag && self.gizmo_drag.is_some() {
                        self.history.record(
                            world,
                            selected_scene_item,
                            &RenderSnapshot::capture(renderer),
                        );
                    }
                });

            if self.visible && self.show_camera_settings {
                egui::Window::new("Camera controls")
                    .default_width(300.0)
                    .resizable(false)
                    .open(&mut self.show_camera_settings)
                    .show(context, |ui| {
                        ui.label("Viewport navigation");
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(
                                    self.camera_control_mode == CameraControlMode::ClickToCapture,
                                    "Click to capture",
                                )
                                .clicked()
                            {
                                self.camera_control_mode = CameraControlMode::ClickToCapture;
                            }
                            if ui
                                .selectable_label(
                                    self.camera_control_mode == CameraControlMode::HoldMouseButton,
                                    "Hold mouse",
                                )
                                .clicked()
                            {
                                self.camera_control_mode = CameraControlMode::HoldMouseButton;
                            }
                        });
                        if self.camera_control_mode == CameraControlMode::ClickToCapture {
                            ui.label("Click the viewport to capture the cursor.");
                            ui.label("Press Esc to release it.");
                        } else {
                            ui.horizontal(|ui| {
                                ui.label("Look while holding");
                                egui::ComboBox::from_id_salt("camera-look-button")
                                    .selected_text(match self.camera_look_button {
                                        CameraLookButton::Left => "LMB",
                                        CameraLookButton::Right => "RMB",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut self.camera_look_button,
                                            CameraLookButton::Right,
                                            "Right mouse button",
                                        );
                                        ui.selectable_value(
                                            &mut self.camera_look_button,
                                            CameraLookButton::Left,
                                            "Left mouse button",
                                        );
                                    });
                            });
                            ui.label("The cursor returns when the button is released.");
                        }
                        ui.separator();
                        ui.label("Move: W A S D");
                        ui.label("Up / down: Space / Shift");
                    });
            }
        });
        let local_light_collection_change_requested = add_local_light.is_some();
        if let Some(kind) = add_local_light {
            let forward = Vec3::new(
                camera.yaw.cos() * camera.pitch.cos(),
                camera.pitch.sin(),
                camera.yaw.sin() * camera.pitch.cos(),
            )
            .normalize();
            let position = (camera.eye + forward * 3.0).to_array();
            let name = match kind {
                LocalLightKind::Point => format!("Point Light {}", world.local_lights().len() + 1),
                LocalLightKind::Spot => format!("Spot Light {}", world.local_lights().len() + 1),
            };
            let light = match kind {
                LocalLightKind::Point => LocalLight::point(name, position),
                LocalLightKind::Spot => LocalLight::spot(name, position, forward.to_array()),
            };
            match world.add_local_light(light) {
                Ok(index) => selected_scene_item = Some(SceneSelection::Light(index)),
                Err(error) => self.scene_status = Some(Err(error)),
            }
        }
        let local_lights_after = world.local_lights().to_vec();
        let local_lights_changed = local_lights_after != local_lights_before;
        if local_lights_changed {
            world.set_local_lights(local_lights_before.clone());
        }
        self.selected_scene_item = selected_scene_item;
        if let Some(group_id) = pause_morph_animation
            && let Some(group) = world.model_group_mut(group_id)
            && let Some(playback) = group.animation.as_mut()
        {
            animation_change = Some((group_id, *playback));
            playback.playing = false;
            scene_modified = true;
        }
        let render_state_modified =
            settings != settings_before || selected_params != selected_params_before;
        let mut changed_name = None;
        let mut changed_animation = None;
        let mut changed_parent = None;
        let mut changed_morph_weights = None;
        if let Some((selection, before)) = name_change
            && let Some(name) = selected_name_mut(world, selection)
        {
            changed_name = Some((selection, name.clone()));
            *name = before;
        }
        if let Some((group_id, before)) = animation_change
            && let Some(after) = world
                .model_group(group_id)
                .and_then(|group| group.animation)
        {
            if let Some(group) = world.model_group_mut(group_id) {
                group.animation = Some(before);
            }
            changed_animation = Some((group_id, after));
        }
        if let Some((group_id, before_parent, before_transform)) = parent_change {
            let after = world.model_group_parent(group_id);
            let after_transform = world
                .model_group(group_id)
                .expect("reparented model group must remain available")
                .transform;
            if after != before_parent {
                world
                    .set_model_group_parent(group_id, before_parent)
                    .expect("previous model parent must remain valid");
                world
                    .model_group_mut(group_id)
                    .expect("reparented model group must remain available")
                    .transform = before_transform;
                changed_parent = Some((group_id, after, after_transform));
            }
        }
        if let Some((selection, before)) = morph_weights_change {
            let (SceneSelection::Part(object_id) | SceneSelection::Object(object_id)) = selection
            else {
                unreachable!("morph weights only belong to scene objects");
            };
            if let Some(object) = world.get_object_mut(object_id) {
                let after = object.morph_weights().to_vec();
                object.set_morph_weights(&before);
                changed_morph_weights = Some((object_id, after));
            }
        }
        let other_undoable_change = render_state_modified
            || changed_name.is_some()
            || changed_animation.is_some()
            || changed_parent.is_some()
            || changed_morph_weights.is_some();
        if other_undoable_change {
            self.history.record(
                world,
                selected_scene_item,
                &RenderSnapshot::capture(renderer),
            );
        } else if local_lights_changed {
            if local_light_collection_change_requested {
                self.history.record(
                    world,
                    selected_scene_item,
                    &RenderSnapshot::capture(renderer),
                );
            } else if let Some(SceneSelection::Light(index)) = selected_scene_item {
                let pointer_down = self.context.input(|input| input.pointer.any_down());
                self.history.record_local_light(
                    world,
                    selected_scene_item,
                    index,
                    pointer_down,
                    &RenderSnapshot::capture(renderer),
                );
            } else {
                self.history.record(
                    world,
                    selected_scene_item,
                    &RenderSnapshot::capture(renderer),
                );
            }
        }
        if let Some((selection, after)) = changed_name
            && let Some(name) = selected_name_mut(world, selection)
        {
            *name = after;
        }
        if let Some((group_id, after)) = changed_animation
            && let Some(group) = world.model_group_mut(group_id)
        {
            group.animation = Some(after);
        }
        if let Some((object_id, after)) = changed_morph_weights
            && let Some(object) = world.get_object_mut(object_id)
        {
            object.set_morph_weights(&after);
        }
        if let Some((group_id, after_parent, after_transform)) = changed_parent {
            world
                .set_model_group_parent(group_id, after_parent)
                .expect("new model parent must remain valid");
            world
                .model_group_mut(group_id)
                .expect("reparented model group must remain available")
                .transform = after_transform;
        }
        if local_lights_changed {
            world.set_local_lights(local_lights_after);
            scene_modified = true;
        }
        scene_modified |= render_state_modified;
        if render_state_modified {
            renderer.set_settings(settings);
            if material_count > 0 {
                renderer.set_material_params(self.selected_material, selected_params);
            }
        }
        if save_current {
            self.scene_save_request = Some(PathBuf::from(&self.scene_path));
            self.scene_status = None;
        }
        if parent_change.is_none()
            && let Some((transform_selection, before)) = transform_change
        {
            if let Some(after) = selected_transform_mut(world, Some(transform_selection)).copied() {
                if let Some(transform) = selected_transform_mut(world, Some(transform_selection)) {
                    *transform = before;
                }
                let pointer_down = self.context.input(|input| input.pointer.any_down());
                self.history.record_transform(
                    world,
                    selected_scene_item,
                    transform_selection,
                    pointer_down,
                    &RenderSnapshot::capture(renderer),
                );
                if let Some(transform) = selected_transform_mut(world, Some(transform_selection)) {
                    *transform = after;
                }
            }
        }
        if !self.context.input(|input| input.pointer.any_down()) {
            self.history.active_transform_selection = None;
            self.history.active_local_light = None;
        }
        if undo_requested {
            let mut render_state = RenderSnapshot::capture(renderer);
            let undone = self
                .history
                .undo(world, &mut selected_scene_item, &mut render_state);
            scene_modified |= undone;
            if undone {
                render_state.apply(renderer);
                settings = render_state.settings;
                if let Some(params) = render_state.materials.get(self.selected_material) {
                    selected_params = *params;
                }
            }
            self.gizmo_drag = None;
        } else if redo_requested {
            let mut render_state = RenderSnapshot::capture(renderer);
            let redone = self
                .history
                .redo(world, &mut selected_scene_item, &mut render_state);
            scene_modified |= redone;
            if redone {
                render_state.apply(renderer);
                settings = render_state.settings;
                if let Some(params) = render_state.materials.get(self.selected_material) {
                    selected_params = *params;
                }
            }
            self.gizmo_drag = None;
        }
        if add_cube {
            self.history.record(
                world,
                selected_scene_item,
                &RenderSnapshot::capture(renderer),
            );
            let forward = Vec3::new(
                camera.yaw.cos() * camera.pitch.cos(),
                camera.pitch.sin(),
                camera.yaw.sin() * camera.pitch.cos(),
            )
            .normalize();
            let id = world.add_object(
                RenderObject::new(
                    Transform::from_translation(camera.eye + forward * 4.0),
                    MESH_CUBE,
                    MATERIAL_DEFAULT,
                )
                .with_name("Cube"),
            );
            selected_scene_item = Some(SceneSelection::Object(id));
            scene_modified = true;
        }
        if add_empty_node {
            self.history.record(
                world,
                selected_scene_item,
                &RenderSnapshot::capture(renderer),
            );
            let forward = Vec3::new(
                camera.yaw.cos() * camera.pitch.cos(),
                camera.pitch.sin(),
                camera.yaw.sin() * camera.pitch.cos(),
            )
            .normalize();
            let id = world.add_empty_node(
                "Empty node",
                Transform::from_translation(camera.eye + forward * 4.0),
            );
            selected_scene_item = Some(SceneSelection::Model(id));
            scene_modified = true;
        }
        if duplicate_selection {
            if selected_scene_item.is_some() {
                self.history.record(
                    world,
                    selected_scene_item,
                    &RenderSnapshot::capture(renderer),
                );
                let duplicate = match selected_scene_item {
                    Some(SceneSelection::Model(id)) => {
                        world.duplicate_model_group(id).map(SceneSelection::Model)
                    }
                    Some(SceneSelection::Part(id)) => {
                        world.duplicate_object(id).map(SceneSelection::Part)
                    }
                    Some(SceneSelection::Object(id)) => {
                        world.duplicate_object(id).map(SceneSelection::Object)
                    }
                    Some(SceneSelection::Light(index)) => world
                        .duplicate_local_light(index)
                        .map(SceneSelection::Light),
                    None => None,
                };
                scene_modified |= duplicate.is_some();
                selected_scene_item = duplicate;
            }
        }
        if delete_selection {
            if selected_scene_item.is_some() {
                self.history.record(
                    world,
                    selected_scene_item,
                    &RenderSnapshot::capture(renderer),
                );
                match selected_scene_item {
                    Some(SceneSelection::Model(id)) => {
                        world.remove_model_group(id);
                    }
                    Some(SceneSelection::Part(id) | SceneSelection::Object(id)) => {
                        world.remove_object(id);
                    }
                    Some(SceneSelection::Light(index)) => {
                        world.remove_local_light(index);
                    }
                    None => {}
                }
                selected_scene_item = None;
                self.gizmo_drag = None;
                scene_modified = true;
            }
        }
        if scene_modified {
            self.mark_scene_dirty();
        }
        self.selected_scene_item = selected_scene_item;
        self.platform
            .handle_platform_output(window, output.platform_output);

        if pick_asset {
            match Self::open_file_dialog(window, "3D models", &["glb", "gltf", "obj"], "model") {
                Ok(receiver) => {
                    self.asset_dialog = Some(receiver);
                    self.asset_status = None;
                }
                Err(error) => {
                    self.asset_status =
                        Some(Err(format!("Could not open model file dialog: {error}")));
                }
            }
        }
        if pick_environment {
            match Self::open_file_dialog(window, "HDR environment", &["hdr", "exr"], "environment")
            {
                Ok(receiver) => {
                    self.environment_dialog = Some(receiver);
                    self.status = None;
                }
                Err(error) => {
                    self.status = Some(Err(format!(
                        "Could not open HDR environment file dialog: {error}"
                    )));
                }
            }
        }
        if pick_scene_to_load {
            match Self::open_file_dialog(
                window,
                "BoneSmashing scenes",
                &["bse-scene", "json"],
                "scene-open",
            ) {
                Ok(receiver) => {
                    self.scene_load_dialog = Some(receiver);
                    self.scene_status = None;
                }
                Err(error) => {
                    self.scene_status =
                        Some(Err(format!("Could not open scene file dialog: {error}")));
                }
            }
        }
        if pick_scene_to_save {
            match Self::save_file_dialog(window) {
                Ok(receiver) => {
                    self.scene_save_dialog = Some(receiver);
                    self.scene_status = None;
                }
                Err(error) => {
                    self.scene_status =
                        Some(Err(format!("Could not open scene save dialog: {error}")));
                }
            }
        }
        renderer.set_settings(settings);
        if material_count > 0 {
            renderer.set_material_params(self.selected_material, selected_params);
        }
        if load_environment && !self.environment_path.trim().is_empty() {
            match renderer
                .begin_environment_load(Path::new(self.environment_path.trim()).to_path_buf())
            {
                Ok(()) => self.status = Some(Ok("Loading HDR environment...".to_owned())),
                Err(error) => self.status = Some(Err(error)),
            }
        }

        let size = window.inner_size();
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [size.width.max(1), size.height.max(1)],
            pixels_per_point: output.pixels_per_point,
        };
        let paint_jobs = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        self.prepared = Some(PreparedFrame {
            paint_jobs,
            textures_delta: output.textures_delta,
            screen,
        });
    }

    fn open_file_dialog(
        window: &Window,
        filter: &str,
        extensions: &[&str],
        thread_name: &str,
    ) -> std::io::Result<mpsc::Receiver<Option<PathBuf>>> {
        let (sender, receiver) = mpsc::channel();
        let dialog = rfd::FileDialog::new()
            .add_filter(filter, extensions)
            .set_parent(window);
        std::thread::Builder::new()
            .name(format!("{thread_name}-file-dialog"))
            .spawn(move || {
                let _ = sender.send(dialog.pick_file());
            })?;
        Ok(receiver)
    }

    fn parse_asset_in_background(
        path: PathBuf,
    ) -> std::io::Result<mpsc::Receiver<(PathBuf, Result<ParsedAsset, String>)>> {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("model-import".to_owned())
            .spawn(move || {
                let result = parse_asset(&path);
                let _ = sender.send((path, result));
            })?;
        Ok(receiver)
    }

    fn load_scene_in_background(
        path: PathBuf,
    ) -> std::io::Result<mpsc::Receiver<Result<LoadedScene, String>>> {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("scene-load".to_owned())
            .spawn(move || {
                let _ = sender.send(load_scene(&path));
            })?;
        Ok(receiver)
    }

    fn save_file_dialog(window: &Window) -> std::io::Result<mpsc::Receiver<Option<PathBuf>>> {
        let (sender, receiver) = mpsc::channel();
        let dialog = rfd::FileDialog::new()
            .add_filter("BoneSmashing scenes", &["bse-scene", "json"])
            .set_file_name("scene.bse-scene")
            .set_parent(window);
        std::thread::Builder::new()
            .name("scene-save-file-dialog".to_owned())
            .spawn(move || {
                let _ = sender.send(dialog.save_file());
            })?;
        Ok(receiver)
    }

    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
    ) -> Vec<wgpu::CommandBuffer> {
        if !self.visible {
            self.prepared = None;
            return Vec::new();
        }
        let Some(prepared) = self.prepared.take() else {
            return Vec::new();
        };
        for (id, delta) in &prepared.textures_delta.set {
            self.renderer.update_texture(device, queue, *id, delta);
        }
        let user_commands = self.renderer.update_buffers(
            device,
            queue,
            encoder,
            &prepared.paint_jobs,
            &prepared.screen,
        );
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("debug UI overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.renderer.render(
                &mut pass.forget_lifetime(),
                &prepared.paint_jobs,
                &prepared.screen,
            );
        }
        for id in &prepared.textures_delta.free {
            self.renderer.free_texture(id);
        }
        user_commands
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{MaterialId, MeshId};
    use crate::scene::{ModelAnimationClip, ModelAnimationPlayback, RenderObject};

    fn test_render_state() -> RenderSnapshot {
        RenderSnapshot {
            settings: RendererSettings::default(),
            materials: vec![MaterialParams::default()],
        }
    }

    #[test]
    fn undo_redo_restores_local_light_edits() {
        let mut world = World::new();
        world
            .add_local_light(LocalLight::point("Key", [0.0, 1.0, 0.0]))
            .unwrap();
        let mut history = UndoHistory::default();
        history.record(&world, None, &test_render_state());

        world.local_light_mut(0).unwrap().intensity = 24.0;
        let mut selection = None;
        let mut render_state = test_render_state();
        assert!(history.undo(&mut world, &mut selection, &mut render_state));
        assert_eq!(world.local_lights()[0].intensity, 12.0);
        assert!(history.redo(&mut world, &mut selection, &mut render_state));
        assert_eq!(world.local_lights()[0].intensity, 24.0);
    }

    #[test]
    fn local_light_drag_is_one_undoable_change() {
        let mut world = World::new();
        world
            .add_local_light(LocalLight::point("Key", [0.0, 1.0, 0.0]))
            .unwrap();
        let mut history = UndoHistory::default();
        let render_state = test_render_state();
        let selection = Some(SceneSelection::Light(0));

        history.record_local_light(&world, selection, 0, true, &render_state);
        world.local_light_mut(0).unwrap().position[0] = 1.0;
        history.record_local_light(&world, selection, 0, true, &render_state);
        world.local_light_mut(0).unwrap().position[0] = 2.0;

        let mut selected = selection;
        let mut render_state = test_render_state();
        assert!(history.undo(&mut world, &mut selected, &mut render_state));
        assert_eq!(world.local_lights()[0].position, [0.0, 1.0, 0.0]);
        assert!(history.redo(&mut world, &mut selected, &mut render_state));
        assert_eq!(world.local_lights()[0].position, [2.0, 1.0, 0.0]);
    }

    #[test]
    fn world_space_gizmo_move_respects_mirrored_parent_transforms() {
        let mut world = World::new();
        let parent = world.add_empty_node(
            "Mirrored parent",
            Transform::new(Vec3::ZERO, Quat::IDENTITY, Vec3::new(-1.0, 1.0, 1.0)),
        );
        let child = world.add_empty_node("Child", Transform::default());
        world.set_model_group_parent(child, Some(parent)).unwrap();

        let parent_matrix =
            selected_parent_world_matrix(&world, Some(SceneSelection::Model(child)));
        let local_delta = parent_matrix.inverse().transform_vector3(Vec3::X);
        world.model_group_mut(child).unwrap().transform.position += local_delta;

        let world_position = world
            .model_group_world_matrix(child)
            .transform_point3(Vec3::ZERO);
        assert!(world_position.abs_diff_eq(Vec3::X, 1e-5));
    }

    #[test]
    fn rotation_ring_orientation_matches_its_axis() {
        for axis in 0..3 {
            let (u, v) = rotation_ring_basis(axis);
            assert!(u.cross(v).dot(axis_vector(axis)) > 0.999);
        }
    }

    #[test]
    fn viewport_picking_prefers_the_object_under_the_pointer_to_the_floor() {
        let camera = Camera {
            eye: Vec3::new(0.0, 2.0, 6.0),
            yaw: -90.0_f32.to_radians(),
            pitch: -15.0_f32.to_radians(),
            up: Vec3::Y,
            aspect: 16.0 / 9.0,
        };
        let mut world = World::new();
        world.add_object(RenderObject::new(
            Transform::from_translation(Vec3::new(0.0, -1.0, 0.0)),
            MeshId(0),
            MaterialId(0),
        ));
        world.add_object(RenderObject::new(
            Transform::new(
                Vec3::new(0.0, -0.32, 0.0),
                Quat::IDENTITY,
                Vec3::splat(0.68),
            ),
            MeshId(1),
            MaterialId(0),
        ));

        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));
        let sphere_center = project_to_screen(
            Vec3::new(0.0, -0.32, 0.0),
            camera.build_view_projection_matrix(),
            screen_rect,
        )
        .unwrap();

        let sphere_id = world.object_id_at(1).unwrap();
        assert!(matches!(
            pick_scene_object(sphere_center, screen_rect, &camera, &world),
            Some(SceneSelection::Part(id)) if id == sphere_id
        ));
    }

    #[test]
    fn undo_redo_restores_objects_without_reusing_ids() {
        let mut world = World::new();
        world.add_object(RenderObject::new(
            Transform::default(),
            MeshId(0),
            MaterialId(0),
        ));
        let mut history = UndoHistory::default();
        let mut render_state = test_render_state();
        history.record(&world, None, &render_state);

        let group =
            world.add_model_group("Model", PathBuf::from("model.glb"), Transform::default());
        let added = world
            .add_group_object(
                group,
                RenderObject::new(Transform::default(), MeshId(1), MaterialId(0)),
            )
            .unwrap();
        let mut selection = Some(SceneSelection::Part(added));

        assert!(history.undo(&mut world, &mut selection, &mut render_state));
        assert!(world.get_object(added).is_none());
        assert!(world.model_group(group).is_none());
        assert_eq!(world.objects().len(), 1);
        assert!(selection.is_none());

        assert!(history.redo(&mut world, &mut selection, &mut render_state));
        assert!(world.get_object(added).is_some());
        assert!(world.model_group(group).is_some());
        assert!(selection == Some(SceneSelection::Part(added)));

        assert!(history.undo(&mut world, &mut selection, &mut render_state));
        let branched_group = world.add_model_group(
            "Branched",
            PathBuf::from("branched.glb"),
            Transform::default(),
        );
        let branched = world.add_object(RenderObject::new(
            Transform::default(),
            MeshId(2),
            MaterialId(0),
        ));
        assert_ne!(branched, added);
        assert_ne!(branched_group, group);
    }

    #[test]
    fn undo_redo_restores_transform_changes() {
        let mut world = World::new();
        let id = world.add_object(RenderObject::new(
            Transform::default(),
            MeshId(0),
            MaterialId(0),
        ));
        let mut history = UndoHistory::default();
        let mut render_state = test_render_state();
        history.record_transform(
            &world,
            Some(SceneSelection::Object(id)),
            SceneSelection::Object(id),
            false,
            &render_state,
        );
        world.get_object_mut(id).unwrap().transform.position = Vec3::X;
        let mut selection = Some(SceneSelection::Object(id));

        assert!(history.undo(&mut world, &mut selection, &mut render_state));
        assert_eq!(world.get_object(id).unwrap().transform.position, Vec3::ZERO);
        assert!(history.redo(&mut world, &mut selection, &mut render_state));
        assert_eq!(world.get_object(id).unwrap().transform.position, Vec3::X);
    }

    #[test]
    fn recording_after_undo_clears_the_redo_branch() {
        let mut world = World::new();
        world.add_object(RenderObject::new(
            Transform::default(),
            MeshId(0),
            MaterialId(0),
        ));
        let mut history = UndoHistory::default();
        let mut render_state = test_render_state();
        history.record(&world, None, &render_state);
        world.add_object(RenderObject::new(
            Transform::default(),
            MeshId(1),
            MaterialId(0),
        ));
        let mut selection = None;

        assert!(history.undo(&mut world, &mut selection, &mut render_state));
        assert!(history.can_redo());
        history.record(&world, selection, &render_state);
        assert!(!history.can_redo());
    }

    #[test]
    fn undo_redo_restores_model_group_parent_changes() {
        let mut world = World::new();
        let parent = world.add_empty_node(
            "Parent",
            Transform::from_translation(Vec3::new(1.0, 0.0, 0.0)),
        );
        let child = world.add_empty_node("Child", Transform::from_translation(Vec3::Y));
        let selection = Some(SceneSelection::Model(child));
        let mut history = UndoHistory::default();
        let mut render_state = test_render_state();
        history.record(&world, selection, &render_state);
        world
            .reparent_model_group_preserving_world(child, Some(parent))
            .unwrap();
        let parented_transform = world.model_group(child).unwrap().transform;
        let mut selection = selection;

        assert!(history.undo(&mut world, &mut selection, &mut render_state));
        assert_eq!(world.model_group_parent(child), None);
        assert_eq!(
            world.model_group(child).unwrap().transform.position,
            Vec3::Y
        );
        assert!(history.redo(&mut world, &mut selection, &mut render_state));
        assert_eq!(world.model_group_parent(child), Some(parent));
        assert_eq!(
            world.model_group(child).unwrap().transform.position,
            parented_transform.position
        );
    }

    #[test]
    fn undo_redo_restores_render_settings_and_material_parameters() {
        let mut world = World::new();
        let mut history = UndoHistory::default();
        let mut render_state = test_render_state();
        history.record(&world, None, &render_state);

        render_state.settings.exposure_ev = 2.0;
        render_state.materials[0].metallic = 0.9;
        let mut selection = None;

        assert!(history.undo(&mut world, &mut selection, &mut render_state));
        assert_eq!(render_state.settings, RendererSettings::default());
        assert_eq!(render_state.materials[0].metallic, 0.0);

        assert!(history.redo(&mut world, &mut selection, &mut render_state));
        assert_eq!(render_state.settings.exposure_ev, 2.0);
        assert_eq!(render_state.materials[0].metallic, 0.9);
    }

    #[test]
    fn undo_redo_restores_renamed_objects() {
        let mut world = World::new();
        let id = world.add_object(
            RenderObject::new(Transform::default(), MeshId(0), MaterialId(0)).with_name("Cube"),
        );
        let mut history = UndoHistory::default();
        let render_state = test_render_state();
        history.record(&world, Some(SceneSelection::Object(id)), &render_state);
        world.get_object_mut(id).unwrap().name = "Named cube".to_owned();
        let mut selection = Some(SceneSelection::Object(id));
        let mut render_state = render_state;

        assert!(history.undo(&mut world, &mut selection, &mut render_state));
        assert_eq!(world.get_object(id).unwrap().name, "Cube");
        assert!(history.redo(&mut world, &mut selection, &mut render_state));
        assert_eq!(world.get_object(id).unwrap().name, "Named cube");
    }

    #[test]
    fn undo_redo_restores_animation_playback_settings() {
        let mut world = World::new();
        let group =
            world.add_model_group("Animated", PathBuf::from("model.glb"), Transform::default());
        let group_data = world.model_group_mut(group).unwrap();
        group_data.animation_clips = vec![
            ModelAnimationClip {
                name: "Idle".to_owned(),
                duration: 1.0,
            },
            ModelAnimationClip {
                name: "Walk".to_owned(),
                duration: 2.0,
            },
        ];
        group_data.animation = Some(ModelAnimationPlayback::default());

        let mut history = UndoHistory::default();
        let mut render_state = test_render_state();
        let selection = Some(SceneSelection::Model(group));
        history.record(&world, selection, &render_state);
        world.model_group_mut(group).unwrap().animation = Some(ModelAnimationPlayback {
            clip_index: 1,
            speed: 1.5,
            looping: false,
            playing: true,
            crossfade_duration: 0.8,
            ..ModelAnimationPlayback::default()
        });
        let mut selection = selection;

        assert!(history.undo(&mut world, &mut selection, &mut render_state));
        assert_eq!(
            world.model_group(group).unwrap().animation,
            Some(ModelAnimationPlayback::default())
        );
        assert!(history.redo(&mut world, &mut selection, &mut render_state));
        let playback = world.model_group(group).unwrap().animation.unwrap();
        assert_eq!(playback.clip_index, 1);
        assert_eq!(playback.speed, 1.5);
        assert!(!playback.looping);
        assert!(playback.playing);
        assert_eq!(playback.crossfade_duration, 0.8);
    }

    #[test]
    fn profile_summary_uses_median_and_p95_and_ignores_non_finite_samples() {
        let summary = summarize_profile_metric([1.0, 2.0, 3.0, 100.0, f32::NAN].into_iter());

        assert_eq!(summary.sample_count, 4);
        assert_eq!(summary.median, Some(2.5));
        assert_eq!(summary.p95, Some(100.0));
    }

    #[test]
    fn screen_coverage_unions_overlapping_mesh_bounds() {
        let area = rectangle_union_area(&[(0.0, 0.0, 0.5, 1.0), (0.25, 0.0, 0.75, 1.0)]);

        assert!((area - 0.75).abs() < f32::EPSILON);
    }
}
