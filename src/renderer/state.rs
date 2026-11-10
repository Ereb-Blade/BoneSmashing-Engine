use glam::{Mat4, Vec3};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::camera::{Camera, CameraUniform};
use crate::config::{DEPTH_FORMAT, MSAA_SAMPLES};
use crate::core::{MaterialId, MeshId};
use crate::renderer::gltf::{GltfModelData, LoadedGltfModel};
use crate::renderer::material::{Material, MaterialParams};
use crate::renderer::mesh::{Mesh, MeshData};
use crate::renderer::pipeline::{HDR_FORMAT, RenderPipelineBundle};
use crate::renderer::{debug_ui::DebugUi, environment::Environment};
use crate::scene::{RenderObject, World};

#[derive(Debug)]
pub enum ParsedAsset {
    Gltf(GltfModelData),
    Obj(MeshData),
}

pub enum LoadedAsset {
    Gltf(LoadedGltfModel),
    Obj(MeshId),
}

pub fn parse_asset(path: &Path) -> Result<ParsedAsset, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("Unsupported model file: {}", path.display()))?;
    match extension.as_str() {
        "glb" | "gltf" => GltfModelData::load(path).map(ParsedAsset::Gltf),
        "obj" => MeshData::load_obj(path)
            .map(ParsedAsset::Obj)
            .map_err(|error| format!("{}: {error}", path.display())),
        _ => Err(format!(
            "Unsupported model format: .{extension}. Choose .glb, .gltf, or .obj"
        )),
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightUniform {
    pub direction_intensity: [f32; 4],
    pub color: [f32; 4],
    pub fill_direction_intensity: [f32; 4],
    pub fill_color: [f32; 4],
    pub ambient_top: [f32; 4],
    pub ambient_bottom: [f32; 4],
}

const SHADOW_MAP_SIZE: u32 = 2048;

fn scaled_render_dimension(dimension: u32, scale: f32) -> u32 {
    ((dimension as f32 * scale).round() as u32).max(1)
}

fn persisted_environment_path() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .map(|path| path.join("MyEngine").join("environment.txt"))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RendererSettings {
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
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RenderTimings {
    pub ui_cpu_ms: f32,
    pub scene_cpu_ms: f32,
    pub acquire_cpu_ms: f32,
    pub encode_submit_cpu_ms: f32,
}

impl Default for RendererSettings {
    fn default() -> Self {
        Self {
            key_light_yaw_degrees: -142.0,
            key_light_elevation_degrees: -53.0,
            key_light_intensity: 2.0,
            fill_light_intensity: 0.25,
            environment_intensity: 0.85,
            exposure_ev: -0.5,
            shadows_enabled: true,
            debug_view: 0,
            unlocked_fps: false,
            render_scale: 1.0,
        }
    }
}

impl RendererSettings {
    fn key_light_direction(self) -> Vec3 {
        let yaw = self.key_light_yaw_degrees.to_radians();
        let elevation = self.key_light_elevation_degrees.to_radians();
        Vec3::new(
            yaw.cos() * elevation.cos(),
            elevation.sin(),
            yaw.sin() * elevation.cos(),
        )
    }
}

#[repr(C)]
#[derive(Copy, Clone, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct InstanceRaw {
    model: [[f32; 4]; 4],
    normal: [[f32; 4]; 3],
    skin_info: [u32; 2],
}

impl InstanceRaw {
    const ATTRIBS: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![
        6 => Float32x4,
        7 => Float32x4,
        8 => Float32x4,
        9 => Float32x4,
        10 => Float32x4,
        11 => Float32x4,
        12 => Float32x4,
        13 => Uint32x2,
    ];

    fn from_model(model: Mat4, skin_offset: u32, skin_count: u32) -> Self {
        let normal = crate::math::compute_normal_matrix(model).to_cols_array_2d();

        Self {
            model: model.to_cols_array_2d(),
            normal: [
                [normal[0][0], normal[0][1], normal[0][2], 0.0],
                [normal[1][0], normal[1][1], normal[1][2], 0.0],
                [normal[2][0], normal[2][1], normal[2][2], 0.0],
            ],
            skin_info: [skin_offset, skin_count],
        }
    }

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}

fn create_depth_view(device: &wgpu::Device, w: u32, h: u32) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: MSAA_SAMPLES,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&Default::default())
}

fn create_scene_color_texture(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("opaque scene color"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn create_scene_color_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
) -> (wgpu::Sampler, wgpu::BindGroup) {
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("scene color sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("scene color bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    (sampler, bind_group)
}

fn create_msaa_color_view(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("msaa_color"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: MSAA_SAMPLES,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&Default::default())
}

fn create_shared_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    camera_buffer: &wgpu::Buffer,
    light_buffer: &wgpu::Buffer,
    skin_buffer: &wgpu::Buffer,
    shadow_matrix_buffer: &wgpu::Buffer,
    shadow_view: &wgpu::TextureView,
    shadow_sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("shared bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: light_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: skin_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: shadow_matrix_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(shadow_view),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::Sampler(shadow_sampler),
            },
        ],
    })
}

fn create_shadow_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    skin_buffer: &wgpu::Buffer,
    shadow_matrix_buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("shadow shared bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 2,
                resource: skin_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: shadow_matrix_buffer.as_entire_binding(),
            },
        ],
    })
}

