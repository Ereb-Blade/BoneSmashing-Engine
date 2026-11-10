use glam::{Quat, Vec3};
use std::path::Path;
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
use crate::renderer::debug_ui::DebugUi;
use crate::renderer::gltf::{GltfPrimitiveInstance, LoadedGltfModel};
use crate::renderer::state::{LoadedAsset, ParsedAsset, Renderer};
use crate::scene::{ObjectId, RenderObject, SceneFile, Transform, World, build_world};

pub struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    input: Input,
    camera: Camera,
    camera_controller: CameraController,
    last_time: Instant,
    world: World,
    animated_model: Option<LoadedGltfModel>,
    animated_object_ids: Vec<ObjectId>,
    animated_instances: Vec<GltfPrimitiveInstance>,
    animation_time: f32,
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
            animated_model: None,
            animated_object_ids: Vec::new(),
            animated_instances: Vec::new(),
            animation_time: 0.0,
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
                for (instance_index, instance) in model.instances.iter().enumerate() {
                    world
                        .add_group_object(
                            group,
                            RenderObject::new(
                                Transform::default(),
                                instance.mesh_id,
                                instance.material_id,
                            )
                            .with_name(instance.name.clone())
                            .with_local_matrix(instance.node_transform)
                            .with_skin_matrices(instance.skin_matrices.clone())
                            .with_asset_source(path.to_path_buf(), instance_index),
                        )
                        .ok_or_else(|| {
                            format!("Could not create model group for {}", path.display())
                        })?;
                }
                Ok(format!("Imported {count} model parts"))
            }
            LoadedAsset::Obj(mesh_id) => {
                let group = world.add_model_group(
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Model"),
                    path.to_path_buf(),
                    Transform::from_translation(position),
                );
                world
                    .add_group_object(
                        group,
                        RenderObject::new(Transform::default(), mesh_id, MATERIAL_DEFAULT)
                            .with_name(
                                path.file_stem()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("Model"),
                            )
                            .with_asset_source(path.to_path_buf(), 0),
                    )
                    .ok_or_else(|| {
                        format!("Could not create model group for {}", path.display())
                    })?;
                Ok("Imported OBJ model".to_owned())
            }
        }
    }

    fn restore_scene(
        renderer: &mut Renderer,
        loaded: crate::scene::LoadedScene,
    ) -> Result<World, String> {
        let original_mesh_count = renderer.meshes.len();
        let original_material_count = renderer.materials.len();
        let result = (|| {
            let mut asset_instances = std::collections::HashMap::with_capacity(loaded.assets.len());
            for (path, parsed) in loaded.assets {
                let instances = match renderer.load_parsed_asset(parsed, &path)? {
                    LoadedAsset::Gltf(model) => model
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
                            .with_local_matrix(instance.node_transform)
                            .with_skin_matrices(instance.skin_matrices)
                            .with_asset_source(path.clone(), index)
                        })
                        .collect(),
                    LoadedAsset::Obj(mesh_id) => vec![
                        RenderObject::new(
                            Transform::default(),
                            mesh_id,
                            crate::core::MATERIAL_DEFAULT,
                        )
                        .with_name(
                            path.file_stem()
                                .and_then(|name| name.to_str())
                                .unwrap_or("Model"),
                        )
                        .with_asset_source(path.clone(), 0),
                    ],
                };
                asset_instances.insert(path, instances);
            }
            let mut world = build_world(
                &loaded.file,
                crate::core::MESH_SPHERE.0 + 1,
                crate::core::MATERIAL_GLASS.0 + 1,
                asset_instances,
            )?;
            renderer.retain_scene_assets(
                world.objects_mut(),
                crate::core::MESH_SPHERE.0 + 1,
                crate::core::MATERIAL_GLASS.0 + 1,
            )?;
            Ok(world)
        })();
        if result.is_err() {
            renderer.discard_assets_after(original_mesh_count, original_material_count);
        }
        result
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let window = Arc::new(
                event_loop
                    .create_window(Window::default_attributes().with_title("BoneSmashing Engine"))
                    .unwrap(),
            );
            let size = window.inner_size();
            if size.width > 0 && size.height > 0 {
                self.camera.aspect = size.width as f32 / size.height as f32;
            }
            self.last_time = Instant::now();
            self.window = Some(window.clone());
            self.renderer = Some(pollster::block_on(Renderer::new(window.clone())));
            if let Some(renderer) = self.renderer.as_ref() {
                self.debug_ui = Some(DebugUi::new(
                    &window,
                    &renderer.device,
                    renderer.surface_format(),
                ));
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
                            for (instance_index, instance) in model.instances.iter().enumerate() {
                                let id = self.world.add_group_object(
                                    group,
                                    RenderObject::new(
                                        Transform::default(),
                                        instance.mesh_id,
                                        instance.material_id,
                                    )
                                    .with_name(instance.name.clone())
                                    .with_local_matrix(instance.node_transform)
                                    .with_skin_matrices(instance.skin_matrices.clone())
                                    .with_asset_source(sample_model.clone(), instance_index),
                                );
                                if let Some(id) = id {
                                    self.animated_object_ids.push(id);
                                }
                            }
                            if let Some(animation) = model.animations.first() {
                                eprintln!(
                                    "Загружена skeletal-анимация '{}', {:.2} с",
                                    animation.name, animation.duration
                                );
                                self.animated_model = Some(model);
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
        if self.input.cursor_locked {
            if let DeviceEvent::MouseMotion { delta } = event {
                self.input.process_mouse_motion(delta);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = &self.window else { return };
        let ui_consumed = self
            .debug_ui
            .as_mut()
            .is_some_and(|debug_ui| debug_ui.on_window_event(window, &event));
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    renderer.resize(size.width, size.height);
                    self.camera.aspect = size.width as f32 / size.height as f32;
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let is_pressed = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(keycode) = event.physical_key {
                    if keycode == KeyCode::F1 && is_pressed {
                        if let Some(debug_ui) = self.debug_ui.as_mut() {
                            debug_ui.toggle();
                        }
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
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => {
                if !ui_consumed {
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

                self.camera_controller
                    .update_camera(&mut self.camera, &self.input, dt);
                self.input.reset_mouse_delta();

                self.world.update(dt);
                self.animation_time += dt;
                if let Some(model) = self.animated_model.as_mut() {
                    if let Some(animation) = model.animations.first() {
                        let time = if animation.duration > 0.0 {
                            self.animation_time % animation.duration
                        } else {
                            0.0
                        };
                        match model.sample_animation_into(0, time, &mut self.animated_instances) {
                            Ok(()) => {
                                for (id, instance) in self
                                    .animated_object_ids
                                    .iter()
                                    .zip(&self.animated_instances)
                                {
                                    if let Some(object) = self.world.get_object_mut(*id) {
                                        object.set_local_matrix(instance.node_transform);
                                        object.set_skin_matrices(&instance.skin_matrices);
                                    }
                                }
                            }
                            Err(error) => {
                                eprintln!("Не удалось обновить skeletal-анимацию: {error}");
                                self.animated_model = None;
                            }
                        }
                    }
                }

                if let Some(debug_ui) = self.debug_ui.as_mut() {
                    if let Some(path) = debug_ui.take_scene_save_request() {
                        let result = SceneFile::from_world(&self.world, &path)
                            .and_then(|scene| scene.write(&path))
                            .map(|()| format!("Saved scene to {}", path.display()));
                        debug_ui.set_scene_status(result);
                    }
                    if let Some(loaded) = debug_ui.take_scene_load_request() {
                        match loaded.and_then(|loaded| Self::restore_scene(renderer, loaded)) {
                            Ok(world) => {
                                self.world = world;
                                self.animated_model = None;
                                self.animated_object_ids.clear();
                                self.animated_instances.clear();
                                self.animation_time = 0.0;
                                debug_ui.scene_loaded();
                                debug_ui.set_scene_status(Ok("Scene loaded".to_owned()));
                            }
                            Err(error) => debug_ui.set_scene_status(Err(error)),
                        }
                    }
                    if let Some((path, parsed)) = debug_ui.take_asset_request() {
                        let result = parsed.and_then(|parsed| {
                            Self::import_asset(
                                renderer,
                                &mut self.world,
                                &self.camera,
                                &path,
                                parsed,
                            )
                        });
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
