use crate::renderer::state::{ParsedAsset, RenderTimings, Renderer, parse_asset};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use winit::window::Window;

struct PreparedFrame {
    paint_jobs: Vec<egui::ClippedPrimitive>,
    textures_delta: egui::TexturesDelta,
    screen: egui_wgpu::ScreenDescriptor,
}

pub struct DebugUi {
    context: egui::Context,
    platform: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    visible: bool,
    environment_path: String,
    status: Option<Result<String, String>>,
    environment_dialog: Option<mpsc::Receiver<Option<PathBuf>>>,
    selected_material: usize,
    asset_path: String,
    asset_status: Option<Result<String, String>>,
    asset_request: Option<(PathBuf, Result<ParsedAsset, String>)>,
    asset_dialog: Option<mpsc::Receiver<Option<PathBuf>>>,
    asset_import: Option<mpsc::Receiver<(PathBuf, Result<ParsedAsset, String>)>>,
    prepared: Option<PreparedFrame>,
}

impl DebugUi {
    pub fn new(window: &Arc<Window>, device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let context = egui::Context::default();
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
            environment_path: String::new(),
            status: None,
            environment_dialog: None,
            selected_material: 0,
            asset_path: String::new(),
            asset_status: None,
            asset_request: None,
            asset_dialog: None,
            asset_import: None,
            prepared: None,
        }
    }

    pub fn on_window_event(&mut self, window: &Window, event: &winit::event::WindowEvent) -> bool {
        self.platform.on_window_event(window, event).consumed
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub fn take_asset_request(&mut self) -> Option<(PathBuf, Result<ParsedAsset, String>)> {
        self.asset_request.take()
    }

    pub fn set_asset_status(&mut self, status: Result<String, String>) {
        self.asset_status = Some(status);
    }

    pub fn prepare_frame(
        &mut self,
        window: &Window,
        frame_time: f32,
        fps: f32,
        cpu_frame_time: f32,
        object_count: usize,
        timings: RenderTimings,
        renderer: &mut Renderer,
    ) {
        let mut settings = renderer.settings();
        let material_count = renderer.material_count();
        self.selected_material = self.selected_material.min(material_count.saturating_sub(1));
        let mut selected_params = renderer
            .material_params(self.selected_material)
            .unwrap_or_default();
        let mut load_environment = false;
        let mut pick_environment = false;
        let mut pick_asset = false;
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
        let input = self.platform.take_egui_input(window);
        let output = self.context.run(input, |context| {
            if !self.visible {
                return;
            }
            egui::Window::new("BoneSmashing Engine | Renderer")
                .default_width(340.0)
                .resizable(true)
                .show(context, |ui| {
                    ui.label(format!("{fps:.1} FPS  |  {:.2} ms", frame_time * 1000.0));
                    ui.label(format!("CPU + waits: {cpu_frame_time:.2} ms"));
                    ui.small(format!(
                        "UI {:.2} | scene {:.2} | acquire {:.2} | encode/submit {:.2} ms",
                        timings.ui_cpu_ms,
                        timings.scene_cpu_ms,
                        timings.acquire_cpu_ms,
                        timings.encode_submit_cpu_ms,
                    ));
                    ui.label(format!("Objects: {object_count}"));
                    ui.separator();

                    ui.collapsing("Scene", |ui| {
                        if ui
                            .add_enabled(
                                self.asset_dialog.is_none() && self.asset_import.is_none(),
                                egui::Button::new("Import model..."),
                            )
                            .clicked()
                        {
                            pick_asset = true;
                        }
                        if !self.asset_path.is_empty() {
                            ui.small(&self.asset_path);
                        }
                        if self.asset_dialog.is_some() {
                            ui.label("Waiting for file selection...");
                        } else if self.asset_import.is_some() {
                            ui.label("Reading model data...");
                        }
                        if let Some(status) = &self.asset_status {
                            match status {
                                Ok(message) => {
                                    ui.colored_label(egui::Color32::LIGHT_GREEN, message);
                                }
                                Err(message) => {
                                    ui.colored_label(egui::Color32::LIGHT_RED, message);
                                }
                            }
                        }
                        ui.small("Supported formats: .glb, .gltf, .obj");
                    });

                    ui.collapsing("Lighting & tone mapping", |ui| {
                        ui.add(
                            egui::Slider::new(&mut settings.key_light_yaw_degrees, -180.0..=180.0)
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
                            egui::Slider::new(&mut settings.key_light_intensity, 0.0..=8.0)
                                .text("Key light"),
                        );
                        ui.add(
                            egui::Slider::new(&mut settings.fill_light_intensity, 0.0..=4.0)
                                .text("Fill light"),
                        );
                        ui.add(
                            egui::Slider::new(&mut settings.environment_intensity, 0.0..=2.0)
                                .text("Environment"),
                        );
                        ui.add(
                            egui::Slider::new(&mut settings.exposure_ev, -10.0..=3.0)
                                .text("Exposure (EV)"),
                        );
                        ui.checkbox(&mut settings.shadows_enabled, "Directional shadows");
                        ui.checkbox(&mut settings.unlocked_fps, "Unlock FPS (disable VSync)");
                        ui.add(
                            egui::Slider::new(&mut settings.render_scale, 0.5..=1.0)
                                .step_by(0.05)
                                .text("Render resolution scale"),
                        );
                        if ui.button("Reset lighting").clicked() {
                            let defaults = crate::renderer::state::RendererSettings::default();
                            settings.key_light_yaw_degrees = defaults.key_light_yaw_degrees;
                            settings.key_light_elevation_degrees =
                                defaults.key_light_elevation_degrees;
                            settings.key_light_intensity = defaults.key_light_intensity;
                            settings.fill_light_intensity = defaults.fill_light_intensity;
                            settings.environment_intensity = defaults.environment_intensity;
                            settings.exposure_ev = defaults.exposure_ev;
                            settings.shadows_enabled = defaults.shadows_enabled;
                            settings.debug_view = defaults.debug_view;
                            settings.render_scale = defaults.render_scale;
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

                    ui.collapsing("HDR environment", |ui| {
                        ui.label(format!(
                            "Active: {} ({} mips)",
                            renderer.environment_name(),
                            renderer.environment_mip_count()
                        ));
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    self.environment_dialog.is_none(),
                                    egui::Button::new("Browse..."),
                                )
                                .clicked()
                            {
                                pick_environment = true;
                            }
                            ui.text_edit_singleline(&mut self.environment_path);
                            if ui.button("Load .hdr").clicked() {
                                load_environment = true;
                            }
                        });
                        if self.environment_dialog.is_some() {
                            ui.label("Waiting for file selection...");
                        }
                        ui.label("Equirectangular .hdr / .exr environment (2:1)");
                        if let Some(status) = &self.status {
                            match status {
                                Ok(message) => {
                                    ui.colored_label(egui::Color32::LIGHT_GREEN, message);
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
                                        let label = renderer.material_label(index).unwrap_or("");
                                        ui.selectable_value(
                                            &mut self.selected_material,
                                            index,
                                            format!("{index}: {label}"),
                                        );
                                    }
                                });
                            ui.add(
                                egui::Slider::new(&mut selected_params.roughness, 0.045..=1.0)
                                    .text("Roughness"),
                            );
                            ui.add(
                                egui::Slider::new(&mut selected_params.metallic, 0.0..=1.0)
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
                    ui.separator();
                    ui.small("F1: show/hide panel  |  Esc: release camera");
                });
        });
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
        renderer.set_settings(settings);
        if material_count > 0 {
            renderer.set_material_params(self.selected_material, selected_params);
        }
        if load_environment && !self.environment_path.trim().is_empty() {
            match renderer.load_environment(Path::new(self.environment_path.trim())) {
                Ok(()) => self.status = Some(Ok("HDR environment loaded".to_owned())),
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