fn create_shadow_view(device: &wgpu::Device) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("directional shadow map"),
        size: wgpu::Extent3d {
            width: SHADOW_MAP_SIZE,
            height: SHADOW_MAP_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn directional_shadow_matrix(ray_direction: Vec3) -> Mat4 {
    let center = Vec3::new(0.0, 1.5, 0.0);
    let ray_direction = ray_direction.normalize();
    let eye = center - ray_direction * 25.0;
    let up = if ray_direction.dot(Vec3::Y).abs() > 0.98 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let view = crate::math::look_at_rh(eye, center, up);
    crate::math::orthographic_rh(-18.0, 18.0, -18.0, 18.0, 0.1, 60.0) * view
}

const INITIAL_INSTANCE_CAPACITY: usize = 256;
const INITIAL_SKIN_MATRIX_CAPACITY: usize = 256;
type DrawBatch = (MeshId, MaterialId, std::ops::Range<u32>, bool);
type TransparentObject = (f32, usize, MeshId, MaterialId, InstanceRaw);
type ShadowBatch = (MeshId, MaterialId, std::ops::Range<u32>);

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    glass_pipeline: wgpu::RenderPipeline,
    skybox_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    postprocess_pipeline: wgpu::RenderPipeline,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    shared_bind_group_layout: wgpu::BindGroupLayout,
    shadow_bind_group_layout: wgpu::BindGroupLayout,
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    camera_buffer: wgpu::Buffer,
    _light_buffer: wgpu::Buffer,
    shared_bind_group: wgpu::BindGroup,
    shadow_shared_bind_group: wgpu::BindGroup,
    skin_buffer: wgpu::Buffer,
    skin_matrix_capacity: usize,
    shadow_matrix_buffer: wgpu::Buffer,
    shadow_view: wgpu::TextureView,
    shadow_sampler: wgpu::Sampler,
    environment: Environment,
    environment_bind_group: wgpu::BindGroup,
    _scene_color_texture: wgpu::Texture,
    scene_color_view: wgpu::TextureView,
    _scene_color_sampler: wgpu::Sampler,
    scene_color_bind_group: wgpu::BindGroup,
    _hdr_output_texture: wgpu::Texture,
    hdr_output_view: wgpu::TextureView,
    _postprocess_sampler: wgpu::Sampler,
    postprocess_bind_group: wgpu::BindGroup,
    environment_bind_group_layout: wgpu::BindGroupLayout,
    scene_color_bind_group_layout: wgpu::BindGroupLayout,
    settings: RendererSettings,
    depth_view: wgpu::TextureView,
    msaa_view: wgpu::TextureView,
    missing_mesh_warnings: HashSet<MeshId>,
    missing_material_warnings: HashSet<MaterialId>,
    batches: HashMap<(MeshId, MaterialId), Vec<InstanceRaw>>,
    transparent_objects: Vec<TransparentObject>,
    skin_matrices: Vec<Mat4>,
    all_instances: Vec<InstanceRaw>,
    draw_batches: Vec<DrawBatch>,
    cached_shadow_instances: Vec<InstanceRaw>,
    cached_shadow_skin_matrices: Vec<Mat4>,
    cached_shadow_batches: Vec<ShadowBatch>,
    cached_shadow_matrix: Mat4,
    shadow_cache_valid: bool,
    timings: RenderTimings,
}

fn create_environment_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    environment: &Environment,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("environment bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&environment.view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&environment.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&environment.diffuse_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&environment.prefiltered_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&environment.brdf_lut_view),
            },
        ],
    })
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone()).unwrap();

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("нет подходящего GPU-адаптера");

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .expect("не удалось создать device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let depth_view = create_depth_view(&device, config.width, config.height);
        let msaa_view = create_msaa_color_view(&device, config.width, config.height, HDR_FORMAT);
        let (scene_color_texture, scene_color_view) =
            create_scene_color_texture(&device, config.width, config.height, HDR_FORMAT);
        let (hdr_output_texture, hdr_output_view) =
            create_scene_color_texture(&device, config.width, config.height, HDR_FORMAT);

        let camera_uniform = CameraUniform::new();
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera buffer"),
            contents: bytemuck::bytes_of(&camera_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let settings = RendererSettings::default();
        let key_direction = settings.key_light_direction();
        let light_uniform = LightUniform {
            direction_intensity: [
                key_direction.x,
                key_direction.y,
                key_direction.z,
                settings.key_light_intensity,
            ],
            color: [1.0, 0.92, 0.78, 1.0],
            fill_direction_intensity: [0.55, -0.3, 0.65, settings.fill_light_intensity],
            fill_color: [0.62, 0.76, 1.0, settings.debug_view as f32],
            ambient_top: [0.0, 0.0, 0.0, settings.environment_intensity],
            ambient_bottom: [0.0, 0.0, 0.0, settings.exposure_ev],
        };
        let light_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("light buffer"),
            contents: bytemuck::bytes_of(&light_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // Создаем пайплайн и бинд-групп лейауты через модуль pipeline
        let pipeline_bundle = RenderPipelineBundle::new(&device, format);
        let mut environment = Environment::studio(&device, &queue);
        if let Some(path) = persisted_environment_path() {
            if let Ok(saved_path) = std::fs::read_to_string(&path) {
                let saved_path = Path::new(saved_path.trim());
                if saved_path.is_file() {
                    match Environment::load(
                        &device,
                        &queue,
                        saved_path,
                        device.limits().max_texture_dimension_2d,
                    ) {
                        Ok(saved_environment) => environment = saved_environment,
                        Err(error) => {
                            eprintln!("Could not restore saved HDR environment: {error}")
                        }
                    }
                }
            }
        }
        let environment_bind_group = create_environment_bind_group(
            &device,
            &pipeline_bundle.environment_bind_group_layout,
            &environment,
        );
        let (scene_color_sampler, scene_color_bind_group) = create_scene_color_bind_group(
            &device,
            &pipeline_bundle.scene_color_bind_group_layout,
            &scene_color_view,
        );
        let (postprocess_sampler, postprocess_bind_group) = create_scene_color_bind_group(
            &device,
            &pipeline_bundle.scene_color_bind_group_layout,
            &hdr_output_view,
        );

        let skin_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("skin matrix buffer"),
            size: (INITIAL_SKIN_MATRIX_CAPACITY * std::mem::size_of::<Mat4>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shadow_matrix = directional_shadow_matrix(key_direction);
        let shadow_matrix_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shadow matrix buffer"),
            contents: bytemuck::bytes_of(&shadow_matrix.to_cols_array_2d()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let shadow_view = create_shadow_view(&device);
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow comparison sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let shared_bind_group = create_shared_bind_group(
            &device,
            &pipeline_bundle.shared_bind_group_layout,
            &camera_buffer,
            &light_buffer,
            &skin_buffer,
            &shadow_matrix_buffer,
            &shadow_view,
            &shadow_sampler,
        );
        let shadow_shared_bind_group = create_shadow_bind_group(
            &device,
            &pipeline_bundle.shadow_bind_group_layout,
            &skin_buffer,
            &shadow_matrix_buffer,
        );

        // Создание материалов через удобные методы Material
        let default_material = Material::from_image_bytes_with_linear_gain(
            &device,
            &queue,
            &pipeline_bundle.texture_bind_group_layout,
            include_bytes!("../../Dark grey.jpg"),
            4.0,
            MaterialParams {
                base_color: [1.0, 1.0, 1.0, 1.0],
                roughness: 0.92,
                metallic: 0.0,
                ..Default::default()
            },
            Some("dark grey floor"),
        )
        .expect("не удалось загрузить текстуру");

        let red_material = Material::solid_color(
            &device,
            &queue,
            &pipeline_bundle.texture_bind_group_layout,
            [255, 255, 255, 255],
            MaterialParams {
                base_color: [1.0, 0.15, 0.15, 1.0],
                roughness: 0.15,
                metallic: 0.8,
                ..Default::default()
            },
            Some("shiny_red_texture"),
        );

        let chrome_material = Material::solid_color(
            &device,
            &queue,
            &pipeline_bundle.texture_bind_group_layout,
            [255, 255, 255, 255],
            MaterialParams {
                base_color: [0.98, 0.99, 1.0, 1.0],
                roughness: 0.12,
                metallic: 1.0,
                ..Default::default()
            },
            Some("polished chrome"),
        );

        let silver_material = Material::solid_color(
            &device,
            &queue,
            &pipeline_bundle.texture_bind_group_layout,
            [255, 255, 255, 255],
            MaterialParams {
                base_color: [0.92, 0.95, 1.0, 1.0],
                roughness: 0.045,
                metallic: 1.0,
                ..Default::default()
            },
            Some("polished silver"),
        );

        let gold_material = Material::solid_color(
            &device,
            &queue,
            &pipeline_bundle.texture_bind_group_layout,
            [255, 255, 255, 255],
            MaterialParams {
                base_color: [1.0, 0.71, 0.24, 1.0],
                roughness: 0.055,
                metallic: 1.0,
                ..Default::default()
            },
            Some("polished gold"),
        );

        let glass_material = Material::solid_color(
            &device,
            &queue,
            &pipeline_bundle.texture_bind_group_layout,
            [255, 255, 255, 255],
            MaterialParams {
                base_color: [0.98, 0.995, 1.0, 1.0],
                roughness: 0.035,
                metallic: 0.0,
                transmission_factor: 1.0,
                alpha_mode: 2,
                ..Default::default()
            },
            Some("clear glass"),
        );

        let materials = vec![
            default_material,
            red_material,
            chrome_material,
            silver_material,
            gold_material,
            glass_material,
        ];
        let meshes = vec![
            Mesh::cube(&device),
            Mesh::plane(&device, 20.0),
            Mesh::sphere(&device, 96, 48),
        ];

        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instance buffer"),
            size: (INITIAL_INSTANCE_CAPACITY * std::mem::size_of::<InstanceRaw>())
                as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            surface,
            device,
            queue,
            config,
            pipeline: pipeline_bundle.pipeline,
            transparent_pipeline: pipeline_bundle.transparent_pipeline,
            glass_pipeline: pipeline_bundle.glass_pipeline,
            skybox_pipeline: pipeline_bundle.skybox_pipeline,
            shadow_pipeline: pipeline_bundle.shadow_pipeline,
            postprocess_pipeline: pipeline_bundle.postprocess_pipeline,
            texture_bind_group_layout: pipeline_bundle.texture_bind_group_layout,
            shared_bind_group_layout: pipeline_bundle.shared_bind_group_layout,
            shadow_bind_group_layout: pipeline_bundle.shadow_bind_group_layout,
            meshes,
            materials,
            instance_buffer,
            instance_capacity: INITIAL_INSTANCE_CAPACITY,
            camera_buffer,
            _light_buffer: light_buffer,
            shared_bind_group,
            shadow_shared_bind_group,
            skin_buffer,
            skin_matrix_capacity: INITIAL_SKIN_MATRIX_CAPACITY,
            shadow_matrix_buffer,
            shadow_view,
            shadow_sampler,
            environment,
            environment_bind_group,
            environment_bind_group_layout: pipeline_bundle.environment_bind_group_layout,
            scene_color_bind_group_layout: pipeline_bundle.scene_color_bind_group_layout,
            settings,
            depth_view,
            msaa_view,
            _scene_color_texture: scene_color_texture,
            scene_color_view,
            _scene_color_sampler: scene_color_sampler,
            scene_color_bind_group,
            _hdr_output_texture: hdr_output_texture,
            hdr_output_view,
            _postprocess_sampler: postprocess_sampler,
            postprocess_bind_group,
            missing_mesh_warnings: HashSet::new(),
            missing_material_warnings: HashSet::new(),
            batches: HashMap::new(),
            transparent_objects: Vec::new(),
            skin_matrices: Vec::with_capacity(INITIAL_SKIN_MATRIX_CAPACITY),
            all_instances: Vec::new(),
            draw_batches: Vec::new(),
            cached_shadow_instances: Vec::new(),
            cached_shadow_skin_matrices: Vec::new(),
            cached_shadow_batches: Vec::new(),
            cached_shadow_matrix: Mat4::IDENTITY,
            shadow_cache_valid: false,
            timings: RenderTimings::default(),
        }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
        self.resize_render_targets();
    }

    fn resize_render_targets(&mut self) {
        let w = scaled_render_dimension(self.config.width, self.settings.render_scale);
        let h = scaled_render_dimension(self.config.height, self.settings.render_scale);
        self.depth_view = create_depth_view(&self.device, w, h);
        self.msaa_view = create_msaa_color_view(&self.device, w, h, HDR_FORMAT);
        let (scene_color_texture, scene_color_view) =
            create_scene_color_texture(&self.device, w, h, HDR_FORMAT);
        let (scene_color_sampler, scene_color_bind_group) = create_scene_color_bind_group(
            &self.device,
            &self.scene_color_bind_group_layout,
            &scene_color_view,
        );
        let (hdr_output_texture, hdr_output_view) =
            create_scene_color_texture(&self.device, w, h, HDR_FORMAT);
        let (postprocess_sampler, postprocess_bind_group) = create_scene_color_bind_group(
            &self.device,
            &self.scene_color_bind_group_layout,
            &hdr_output_view,
        );
        self._scene_color_texture = scene_color_texture;
        self.scene_color_view = scene_color_view;
        self._scene_color_sampler = scene_color_sampler;
        self.scene_color_bind_group = scene_color_bind_group;
        self._hdr_output_texture = hdr_output_texture;
        self.hdr_output_view = hdr_output_view;
        self._postprocess_sampler = postprocess_sampler;
        self.postprocess_bind_group = postprocess_bind_group;
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn settings(&self) -> RendererSettings {
        self.settings
    }

    pub fn set_settings(&mut self, settings: RendererSettings) {
        let mut settings = settings;
        settings.render_scale = if settings.render_scale.is_finite() {
            settings.render_scale.clamp(0.5, 1.0)
        } else {
            self.settings.render_scale
        };
        let render_scale_changed = self.settings.render_scale != settings.render_scale;
        if self.settings.unlocked_fps != settings.unlocked_fps {
            self.config.present_mode = if settings.unlocked_fps {
                wgpu::PresentMode::AutoNoVsync
            } else {
                wgpu::PresentMode::Fifo
            };
            self.surface.configure(&self.device, &self.config);
        }
        self.settings = settings;
        if render_scale_changed {
            self.resize_render_targets();
        }
    }

    pub fn material_count(&self) -> usize {
        self.materials.len()
    }

    pub fn discard_assets_after(&mut self, mesh_count: usize, material_count: usize) {
        self.meshes.truncate(mesh_count);
        self.materials.truncate(material_count);
        self.missing_mesh_warnings
            .retain(|mesh_id| mesh_id.0 < mesh_count);
        self.missing_material_warnings
            .retain(|material_id| material_id.0 < material_count);
        self.batches.clear();
        self.draw_batches.clear();
        self.transparent_objects.clear();
        self.shadow_cache_valid = false;
    }

    pub fn retain_scene_assets(
        &mut self,
        objects: &mut [RenderObject],
        built_in_mesh_count: usize,
        built_in_material_count: usize,
    ) -> Result<(), String> {
        if built_in_mesh_count > self.meshes.len()
            || built_in_material_count > self.materials.len()
            || objects.iter().any(|object| {
                object.mesh_id.0 >= self.meshes.len()
                    || object.material_id.0 >= self.materials.len()
            })
        {
            return Err("Scene references a mesh or material that is not loaded".to_owned());
        }

        let used_meshes = objects
            .iter()
            .map(|object| object.mesh_id.0)
            .collect::<HashSet<_>>();
        let used_materials = objects
            .iter()
            .map(|object| object.material_id.0)
            .collect::<HashSet<_>>();
        let mut mesh_remap = vec![0; self.meshes.len()];
        let mut meshes = Vec::with_capacity(built_in_mesh_count + used_meshes.len());
        for (old_index, mesh) in std::mem::take(&mut self.meshes).into_iter().enumerate() {
            if old_index < built_in_mesh_count || used_meshes.contains(&old_index) {
                mesh_remap[old_index] = meshes.len();
                meshes.push(mesh);
            }
        }
        let mut material_remap = vec![0; self.materials.len()];
        let mut materials = Vec::with_capacity(built_in_material_count + used_materials.len());
        for (old_index, material) in std::mem::take(&mut self.materials).into_iter().enumerate() {
            if old_index < built_in_material_count || used_materials.contains(&old_index) {
                material_remap[old_index] = materials.len();
                materials.push(material);
            }
        }
        for object in objects {
            object.mesh_id = MeshId(mesh_remap[object.mesh_id.0]);
            object.material_id = MaterialId(material_remap[object.material_id.0]);
        }
        self.meshes = meshes;
        self.materials = materials;
        self.missing_mesh_warnings.clear();
        self.missing_material_warnings.clear();
        self.batches.clear();
        self.draw_batches.clear();
        self.transparent_objects.clear();
        self.shadow_cache_valid = false;
        Ok(())
    }

    pub fn material_label(&self, index: usize) -> Option<&str> {
        self.materials
            .get(index)
            .map(|material| material.label.as_str())
    }

    pub fn material_params(&self, index: usize) -> Option<MaterialParams> {
        self.materials.get(index).map(|material| material.params)
    }

    pub fn set_material_params(&mut self, index: usize, params: MaterialParams) {
        if let Some(material) = self.materials.get_mut(index) {
            if material.params != params {
                self.queue
                    .write_buffer(&material._params_buffer, 0, bytemuck::bytes_of(&params));
                material.alpha_mode = params.alpha_mode;
                material.params = params;
                self.shadow_cache_valid = false;
            }
        }
    }

    pub fn environment_name(&self) -> &str {
        &self.environment.label
    }

    pub fn environment_mip_count(&self) -> u32 {
        self.environment.mip_count
    }

    pub fn use_studio_environment(&mut self) -> Result<(), String> {
        if let Some(settings_path) = persisted_environment_path() {
            match std::fs::remove_file(&settings_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "Could not clear saved HDR environment path {}: {error}",
                        settings_path.display()
                    ));
                }
            }
        }
        let environment = Environment::studio(&self.device, &self.queue);
        let bind_group = create_environment_bind_group(
            &self.device,
            &self.environment_bind_group_layout,
            &environment,
        );
        self.environment = environment;
        self.environment_bind_group = bind_group;
        Ok(())
    }

    pub fn load_environment(&mut self, path: &Path) -> Result<(), String> {
        let environment = Environment::load(
            &self.device,
            &self.queue,
            path,
            self.device.limits().max_texture_dimension_2d,
        )?;
        let bind_group = create_environment_bind_group(
            &self.device,
            &self.environment_bind_group_layout,
            &environment,
        );
        self.environment = environment;
        self.environment_bind_group = bind_group;
        if let Some(settings_path) = persisted_environment_path() {
            let result = settings_path
                .parent()
                .map(std::fs::create_dir_all)
                .transpose()
                .and_then(|_| std::fs::write(&settings_path, path.to_string_lossy().as_bytes()));
            if let Err(error) = result {
                eprintln!(
                    "HDR environment loaded but its path could not be saved to {}: {error}",
                    settings_path.display()
                );
            }
        }
        Ok(())
    }

    pub fn load_obj(&mut self, path: impl AsRef<Path>) -> Result<MeshId, String> {
        let path = path.as_ref();
        let mesh_data =
            MeshData::load_obj(path).map_err(|error| format!("{}: {error}", path.display()))?;
        self.upload_obj(mesh_data, path)
    }

    fn upload_obj(&mut self, mesh_data: MeshData, path: &Path) -> Result<MeshId, String> {
        let max_buffer_size = self.device.limits().max_buffer_size;
        let vertex_bytes = std::mem::size_of_val(mesh_data.vertices.as_slice()) as u64;
        let index_bytes = std::mem::size_of_val(mesh_data.indices.as_slice()) as u64;
        if vertex_bytes > max_buffer_size || index_bytes > max_buffer_size {
            return Err(format!(
                "{}: mesh buffers exceed the device limit of {max_buffer_size} bytes",
                path.display()
            ));
        }

        let id = MeshId(self.meshes.len());
        let label = path.file_stem().and_then(|name| name.to_str());
        self.meshes.push(Mesh::from_slices(
            &self.device,
            &mesh_data.vertices,
            &mesh_data.indices,
            label,
        ));
        Ok(id)
    }

    pub fn load_gltf(&mut self, path: impl AsRef<Path>) -> Result<LoadedGltfModel, String> {
        let path = path.as_ref();
        let model = GltfModelData::load(path)?;
        self.upload_gltf(model, path)
    }

    pub fn load_parsed_asset(
        &mut self,
        parsed: ParsedAsset,
        path: &Path,
    ) -> Result<LoadedAsset, String> {
        match parsed {
            ParsedAsset::Gltf(model) => self.upload_gltf(model, path).map(LoadedAsset::Gltf),
            ParsedAsset::Obj(mesh_data) => self.upload_obj(mesh_data, path).map(LoadedAsset::Obj),
        }
    }

    fn upload_gltf(
        &mut self,
        model: GltfModelData,
        path: &Path,
    ) -> Result<LoadedGltfModel, String> {
        let limits = self.device.limits();
        let max_buffer_size = limits.max_buffer_size;

        for (index, image) in model.images.iter().enumerate() {
            if image.width > limits.max_texture_dimension_2d
                || image.height > limits.max_texture_dimension_2d
            {
                return Err(format!(
                    "{}: image {index} exceeds the device's maximum 2D texture dimension",
                    path.display()
                ));
            }
        }
        for (index, primitive) in model.primitives.iter().enumerate() {
            let vertex_bytes = std::mem::size_of_val(primitive.mesh.vertices.as_slice()) as u64;
            let skin_bytes = std::mem::size_of_val(primitive.mesh.skin_vertices.as_slice()) as u64;
            let index_bytes = std::mem::size_of_val(primitive.mesh.indices.as_slice()) as u64;
            if primitive.mesh.skin_vertices.len() != primitive.mesh.vertices.len()
                || vertex_bytes > max_buffer_size
                || skin_bytes > max_buffer_size
                || index_bytes > max_buffer_size
            {
                return Err(format!(
                    "{}: primitive {index} exceeds the device's maximum buffer size",
                    path.display()
                ));
            }
        }
        for material in &model.materials {
            if material
                .base_color_image
                .is_some_and(|image| image >= model.images.len())
                || material
                    .metallic_roughness_image
                    .is_some_and(|image| image >= model.images.len())
                || material
                    .normal_image
                    .is_some_and(|image| image >= model.images.len())
                || material
                    .occlusion_image
                    .is_some_and(|image| image >= model.images.len())
                || material
                    .emissive_image
                    .is_some_and(|image| image >= model.images.len())
                || material
                    .specular_image
                    .is_some_and(|image| image >= model.images.len())
                || material
                    .specular_color_image
                    .is_some_and(|image| image >= model.images.len())
                || material
                    .transmission_image
                    .is_some_and(|image| image >= model.images.len())
                || material
                    .thickness_image
                    .is_some_and(|image| image >= model.images.len())
            {
                return Err(format!(
                    "{}: material references a missing image",
                    path.display()
                ));
            }
        }

        let default_texture_mip_levels = [vec![255; 4]];
        let flat_normal_mip_levels = [vec![128, 128, 255, 255]];
        let first_material_id = self.materials.len();
        for (index, material_data) in model.materials.iter().enumerate() {
            let base_color = material_data
                .base_color_image
                .map(|image| &model.images[image]);
            let metallic_roughness = material_data
                .metallic_roughness_image
                .map(|image| &model.images[image]);
            let normal = material_data.normal_image.map(|image| &model.images[image]);
            let base_color_mips = base_color
                .and_then(|image| image.base_color_mip_levels.as_deref())
                .unwrap_or(&default_texture_mip_levels);
            let (base_color_width, base_color_height) = base_color
                .map(|image| (image.width, image.height))
                .unwrap_or((1, 1));
            let metallic_roughness_mips = metallic_roughness
                .and_then(|image| image.metallic_roughness_mip_levels.as_deref())
                .unwrap_or(&default_texture_mip_levels);
            let (mr_width, mr_height) = metallic_roughness
                .map(|image| (image.width, image.height))
                .unwrap_or((1, 1));
            let normal_mips = normal
                .and_then(|image| image.normal_mip_levels.as_deref())
                .unwrap_or(&flat_normal_mip_levels);
            let (normal_width, normal_height) = normal
                .map(|image| (image.width, image.height))
                .unwrap_or((1, 1));
            let occlusion = material_data
                .occlusion_image
                .map(|image| &model.images[image]);
            let (occlusion_mips, occlusion_width, occlusion_height) =
                occlusion.map_or((&default_texture_mip_levels[..], 1, 1), |image| {
                    (
                        image
                            .data_mip_levels
                            .as_deref()
                            .unwrap_or(&default_texture_mip_levels),
                        image.width,
                        image.height,
                    )
                });
            let emissive = material_data
                .emissive_image
                .map(|image| &model.images[image]);
            let (emissive_mips, emissive_width, emissive_height) =
                emissive.map_or((&default_texture_mip_levels[..], 1, 1), |image| {
                    (
                        image
                            .emissive_mip_levels
                            .as_deref()
                            .unwrap_or(&default_texture_mip_levels),
                        image.width,
                        image.height,
                    )
                });
            let specular = material_data
                .specular_image
                .map(|image| &model.images[image]);
            let (specular_mips, specular_width, specular_height) =
                specular.map_or((&default_texture_mip_levels[..], 1, 1), |image| {
                    (
                        image
                            .data_mip_levels
                            .as_deref()
                            .unwrap_or(&default_texture_mip_levels),
                        image.width,
                        image.height,
                    )
                });
            let specular_color = material_data
                .specular_color_image
                .map(|image| &model.images[image]);
            let (specular_color_mips, specular_color_width, specular_color_height) = specular_color
                .map_or((&default_texture_mip_levels[..], 1, 1), |image| {
                    (
                        image
                            .emissive_mip_levels
                            .as_deref()
                            .unwrap_or(&default_texture_mip_levels),
                        image.width,
                        image.height,
                    )
                });
            let transmission = material_data
                .transmission_image
                .map(|image| &model.images[image]);
            let (transmission_mips, transmission_width, transmission_height) =
                transmission.map_or((&default_texture_mip_levels[..], 1, 1), |image| {
                    (
                        image
                            .data_mip_levels
                            .as_deref()
                            .unwrap_or(&default_texture_mip_levels),
                        image.width,
                        image.height,
                    )
                });
            let thickness = material_data
                .thickness_image
                .map(|image| &model.images[image]);
            let (thickness_mips, thickness_width, thickness_height) =
                thickness.map_or((&default_texture_mip_levels[..], 1, 1), |image| {
                    (
                        image
                            .data_mip_levels
                            .as_deref()
                            .unwrap_or(&default_texture_mip_levels),
                        image.width,
                        image.height,
                    )
                });
            let label = format!(
                "{} material {index}",
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("gltf")
            );
            self.materials.push(Material::from_precomputed_mipmaps(
                &self.device,
                &self.queue,
                &self.texture_bind_group_layout,
                (base_color_mips, base_color_width, base_color_height),
                (metallic_roughness_mips, mr_width, mr_height),
                (normal_mips, normal_width, normal_height),
                (occlusion_mips, occlusion_width, occlusion_height),
                (emissive_mips, emissive_width, emissive_height),
                (specular_mips, specular_width, specular_height),
                (
                    specular_color_mips,
                    specular_color_width,
                    specular_color_height,
                ),
                (transmission_mips, transmission_width, transmission_height),
                (thickness_mips, thickness_width, thickness_height),
                MaterialParams {
                    base_color: material_data.base_color_factor,
                    roughness: material_data.roughness_factor,
                    metallic: material_data.metallic_factor,
                    unlit: if material_data.unlit { 1.0 } else { 0.0 },
                    alpha_mode: material_data.alpha_mode,
                    alpha_cutoff: material_data.alpha_cutoff,
                    double_sided: u32::from(material_data.double_sided),
                    transmission_factor: material_data.transmission_factor,
                    ior: material_data.ior,
                    specular_factor: material_data.specular_factor,
                    specular_color_factor: material_data.specular_color_factor,
                    pbr_workflow: u32::from(material_data.specular_glossiness),
                    emissive_factor: material_data.emissive_factor,
                    emissive_strength: material_data.emissive_strength,
                    occlusion_strength: material_data.occlusion_strength,
                    normal_scale: if material_data.normal_image.is_some() {
                        material_data.normal_scale
                    } else {
                        0.0
                    },
                    base_color_tex_coord: material_data.base_color_tex_coord,
                    metallic_roughness_tex_coord: material_data.metallic_roughness_tex_coord,
                    normal_tex_coord: material_data.normal_tex_coord,
                    occlusion_tex_coord: material_data.occlusion_tex_coord,
                    emissive_tex_coord: material_data.emissive_tex_coord,
                    specular_tex_coord: material_data.specular_tex_coord,
                    specular_color_tex_coord: material_data.specular_color_tex_coord,
                    transmission_tex_coord: material_data.transmission_tex_coord,
                    thickness_factor: material_data.thickness_factor,
                    attenuation_distance: material_data.attenuation_distance,
                    attenuation_color: material_data.attenuation_color,
                    thickness_tex_coord: material_data.thickness_tex_coord,
                    base_color_uv_transform: material_data.base_color_transform,
                    metallic_roughness_uv_transform: material_data.metallic_roughness_transform,
                    normal_uv_transform: material_data.normal_transform,
                    occlusion_uv_transform: material_data.occlusion_transform,
                    emissive_uv_transform: material_data.emissive_transform,
                    specular_uv_transform: material_data.specular_transform,
                    specular_color_uv_transform: material_data.specular_color_transform,
                    transmission_uv_transform: material_data.transmission_transform,
                    thickness_uv_transform: material_data.thickness_transform,
                    ..Default::default()
                },
                Some(&label),
            ));
        }

        let first_mesh_id = self.meshes.len();
        for (index, primitive) in model.primitives.iter().enumerate() {
            let label = format!(
                "{} primitive {index}",
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("gltf")
            );
            self.meshes.push(Mesh::from_mesh_data(
                &self.device,
                &primitive.mesh,
                Some(&label),
            ));
        }

        model
            .into_loaded(first_mesh_id, first_material_id)
            .map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn render(
        &mut self,
        camera: &Camera,
        world: &mut World,
        debug_ui: &mut DebugUi,
        window: &Window,
        frame_time: f32,
        fps: f32,
        cpu_frame_time: f32,
    ) {
        let ui_started = Instant::now();
        let previous_timings = self.timings;
        debug_ui.prepare_frame(
            window,
            frame_time,
            fps,
            cpu_frame_time,
            previous_timings,
            self,
            world,
        );
        self.timings.ui_cpu_ms = ui_started.elapsed().as_secs_f32() * 1000.0;
        let objects = world.objects();
        let scene_started = Instant::now();
        let key_direction = self.settings.key_light_direction();
        let light_uniform = LightUniform {
            direction_intensity: [
                key_direction.x,
                key_direction.y,
                key_direction.z,
                self.settings.key_light_intensity,
            ],
            color: [
                1.0,
                0.92,
                0.78,
                if self.settings.shadows_enabled {
                    1.0
                } else {
                    0.0
                },
            ],
            fill_direction_intensity: [0.55, -0.3, 0.65, self.settings.fill_light_intensity],
            fill_color: [0.62, 0.76, 1.0, self.settings.debug_view as f32],
            ambient_top: [0.0, 0.0, 0.0, self.settings.environment_intensity],
            ambient_bottom: [0.0, 0.0, 0.0, self.settings.exposure_ev],
        };
        self.queue
            .write_buffer(&self._light_buffer, 0, bytemuck::bytes_of(&light_uniform));
        let mut camera_uniform = CameraUniform::new();
        camera_uniform.update_view_proj(camera);
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera_uniform));

        for instances in self.batches.values_mut() {
            instances.clear();
        }
        self.transparent_objects.clear();
        self.skin_matrices.clear();
        self.skin_matrices.push(Mat4::IDENTITY);
        for (object_index, obj) in objects.iter().enumerate() {
            let model_matrix = world.group_model_matrix(obj);
            let Ok(skin_offset) = u32::try_from(self.skin_matrices.len()) else {
                eprintln!("Не удалось отрисовать объект: превышен диапазон индексов скелета");
                continue;
            };
            let Ok(skin_count) = u32::try_from(obj.skin_matrices().len()) else {
                eprintln!("Не удалось отрисовать объект: слишком большой скелет");
                continue;
            };
            self.skin_matrices.extend_from_slice(obj.skin_matrices());
            let instance = InstanceRaw::from_model(model_matrix, skin_offset, skin_count);
            if self
                .materials
                .get(obj.material_id.0)
                .is_some_and(|material| {
                    material.alpha_mode == 2 || material.params.transmission_factor > 0.0
                })
            {
                let position = model_matrix.transform_point3(glam::Vec3::ZERO);
                let distance_squared = position.distance_squared(camera.eye);
                self.transparent_objects.push((
                    distance_squared,
                    object_index,
                    obj.mesh_id,
                    obj.material_id,
                    instance,
                ));
            } else {
                self.batches
                    .entry((obj.mesh_id, obj.material_id))
                    .or_default()
                    .push(instance);
            }
        }
        self.transparent_objects.sort_unstable_by(|left, right| {
            right
                .0
                .total_cmp(&left.0)
                .then_with(|| left.1.cmp(&right.1))
        });

        self.all_instances.clear();
        self.draw_batches.clear();

        for (&(mesh_id, material_id), instances) in &self.batches {
            if instances.is_empty() {
                continue;
            }
            let start = self.all_instances.len() as u32;
            let count = instances.len() as u32;
            self.all_instances.extend_from_slice(instances);
            self.draw_batches
                .push((mesh_id, material_id, start..start + count, false));
        }

        for &(_, _, mesh_id, material_id, instance) in &self.transparent_objects {
            let start = self.all_instances.len() as u32;
            self.all_instances.push(instance);
            self.draw_batches
                .push((mesh_id, material_id, start..start + 1, true));
        }

        let skin_buffer_bytes = self.skin_matrices.len() * std::mem::size_of::<Mat4>();
        let max_storage_binding_size =
            self.device.limits().max_storage_buffer_binding_size as usize;
        if skin_buffer_bytes > max_storage_binding_size {
            eprintln!(
                "Не удалось отрисовать кадр: буфер матриц скелетов превышает лимит устройства ({max_storage_binding_size} байт)"
            );
            return;
        }
        if self.skin_matrices.len() > self.skin_matrix_capacity {
            self.skin_matrix_capacity = self
                .skin_matrices
                .len()
                .checked_next_power_of_two()
                .unwrap_or(self.skin_matrices.len())
                .min(max_storage_binding_size / std::mem::size_of::<Mat4>());
            self.skin_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("skin matrix buffer"),
                size: (self.skin_matrix_capacity * std::mem::size_of::<Mat4>()) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.shared_bind_group = create_shared_bind_group(
                &self.device,
                &self.shared_bind_group_layout,
                &self.camera_buffer,
                &self._light_buffer,
                &self.skin_buffer,
                &self.shadow_matrix_buffer,
                &self.shadow_view,
                &self.shadow_sampler,
            );
            self.shadow_shared_bind_group = create_shadow_bind_group(
                &self.device,
                &self.shadow_bind_group_layout,
                &self.skin_buffer,
                &self.shadow_matrix_buffer,
            );
        }
        self.queue.write_buffer(
            &self.skin_buffer,
            0,
            bytemuck::cast_slice(&self.skin_matrices),
        );

        if self.all_instances.len() > self.instance_capacity {
            self.instance_capacity = self
                .all_instances
                .len()
                .checked_next_power_of_two()
                .unwrap_or(self.all_instances.len());
            self.instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instance buffer"),
                size: (self.instance_capacity * std::mem::size_of::<InstanceRaw>())
                    as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        if !self.all_instances.is_empty() {
            self.queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.all_instances),
            );
        }

        let shadow_matrix = directional_shadow_matrix(key_direction);
        let opaque_instance_count = self
            .draw_batches
            .iter()
            .filter(|batch| !batch.3)
            .map(|batch| batch.2.end as usize - batch.2.start as usize)
            .sum::<usize>();
        let render_shadows = self.settings.shadows_enabled
            && (!self.shadow_cache_valid
                || self.cached_shadow_matrix != shadow_matrix
                || self.cached_shadow_instances != self.all_instances[..opaque_instance_count]
                || self.cached_shadow_skin_matrices != self.skin_matrices
                || !self
                    .draw_batches
                    .iter()
                    .filter(|batch| !batch.3)
                    .map(|(mesh, material, range, _)| (*mesh, *material, range.clone()))
                    .eq(self.cached_shadow_batches.iter().cloned()));

        self.timings.scene_cpu_ms = scene_started.elapsed().as_secs_f32() * 1000.0;
        let acquire_started = Instant::now();
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            Err(e) => {
                eprintln!("surface error: {e:?}");
                return;
            }
        };
        self.timings.acquire_cpu_ms = acquire_started.elapsed().as_secs_f32() * 1000.0;
        let view = frame.texture.create_view(&Default::default());

        let encode_started = Instant::now();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        if render_shadows {
            self.queue.write_buffer(
                &self.shadow_matrix_buffer,
                0,
                bytemuck::bytes_of(&shadow_matrix.to_cols_array_2d()),
            );
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("directional shadow pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, &self.shadow_shared_bind_group, &[]);
            pass.set_vertex_buffer(1, self.instance_buffer.slice(..));

            for (mesh_id, material_id, range, transparent) in &self.draw_batches {
                if *transparent {
                    continue;
                }
                let (Some(mesh), Some(material)) = (
                    self.meshes.get(mesh_id.0),
                    self.materials.get(material_id.0),
                ) else {
                    continue;
                };
                pass.set_bind_group(1, &material.bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_vertex_buffer(2, mesh.skin_buffer.slice(..));
                pass.set_vertex_buffer(3, mesh.tangent_buffer.slice(..));
                pass.set_vertex_buffer(4, mesh.uv_sets_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.num_indices, 0, range.clone());
            }
            self.cached_shadow_instances.clear();
            self.cached_shadow_instances
                .extend_from_slice(&self.all_instances[..opaque_instance_count]);
            self.cached_shadow_skin_matrices
                .clone_from(&self.skin_matrices);
            self.cached_shadow_batches.clear();
            self.cached_shadow_batches.extend(
                self.draw_batches
                    .iter()
                    .filter(|batch| !batch.3)
                    .map(|(mesh, material, range, _)| (*mesh, *material, range.clone())),
            );
            self.cached_shadow_matrix = shadow_matrix;
            self.shadow_cache_valid = true;
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("opaque scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.msaa_view,
                    resolve_target: Some(&self.scene_color_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.1,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_bind_group(0, &self.shared_bind_group, &[]);
            pass.set_bind_group(2, &self.environment_bind_group, &[]);
            pass.set_pipeline(&self.skybox_pipeline);
            pass.set_bind_group(1, &self.materials[0].bind_group, &[]);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(1, self.instance_buffer.slice(..));

            for (mesh_id, material_id, range, transparent) in &self.draw_batches {
                if *transparent {
                    continue;
                }
                let mesh_opt = self.meshes.get(mesh_id.0);
                let mat_opt = self.materials.get(material_id.0);

                match (mesh_opt, mat_opt) {
                    (Some(mesh), Some(material)) => {
                        pass.set_bind_group(1, &material.bind_group, &[]);
                        pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        pass.set_vertex_buffer(2, mesh.skin_buffer.slice(..));
                        pass.set_vertex_buffer(3, mesh.tangent_buffer.slice(..));
                        pass.set_vertex_buffer(4, mesh.uv_sets_buffer.slice(..));
                        pass.set_index_buffer(
                            mesh.index_buffer.slice(..),
                            wgpu::IndexFormat::Uint32,
                        );
                        pass.draw_indexed(0..mesh.num_indices, 0, range.clone());
                    }
                    (None, _) => {
                        if self.missing_mesh_warnings.insert(*mesh_id) {
                            eprintln!(
                                "[ПРЕДУПРЕЖДЕНИЕ] Объект не отрисован: отсутствует MeshId({})!",
                                mesh_id.0
                            );
                        }
                    }
                    (_, None) => {
                        if self.missing_material_warnings.insert(*material_id) {
                            eprintln!(
                                "[ПРЕДУПРЕЖДЕНИЕ] Объект не отрисован: отсутствует MaterialId({})!",
                                material_id.0
                            );
                        }
                    }
                }
            }
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("transparent scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.msaa_view,
                    resolve_target: Some(&self.hdr_output_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &self.shared_bind_group, &[]);
            pass.set_bind_group(2, &self.environment_bind_group, &[]);
            pass.set_vertex_buffer(1, self.instance_buffer.slice(..));

            for (mesh_id, material_id, range, transparent) in &self.draw_batches {
                if !transparent {
                    continue;
                }
                let mesh_opt = self.meshes.get(mesh_id.0);
                let mat_opt = self.materials.get(material_id.0);

                match (mesh_opt, mat_opt) {
                    (Some(mesh), Some(material)) => {
                        if material.params.transmission_factor > 0.0 {
                            pass.set_bind_group(3, &self.scene_color_bind_group, &[]);
                            pass.set_pipeline(&self.glass_pipeline);
                        } else {
                            pass.set_pipeline(&self.transparent_pipeline);
                        }
                        pass.set_bind_group(1, &material.bind_group, &[]);
                        pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        pass.set_vertex_buffer(2, mesh.skin_buffer.slice(..));
                        pass.set_vertex_buffer(3, mesh.tangent_buffer.slice(..));
                        pass.set_vertex_buffer(4, mesh.uv_sets_buffer.slice(..));
                        pass.set_index_buffer(
                            mesh.index_buffer.slice(..),
                            wgpu::IndexFormat::Uint32,
                        );
                        pass.draw_indexed(0..mesh.num_indices, 0, range.clone());
                    }
                    (None, _) => {
                        if self.missing_mesh_warnings.insert(*mesh_id) {
                            eprintln!(
                                "[ПРЕДУПРЕЖДЕНИЕ] Объект не отрисован: отсутствует MeshId({})!",
                                mesh_id.0
                            );
                        }
                    }
                    (_, None) => {
                        if self.missing_material_warnings.insert(*material_id) {
                            eprintln!(
                                "[ПРЕДУПРЕЖДЕНИЕ] Объект не отрисован: отсутствует MaterialId({})!",
                                material_id.0
                            );
                        }
                    }
                }
            }
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("HDR tone mapping pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.postprocess_pipeline);
            pass.set_bind_group(0, &self.shared_bind_group, &[]);
            pass.set_bind_group(1, &self.materials[0].bind_group, &[]);
            pass.set_bind_group(2, &self.environment_bind_group, &[]);
            pass.set_bind_group(3, &self.postprocess_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        let mut command_buffers = debug_ui.render(&self.device, &self.queue, &mut encoder, &view);
        command_buffers.push(encoder.finish());
        self.queue.submit(command_buffers);
        self.timings.encode_submit_cpu_ms = encode_started.elapsed().as_secs_f32() * 1000.0;
        frame.present();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_uniform_matches_wgsl_layout() {
        assert_eq!(std::mem::size_of::<LightUniform>(), 96);
    }

    #[test]
    fn render_scale_reduces_internal_resolution_and_keeps_small_sizes_valid() {
        assert_eq!(scaled_render_dimension(1680, 1.0), 1680);
        assert_eq!(scaled_render_dimension(1680, 0.75), 1260);
        assert_eq!(scaled_render_dimension(1, 0.5), 1);
    }

    #[test]
    fn shadow_camera_contains_scene_center_in_depth_range() {
        let clip = directional_shadow_matrix(RendererSettings::default().key_light_direction())
            * Vec3::new(0.0, 1.5, 0.0).extend(1.0);
        let ndc = clip.truncate() / clip.w;

        assert!(ndc.x.abs() < 1e-5);
        assert!(ndc.y.abs() < 1e-5);
        assert!((0.0..1.0).contains(&ndc.z));
    }

    #[test]
    fn parses_supported_assets_without_a_gpu_device() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let obj_path = manifest_dir
            .join("assets")
            .join("models")
            .join("sample.obj");
        let gltf_path = manifest_dir
            .join("assets")
            .join("models")
            .join("sample.gltf");

        assert!(matches!(parse_asset(&obj_path), Ok(ParsedAsset::Obj(_))));
        assert!(matches!(parse_asset(&gltf_path), Ok(ParsedAsset::Gltf(_))));
    }

    #[test]
    fn rejects_unsupported_asset_extensions() {
        let error = parse_asset(Path::new("model.fbx")).unwrap_err();

        assert!(error.contains("Unsupported model format"));
    }
}
