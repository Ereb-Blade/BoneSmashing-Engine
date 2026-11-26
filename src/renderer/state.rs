#![allow(
    clippy::large_enum_variant,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::collapsible_if
)]

use glam::{Mat4, Vec3};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Instant;
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::camera::{Camera, CameraUniform};
use crate::config::DEPTH_FORMAT;
use crate::core::{MATERIAL_DEFAULT, MESH_PLANE, MaterialId, MeshId};
use crate::renderer::gltf::{GltfModelData, LoadedGltfModel};
use crate::renderer::material::{Material, MaterialParams, generate_mip_levels_with_alpha};
use crate::renderer::mesh::{Mesh, MeshData, ObjModelData};
use crate::renderer::pipeline::{HDR_FORMAT, RenderPipelineBundle};
use crate::renderer::{debug_ui::DebugUi, environment::Environment};
use crate::scene::{LocalLight, LocalLightKind, MAX_LOCAL_LIGHTS, RenderObject, World};

#[derive(Debug)]
pub enum ParsedAsset {
    Gltf(GltfModelData),
    Obj(ObjModelData),
}

pub enum LoadedAsset {
    Gltf(LoadedGltfModel),
    Obj(LoadedObjModel),
}

#[derive(Debug)]
pub struct LoadedObjInstance {
    pub name: String,
    pub mesh_id: MeshId,
    pub material_id: MaterialId,
}

#[derive(Debug)]
pub struct LoadedObjModel {
    pub instances: Vec<LoadedObjInstance>,
}

pub fn parse_asset(path: &Path) -> Result<ParsedAsset, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("Unsupported model file: {}", path.display()))?;
    match extension.as_str() {
        "glb" | "gltf" => GltfModelData::load(path).map(ParsedAsset::Gltf),
        "obj" => ObjModelData::load(path)
            .map(ParsedAsset::Obj)
            .map_err(|error| format!("{}: {error}", path.display())),
        _ => Err(format!(
            "Unsupported model format: .{extension}. Choose .glb, .gltf, or .obj"
        )),
    }
}

#[repr(C)]
#[derive(Copy, Clone, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LocalLightUniform {
    pub position_range: [f32; 4],
    pub color_intensity: [f32; 4],
    pub direction_type: [f32; 4],
    pub spot_angles: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightUniform {
    pub direction_intensity: [f32; 4],
    pub color: [f32; 4],
    pub fill_direction_intensity: [f32; 4],
    pub fill_color: [f32; 4],
    pub ambient_top: [f32; 4],
    pub render_options: [f32; 4],
    pub local_light_info: [u32; 4],
    pub local_lights: [LocalLightUniform; MAX_LOCAL_LIGHTS],
}

const SHADOW_MAP_SIZE: u32 = 4096;

fn build_light_uniform(settings: RendererSettings, world: &World) -> LightUniform {
    let key_direction = settings.key_light_direction();
    let mut lights = [LocalLightUniform::default(); MAX_LOCAL_LIGHTS];
    let mut light_count = 0_u32;
    for (index, light) in world.local_lights().iter().enumerate() {
        if !light.enabled {
            continue;
        }
        debug_assert!((light_count as usize) < MAX_LOCAL_LIGHTS);
        let position = world
            .local_light_world_position(index)
            .expect("light index comes from the same world");
        let direction = world
            .local_light_world_direction(index)
            .expect("light index comes from the same world");
        lights[light_count as usize] = LocalLightUniform {
            position_range: [position.x, position.y, position.z, light.range],
            color_intensity: [
                light.color[0],
                light.color[1],
                light.color[2],
                light.intensity,
            ],
            direction_type: [
                direction.x,
                direction.y,
                direction.z,
                if light.kind == LocalLightKind::Spot {
                    1.0
                } else {
                    0.0
                },
            ],
            spot_angles: [
                light.inner_cone_degrees.to_radians().cos(),
                light.outer_cone_degrees.to_radians().cos(),
                0.0,
                0.0,
            ],
        };
        light_count += 1;
    }

    LightUniform {
        direction_intensity: [
            key_direction.x,
            key_direction.y,
            key_direction.z,
            settings.key_light_intensity,
        ],
        color: [
            1.0,
            0.92,
            0.78,
            if settings.shadows_enabled { 1.0 } else { 0.0 },
        ],
        fill_direction_intensity: [0.55, -0.3, 0.65, settings.fill_light_intensity],
        fill_color: [0.62, 0.76, 1.0, settings.debug_view as f32],
        ambient_top: [0.0, 0.0, 0.0, settings.environment_intensity],
        render_options: [
            settings.shadow_pcf_samples as f32,
            0.0,
            0.0,
            settings.exposure_ev,
        ],
        local_light_info: [light_count, 0, 0, 0],
        local_lights: lights,
    }
}

fn scaled_render_dimension(dimension: u32, scale: f32) -> u32 {
    ((dimension as f32 * scale).round() as u32).max(1)
}

fn persisted_environment_path() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .map(|path| path.join("MyEngine").join("environment.txt"))
}

fn supported_msaa_sample_counts(adapter: &wgpu::Adapter) -> Vec<u32> {
    let hdr_flags = adapter.get_texture_format_features(HDR_FORMAT).flags;
    let depth_flags = adapter.get_texture_format_features(DEPTH_FORMAT).flags;
    supported_msaa_sample_counts_for_flags(hdr_flags, depth_flags)
}

fn supported_msaa_sample_counts_for_flags(
    hdr_flags: wgpu::TextureFormatFeatureFlags,
    depth_flags: wgpu::TextureFormatFeatureFlags,
) -> Vec<u32> {
    [1, 2, 4, 8]
        .into_iter()
        .filter(|&count| {
            count == 1
                || (hdr_flags.sample_count_supported(count)
                    && hdr_flags.contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE)
                    && depth_flags.sample_count_supported(count))
        })
        .collect()
}

fn default_msaa_sample_count(supported: &[u32]) -> u32 {
    [4, 2, 1]
        .into_iter()
        .find(|count| supported.contains(count))
        .unwrap_or(1)
}

fn select_surface_format(caps: &wgpu::SurfaceCapabilities) -> Result<wgpu::TextureFormat, String> {
    caps.formats
        .iter()
        .copied()
        .find(|format| format.is_srgb())
        .or_else(|| caps.formats.first().copied())
        .ok_or_else(|| "The GPU surface reports no supported texture formats".to_owned())
}

fn select_alpha_mode(caps: &wgpu::SurfaceCapabilities) -> Result<wgpu::CompositeAlphaMode, String> {
    caps.alpha_modes
        .first()
        .copied()
        .ok_or_else(|| "The GPU surface reports no supported alpha modes".to_owned())
}

fn select_present_mode(caps: &wgpu::SurfaceCapabilities) -> Result<wgpu::PresentMode, String> {
    [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Fifo]
        .into_iter()
        .find(|mode| caps.present_modes.contains(mode))
        .or_else(|| caps.present_modes.first().copied())
        .ok_or_else(|| "The GPU surface reports no supported present modes".to_owned())
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
    pub shadow_pcf_samples: u32,
    pub debug_view: u32,
    pub unlocked_fps: bool,
    pub render_scale: f32,
    pub msaa_samples: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RenderTimings {
    pub ui_cpu_ms: f32,
    pub scene_cpu_ms: f32,
    pub acquire_cpu_ms: f32,
    pub encode_submit_cpu_ms: f32,
    pub gpu_frame_ms: Option<f32>,
    pub gpu_shadow_map_ms: Option<f32>,
    pub gpu_opaque_pass_ms: Option<f32>,
    pub gpu_sample_id: u64,
    pub draw_calls: usize,
    pub triangle_count: usize,
    pub active_meshes: usize,
    pub active_materials: usize,
    pub render_batches: usize,
    pub active_instances: usize,
}

const GPU_TIMESTAMP_FRAME_START: u32 = 0;
const GPU_TIMESTAMP_SHADOW_START: u32 = 1;
const GPU_TIMESTAMP_SHADOW_END: u32 = 2;
const GPU_TIMESTAMP_OPAQUE_START: u32 = 3;
const GPU_TIMESTAMP_OPAQUE_END: u32 = 4;
const GPU_TIMESTAMP_FRAME_END: u32 = 5;
const GPU_TIMESTAMP_QUERY_COUNT: u32 = 6;
const GPU_TIMESTAMP_BUFFER_SIZE: u64 = GPU_TIMESTAMP_QUERY_COUNT as u64 * wgpu::QUERY_SIZE as u64;

#[derive(Clone, Copy, Debug, PartialEq)]
struct GpuTimingSample {
    frame_ms: f32,
    shadow_map_ms: f32,
    opaque_pass_ms: f32,
}

struct GpuTimestampProfiler {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    pending_readback: Option<mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
    timestamp_period_ns: f32,
}

fn gpu_timestamp_features(adapter_features: wgpu::Features) -> wgpu::Features {
    let required =
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    if adapter_features.contains(required) {
        required
    } else {
        wgpu::Features::empty()
    }
}

fn timestamp_delta_ms(start: u64, end: u64, timestamp_period_ns: f32) -> Option<f32> {
    let ticks = end.checked_sub(start)?;
    if !timestamp_period_ns.is_finite() || timestamp_period_ns <= 0.0 {
        return None;
    }

    let milliseconds = (ticks as f64 * timestamp_period_ns as f64 / 1_000_000.0) as f32;
    milliseconds.is_finite().then_some(milliseconds)
}

fn gpu_timing_sample(
    timestamps: [u64; GPU_TIMESTAMP_QUERY_COUNT as usize],
    timestamp_period_ns: f32,
) -> Option<GpuTimingSample> {
    Some(GpuTimingSample {
        frame_ms: timestamp_delta_ms(
            timestamps[GPU_TIMESTAMP_FRAME_START as usize],
            timestamps[GPU_TIMESTAMP_FRAME_END as usize],
            timestamp_period_ns,
        )?,
        shadow_map_ms: timestamp_delta_ms(
            timestamps[GPU_TIMESTAMP_SHADOW_START as usize],
            timestamps[GPU_TIMESTAMP_SHADOW_END as usize],
            timestamp_period_ns,
        )?,
        opaque_pass_ms: timestamp_delta_ms(
            timestamps[GPU_TIMESTAMP_OPAQUE_START as usize],
            timestamps[GPU_TIMESTAMP_OPAQUE_END as usize],
            timestamp_period_ns,
        )?,
    })
}

impl GpuTimestampProfiler {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        let timestamp_period_ns = queue.get_timestamp_period();
        if !timestamp_period_ns.is_finite() || timestamp_period_ns <= 0.0 {
            return None;
        }

        Some(Self {
            query_set: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("GPU frame timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: GPU_TIMESTAMP_QUERY_COUNT,
            }),
            resolve_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("GPU timestamp resolve buffer"),
                size: GPU_TIMESTAMP_BUFFER_SIZE,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("GPU timestamp readback buffer"),
                size: GPU_TIMESTAMP_BUFFER_SIZE,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            pending_readback: None,
            timestamp_period_ns,
        })
    }

    fn write_start(&self, encoder: &mut wgpu::CommandEncoder) -> bool {
        if self.pending_readback.is_some() {
            return false;
        }
        encoder.write_timestamp(&self.query_set, GPU_TIMESTAMP_FRAME_START);
        true
    }

    fn write_timestamp(&self, encoder: &mut wgpu::CommandEncoder, query_index: u32) {
        encoder.write_timestamp(&self.query_set, query_index);
    }

    fn write_end_and_resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.write_timestamp(&self.query_set, GPU_TIMESTAMP_FRAME_END);
        encoder.resolve_query_set(
            &self.query_set,
            0..GPU_TIMESTAMP_QUERY_COUNT,
            &self.resolve_buffer,
            0,
        );
        encoder.copy_buffer_to_buffer(
            &self.resolve_buffer,
            0,
            &self.readback_buffer,
            0,
            GPU_TIMESTAMP_BUFFER_SIZE,
        );
    }

    fn map_after_submit(&mut self) {
        let (sender, receiver) = mpsc::channel();
        self.readback_buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.pending_readback = Some(receiver);
    }

    fn poll(&mut self, device: &wgpu::Device) -> Result<Option<GpuTimingSample>, String> {
        device
            .poll(wgpu::PollType::Poll)
            .map_err(|error| format!("Could not poll GPU timestamp queries: {error}"))?;

        let Some(receiver) = self.pending_readback.as_ref() else {
            return Ok(None);
        };
        let completion = receiver.try_recv();

        match completion {
            Ok(Ok(())) => {
                let timestamps = {
                    let mapped = self.readback_buffer.slice(..).get_mapped_range();
                    bytemuck::try_cast_slice::<u8, u64>(&mapped)
                        .ok()
                        .filter(|values| values.len() == GPU_TIMESTAMP_QUERY_COUNT as usize)
                        .and_then(|values| values.try_into().ok())
                };
                self.readback_buffer.unmap();
                self.pending_readback = None;

                let Some(timestamps) = timestamps else {
                    return Err("GPU timestamp readback had an invalid size".to_owned());
                };
                gpu_timing_sample(timestamps, self.timestamp_period_ns)
                    .map(Some)
                    .ok_or_else(|| "GPU timestamp values were invalid".to_owned())
            }
            Ok(Err(error)) => {
                self.pending_readback = None;
                Err(format!("Could not map GPU timestamp results: {error}"))
            }
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending_readback = None;
                Err("GPU timestamp readback channel disconnected".to_owned())
            }
        }
    }
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
            shadow_pcf_samples: 8,
            debug_view: 0,
            unlocked_fps: false,
            render_scale: 0.75,
            msaa_samples: 1,
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

fn create_depth_view(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    sample_count: u32,
) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count,
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
    copy_usage: wgpu::TextureUsages,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scene color snapshot"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | copy_usage,
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
    sample_count: u32,
) -> Option<wgpu::TextureView> {
    if sample_count == 1 {
        return None;
    }
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("msaa_color"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    Some(tex.create_view(&Default::default()))
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
    let eye = center - ray_direction * 20.0;
    let up = if ray_direction.dot(Vec3::Y).abs() > 0.98 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let view = crate::math::look_at_rh(eye, center, up);
    crate::math::orthographic_rh(-7.5, 7.5, -7.5, 7.5, 1.0, 40.0) * view
}

fn directional_shadow_matrix_for_bounds(ray_direction: Vec3, bounds: &[(Vec3, f32)]) -> Mat4 {
    if !ray_direction.is_finite() || ray_direction.length_squared() <= f32::EPSILON {
        return directional_shadow_matrix(Vec3::NEG_Y);
    }

    let ray_direction = ray_direction.normalize();
    let up = if ray_direction.dot(Vec3::Y).abs() > 0.98 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let light_orientation = crate::math::look_at_rh(Vec3::ZERO, ray_direction, up);
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut found_bounds = false;

    for &(world_center, radius) in bounds {
        if !world_center.is_finite() || !radius.is_finite() || radius < 0.0 {
            continue;
        }

        let center = light_orientation.transform_point3(world_center);
        let extent = Vec3::splat(radius);
        min = min.min(center - extent);
        max = max.max(center + extent);
        found_bounds = true;
    }

    if !found_bounds {
        return directional_shadow_matrix(ray_direction);
    }

    let light_center = (min + max) * 0.5;
    let half_extents = (max - min) * 0.5;
    let xy_padding = (half_extents.x.max(half_extents.y) * 0.05).max(0.5);
    let half_width = (half_extents.x + xy_padding).max(1.0);
    let half_height = (half_extents.y + xy_padding).max(1.0);
    let depth_padding = (half_extents.z * 0.05).max(1.0);
    let light_distance = half_extents.z + depth_padding + 1.0;
    let world_center = light_orientation.inverse().transform_point3(light_center);
    let eye = world_center - ray_direction * light_distance;
    let view = crate::math::look_at_rh(eye, world_center, up);
    let projection = crate::math::orthographic_rh(
        -half_width,
        half_width,
        -half_height,
        half_height,
        0.1,
        light_distance + half_extents.z + depth_padding,
    );

    projection * view
}

fn object_in_view(
    bounds: Option<(Vec3, f32)>,
    model: Mat4,
    is_skinned: bool,
    view_projection: Mat4,
) -> bool {
    if is_skinned {
        return true;
    }
    let Some((bounds_center, bounds_radius)) = bounds else {
        return true;
    };
    if bounds_radius <= 0.0 {
        return true;
    }
    let world_center = model.transform_point3(bounds_center);
    let conservative_scale = (model.x_axis.truncate().length_squared()
        + model.y_axis.truncate().length_squared()
        + model.z_axis.truncate().length_squared())
    .sqrt();
    sphere_in_view(
        world_center,
        bounds_radius * conservative_scale,
        view_projection,
    )
}

fn sphere_in_view(center: Vec3, radius: f32, view_projection: Mat4) -> bool {
    let columns = view_projection.to_cols_array_2d();
    let row = |index: usize| {
        glam::Vec4::new(
            columns[0][index],
            columns[1][index],
            columns[2][index],
            columns[3][index],
        )
    };
    let row0 = row(0);
    let row1 = row(1);
    let row2 = row(2);
    let row3 = row(3);
    let mut planes = [
        row3 + row0,
        row3 - row0,
        row3 + row1,
        row3 - row1,
        row2,
        row3 - row2,
    ];

    for plane in &mut planes {
        let length = plane.truncate().length();
        if !length.is_finite() || length <= f32::EPSILON {
            return true;
        }
        *plane /= length;
    }

    let center = center.extend(1.0);
    planes
        .into_iter()
        .all(|plane| plane.dot(center) >= -(radius))
}

const INITIAL_INSTANCE_CAPACITY: usize = 256;
const INITIAL_SKIN_MATRIX_CAPACITY: usize = 256;
type DrawBatch = (MeshId, MaterialId, std::ops::Range<u32>, bool);
type TransparentObject = (f32, usize, MeshId, MaterialId, InstanceRaw);
type ShadowBatch = (MeshId, MaterialId, std::ops::Range<u32>);

fn transparent_depth(world_center: Vec3, camera_eye: Vec3, camera_forward: Vec3) -> f32 {
    (world_center - camera_eye).dot(camera_forward)
}

fn sort_transparent_objects(objects: &mut [TransparentObject]) {
    objects.sort_unstable_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
    });
}

fn should_prepare_instance(camera_visible: bool, casts_shadow: bool) -> bool {
    camera_visible || casts_shadow
}

fn uses_glass_pipeline(transmission_factor: f32) -> bool {
    transmission_factor > 0.0
}

fn is_transparent_material(alpha_mode: u32, transmission_factor: f32) -> bool {
    alpha_mode == 2 || uses_glass_pipeline(transmission_factor)
}

fn material_casts_shadow(transmission_factor: f32) -> bool {
    !uses_glass_pipeline(transmission_factor)
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    surface_ready: bool,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    adapter_name: String,
    pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    glass_pipeline: wgpu::RenderPipeline,
    skybox_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    postprocess_pipeline: wgpu::RenderPipeline,
    pipeline_layout: wgpu::PipelineLayout,
    glass_pipeline_layout: wgpu::PipelineLayout,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    shared_bind_group_layout: wgpu::BindGroupLayout,
    shadow_bind_group_layout: wgpu::BindGroupLayout,
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    shadow_instance_buffer: wgpu::Buffer,
    shadow_instance_capacity: usize,
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
    supported_msaa_samples: Vec<u32>,
    depth_view: wgpu::TextureView,
    msaa_view: Option<wgpu::TextureView>,
    missing_mesh_warnings: HashSet<MeshId>,
    missing_material_warnings: HashSet<MaterialId>,
    shadow_bounds: Vec<(Vec3, f32)>,
    batches: HashMap<(MeshId, MaterialId), Vec<InstanceRaw>>,
    shadow_batches: HashMap<(MeshId, MaterialId), Vec<InstanceRaw>>,
    transparent_objects: Vec<TransparentObject>,
    skin_matrices: Vec<Mat4>,
    all_instances: Vec<InstanceRaw>,
    shadow_instances: Vec<InstanceRaw>,
    draw_batches: Vec<DrawBatch>,
    shadow_draw_batches: Vec<ShadowBatch>,
    active_mesh_ids: HashSet<MeshId>,
    active_material_ids: HashSet<MaterialId>,
    cached_shadow_instances: Vec<InstanceRaw>,
    cached_shadow_skin_matrices: Vec<Mat4>,
    cached_shadow_batches: Vec<ShadowBatch>,
    cached_shadow_matrix: Mat4,
    shadow_cache_valid: bool,
    timings: RenderTimings,
    gpu_timestamp_profiler: Option<GpuTimestampProfiler>,
    environment_load: Option<mpsc::Receiver<(Option<PathBuf>, Result<Environment, String>)>>,
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
    pub async fn new(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .map_err(|error| format!("Could not create window surface: {error}"))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|error| format!("Could not find a suitable GPU adapter: {error:?}"))?;
        let adapter_name = adapter.get_info().name;

        let mut device_descriptor = wgpu::DeviceDescriptor::default();
        let required_sampled_textures = 18;
        if adapter.limits().max_sampled_textures_per_shader_stage < required_sampled_textures {
            return Err(
                "GPU does not support the 18 sampled textures per shader stage required by material rendering"
                    .to_owned(),
            );
        }
        device_descriptor
            .required_limits
            .max_sampled_textures_per_shader_stage = required_sampled_textures;
        let adapter_features = adapter.features();
        if adapter_features.contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES) {
            device_descriptor.required_features |=
                wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        }
        let timestamp_features = gpu_timestamp_features(adapter_features);
        device_descriptor.required_features |= timestamp_features;
        let (device, queue) = adapter
            .request_device(&device_descriptor)
            .await
            .map_err(|error| format!("Could not create GPU device: {error}"))?;
        let gpu_timestamp_profiler = if timestamp_features.is_empty() {
            None
        } else {
            GpuTimestampProfiler::new(&device, &queue)
        };
        device.on_uncaptured_error(Box::new(move |error| {
            eprintln!("GPU uncaptured error: {error:?}");
        }));
        let supported_msaa_samples = supported_msaa_sample_counts(&adapter);
        let mut settings = RendererSettings::default();
        if !supported_msaa_samples.contains(&settings.msaa_samples) {
            settings.msaa_samples = default_msaa_sample_count(&supported_msaa_samples);
        }

        let caps = surface.get_capabilities(&adapter);
        let format = select_surface_format(&caps)?;
        let alpha_mode = select_alpha_mode(&caps)?;
        let present_mode = select_present_mode(&caps)?;

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let depth_view =
            create_depth_view(&device, config.width, config.height, settings.msaa_samples);
        let msaa_view = create_msaa_color_view(
            &device,
            config.width,
            config.height,
            HDR_FORMAT,
            settings.msaa_samples,
        );
        let (scene_color_texture, scene_color_view) = create_scene_color_texture(
            &device,
            config.width,
            config.height,
            HDR_FORMAT,
            wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
        );
        let (hdr_output_texture, hdr_output_view) = create_scene_color_texture(
            &device,
            config.width,
            config.height,
            HDR_FORMAT,
            wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        );

        let camera_uniform = CameraUniform::new();
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera buffer"),
            contents: bytemuck::bytes_of(&camera_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let light_uniform = build_light_uniform(settings, &World::new());
        let light_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("light buffer"),
            contents: bytemuck::bytes_of(&light_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // Создаем пайплайн и бинд-групп лейауты через модуль pipeline
        let pipeline_bundle = RenderPipelineBundle::new(&device, format, settings.msaa_samples);
        let environment = Environment::fallback(&device, &queue);
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
        let shadow_matrix = directional_shadow_matrix(settings.key_light_direction());
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
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
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
                specular_factor: 0.0,
                ..Default::default()
            },
            Some("dark grey floor"),
        )
        .map_err(|error| format!("Could not create default material: {error}"))?;

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
        let shadow_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow instance buffer"),
            size: (INITIAL_INSTANCE_CAPACITY * std::mem::size_of::<InstanceRaw>())
                as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut renderer = Self {
            surface,
            device,
            surface_ready: size.width > 0 && size.height > 0,
            queue,
            config,
            adapter_name,
            pipeline: pipeline_bundle.pipeline,
            transparent_pipeline: pipeline_bundle.transparent_pipeline,
            glass_pipeline: pipeline_bundle.glass_pipeline,
            skybox_pipeline: pipeline_bundle.skybox_pipeline,
            pipeline_layout: pipeline_bundle.pipeline_layout,
            glass_pipeline_layout: pipeline_bundle.glass_pipeline_layout,
            shadow_pipeline: pipeline_bundle.shadow_pipeline,
            postprocess_pipeline: pipeline_bundle.postprocess_pipeline,
            texture_bind_group_layout: pipeline_bundle.texture_bind_group_layout,
            shared_bind_group_layout: pipeline_bundle.shared_bind_group_layout,
            shadow_bind_group_layout: pipeline_bundle.shadow_bind_group_layout,
            meshes,
            materials,
            instance_buffer,
            instance_capacity: INITIAL_INSTANCE_CAPACITY,
            shadow_instance_buffer,
            shadow_instance_capacity: INITIAL_INSTANCE_CAPACITY,
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
            supported_msaa_samples,
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
            shadow_bounds: Vec::new(),
            batches: HashMap::new(),
            shadow_batches: HashMap::new(),
            transparent_objects: Vec::new(),
            skin_matrices: Vec::with_capacity(INITIAL_SKIN_MATRIX_CAPACITY),
            all_instances: Vec::new(),
            shadow_instances: Vec::new(),
            draw_batches: Vec::new(),
            shadow_draw_batches: Vec::new(),
            active_mesh_ids: HashSet::new(),
            active_material_ids: HashSet::new(),
            cached_shadow_instances: Vec::new(),
            cached_shadow_skin_matrices: Vec::new(),
            cached_shadow_batches: Vec::new(),
            cached_shadow_matrix: Mat4::IDENTITY,
            shadow_cache_valid: false,
            timings: RenderTimings::default(),
            gpu_timestamp_profiler,
            environment_load: None,
        };
        renderer.start_initial_environment_load();
        Ok(renderer)
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            self.surface_ready = false;
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
        self.resize_render_targets();
        self.surface_ready = true;
    }

    pub fn is_surface_ready(&self) -> bool {
        self.surface_ready
    }

    fn resize_render_targets(&mut self) {
        let w = scaled_render_dimension(self.config.width, self.settings.render_scale);
        let h = scaled_render_dimension(self.config.height, self.settings.render_scale);
        self.resize_msaa_targets(w, h);
        let (scene_color_texture, scene_color_view) = create_scene_color_texture(
            &self.device,
            w,
            h,
            HDR_FORMAT,
            wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
        );
        let (scene_color_sampler, scene_color_bind_group) = create_scene_color_bind_group(
            &self.device,
            &self.scene_color_bind_group_layout,
            &scene_color_view,
        );
        let (hdr_output_texture, hdr_output_view) = create_scene_color_texture(
            &self.device,
            w,
            h,
            HDR_FORMAT,
            wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        );
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

    fn resize_msaa_targets(&mut self, w: u32, h: u32) {
        self.depth_view = create_depth_view(&self.device, w, h, self.settings.msaa_samples);
        self.msaa_view =
            create_msaa_color_view(&self.device, w, h, HDR_FORMAT, self.settings.msaa_samples);
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub fn render_dimensions(&self) -> (u32, u32) {
        (
            scaled_render_dimension(self.config.width, self.settings.render_scale),
            scaled_render_dimension(self.config.height, self.settings.render_scale),
        )
    }

    pub fn surface_dimensions(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
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
        if !self.supported_msaa_samples.contains(&settings.msaa_samples) {
            settings.msaa_samples = self.settings.msaa_samples;
        }
        if ![2, 4, 8, 16].contains(&settings.shadow_pcf_samples) {
            settings.shadow_pcf_samples = self.settings.shadow_pcf_samples;
        }
        let render_scale_changed = self.settings.render_scale != settings.render_scale;
        let msaa_samples_changed = self.settings.msaa_samples != settings.msaa_samples;
        if self.settings.unlocked_fps != settings.unlocked_fps {
            self.config.present_mode = if settings.unlocked_fps {
                wgpu::PresentMode::AutoNoVsync
            } else {
                wgpu::PresentMode::Fifo
            };
            self.surface.configure(&self.device, &self.config);
        }
        self.settings = settings;
        if msaa_samples_changed {
            let shader = self
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("shader"),
                    source: wgpu::ShaderSource::Wgsl(crate::renderer::pipeline::SHADER.into()),
                });
            let (pipeline, transparent_pipeline, glass_pipeline, skybox_pipeline) =
                RenderPipelineBundle::create_multisample_pipelines(
                    &self.device,
                    &shader,
                    &self.pipeline_layout,
                    &self.glass_pipeline_layout,
                    settings.msaa_samples,
                );
            self.pipeline = pipeline;
            self.transparent_pipeline = transparent_pipeline;
            self.glass_pipeline = glass_pipeline;
            self.skybox_pipeline = skybox_pipeline;
        }
        if render_scale_changed {
            self.resize_render_targets();
        } else if msaa_samples_changed {
            let w = scaled_render_dimension(self.config.width, settings.render_scale);
            let h = scaled_render_dimension(self.config.height, settings.render_scale);
            self.resize_msaa_targets(w, h);
        }
    }

    pub fn supported_msaa_samples(&self) -> &[u32] {
        &self.supported_msaa_samples
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

    pub fn environment_path(&self) -> Option<&Path> {
        self.environment.source_path.as_deref()
    }

    pub fn environment_mip_count(&self) -> u32 {
        self.environment.mip_count
    }

    pub fn environment_is_loading(&self) -> bool {
        self.environment_load.is_some()
    }

    pub fn begin_environment_load(&mut self, path: PathBuf) -> Result<(), String> {
        if self.environment_load.is_some() {
            return Err("An HDR environment is already loading".to_owned());
        }

        let worker_path = path.clone();
        self.start_environment_task(Some(path), move |device, queue| {
            Environment::load(
                device,
                queue,
                &worker_path,
                device.limits().max_texture_dimension_2d,
            )
        })
    }

    pub fn begin_default_environment_load(&mut self) -> Result<(), String> {
        self.start_environment_task(None, |device, queue| Ok(Environment::studio(device, queue)))
    }

    fn start_initial_environment_load(&mut self) {
        let result = self.start_environment_task(None, move |device, queue| {
            if let Some(settings_path) = persisted_environment_path() {
                match std::fs::read_to_string(&settings_path) {
                    Ok(saved_path) => {
                        let saved_path = PathBuf::from(saved_path.trim());
                        if saved_path.is_file() {
                            match Environment::load(
                                device,
                                queue,
                                &saved_path,
                                device.limits().max_texture_dimension_2d,
                            ) {
                                Ok(environment) => return Ok(environment),
                                Err(error) => {
                                    eprintln!("Could not restore saved HDR environment: {error}");
                                }
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        eprintln!(
                            "Could not read saved HDR environment path {}: {error}",
                            settings_path.display()
                        );
                    }
                }
            }
            Ok(Environment::studio(device, queue))
        });
        if let Err(error) = result {
            eprintln!("Could not start initial environment loader: {error}");
        }
    }

    fn start_environment_task(
        &mut self,
        path: Option<PathBuf>,
        load: impl FnOnce(&wgpu::Device, &wgpu::Queue) -> Result<Environment, String> + Send + 'static,
    ) -> Result<(), String> {
        if self.environment_load.is_some() {
            return Err("An HDR environment is already loading".to_owned());
        }
        let device = self.device.clone();
        let queue = self.queue.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("hdr-environment-loader".to_owned())
            .spawn(move || {
                let result = load(&device, &queue);
                let _ = sender.send((path, result));
            })
            .map_err(|error| format!("Could not start HDR environment loader: {error}"))?;
        self.environment_load = Some(receiver);
        Ok(())
    }

    pub fn poll_environment_load(&mut self) -> Option<Result<Option<PathBuf>, String>> {
        let result = match self.environment_load.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.environment_load = None;
                return Some(Err("HDR environment loader stopped unexpectedly".to_owned()));
            }
        };
        self.environment_load = None;

        let (path, environment) = match result {
            (path, Ok(environment)) => (path, environment),
            (_, Err(error)) => return Some(Err(error)),
        };
        let bind_group = create_environment_bind_group(
            &self.device,
            &self.environment_bind_group_layout,
            &environment,
        );
        self.environment = environment;
        self.environment_bind_group = bind_group;
        if let Some(path) = path {
            if let Some(settings_path) = persisted_environment_path() {
                let result = settings_path
                    .parent()
                    .map(std::fs::create_dir_all)
                    .transpose()
                    .and_then(|_| {
                        std::fs::write(&settings_path, path.to_string_lossy().as_bytes())
                    });
                if let Err(error) = result {
                    eprintln!(
                        "HDR environment loaded but its path could not be saved to {}: {error}",
                        settings_path.display()
                    );
                }
            }
            return Some(Ok(Some(path)));
        }
        Some(Ok(None))
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
            ParsedAsset::Obj(model) => self.upload_obj_model(model, path).map(LoadedAsset::Obj),
        }
    }

    fn upload_obj_model(
        &mut self,
        model: ObjModelData,
        path: &Path,
    ) -> Result<LoadedObjModel, String> {
        let limits = self.device.limits();
        let max_buffer_size = limits.max_buffer_size;
        for (index, texture) in model.textures.iter().enumerate() {
            if texture.width > limits.max_texture_dimension_2d
                || texture.height > limits.max_texture_dimension_2d
            {
                return Err(format!(
                    "{}: OBJ texture {index} exceeds the device's maximum 2D texture dimension",
                    path.display()
                ));
            }
        }
        for (index, primitive) in model.primitives.iter().enumerate() {
            let vertex_bytes = std::mem::size_of_val(primitive.mesh.vertices.as_slice()) as u64;
            let uv_bytes = std::mem::size_of_val(primitive.mesh.uv_sets.as_slice()) as u64;
            let skin_bytes = std::mem::size_of_val(primitive.mesh.skin_vertices.as_slice()) as u64;
            let tangent_bytes = std::mem::size_of_val(primitive.mesh.tangents.as_slice()) as u64;
            let index_bytes = std::mem::size_of_val(primitive.mesh.indices.as_slice()) as u64;
            if vertex_bytes > max_buffer_size
                || uv_bytes > max_buffer_size
                || skin_bytes > max_buffer_size
                || tangent_bytes > max_buffer_size
                || index_bytes > max_buffer_size
                || primitive
                    .material_index
                    .is_some_and(|material| material >= model.materials.len())
            {
                return Err(format!(
                    "{}: OBJ primitive {index} exceeds a device limit or references a missing material",
                    path.display()
                ));
            }
        }
        for material in &model.materials {
            if material
                .diffuse_texture
                .is_some_and(|image| image >= model.textures.len())
                || material
                    .ambient_texture
                    .is_some_and(|image| image >= model.textures.len())
                || material
                    .normal_texture
                    .is_some_and(|image| image >= model.textures.len())
                || !material.diffuse.iter().all(|value| value.is_finite())
                || !material.specular.iter().all(|value| value.is_finite())
                || !material.shininess.is_finite()
                || !material.dissolve.is_finite()
            {
                return Err(format!(
                    "{}: OBJ material contains invalid values or references a missing texture",
                    path.display()
                ));
            }
        }

        let white_mips = [vec![255; 4]];
        let flat_normal_mips = [vec![128, 128, 255, 255]];
        let first_material_id = self.materials.len();
        for material in &model.materials {
            let diffuse = material.diffuse_texture.map(|index| &model.textures[index]);
            let diffuse_mips = diffuse
                .map(|texture| {
                    generate_mip_levels_with_alpha(
                        &texture.rgba,
                        texture.width,
                        texture.height,
                        true,
                        false,
                        true,
                    )
                })
                .unwrap_or_else(|| white_mips.to_vec());
            let (diffuse_width, diffuse_height) = diffuse
                .map(|texture| (texture.width, texture.height))
                .unwrap_or((1, 1));

            let normal = material.normal_texture.map(|index| &model.textures[index]);
            let normal_mips = normal
                .map(|texture| {
                    generate_mip_levels_with_alpha(
                        &texture.rgba,
                        texture.width,
                        texture.height,
                        false,
                        true,
                        false,
                    )
                })
                .unwrap_or_else(|| flat_normal_mips.to_vec());
            let (normal_width, normal_height) = normal
                .map(|texture| (texture.width, texture.height))
                .unwrap_or((1, 1));

            let occlusion = material.ambient_texture.map(|index| &model.textures[index]);
            let occlusion_mips = occlusion
                .map(|texture| {
                    generate_mip_levels_with_alpha(
                        &texture.rgba,
                        texture.width,
                        texture.height,
                        false,
                        false,
                        false,
                    )
                })
                .unwrap_or_else(|| white_mips.to_vec());
            let (occlusion_width, occlusion_height) = occlusion
                .map(|texture| (texture.width, texture.height))
                .unwrap_or((1, 1));

            let label = format!(
                "{} - {}",
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("OBJ"),
                material.name
            );
            let shininess = material.shininess.clamp(0.0, 1000.0);
            self.materials.push(Material::from_precomputed_mipmaps(
                &self.device,
                &self.queue,
                &self.texture_bind_group_layout,
                (&diffuse_mips, diffuse_width, diffuse_height),
                (&white_mips, 1, 1),
                (&normal_mips, normal_width, normal_height),
                (&occlusion_mips, occlusion_width, occlusion_height),
                (&white_mips, 1, 1),
                (&white_mips, 1, 1),
                (&white_mips, 1, 1),
                (&white_mips, 1, 1),
                (&white_mips, 1, 1),
                (&white_mips, 1, 1),
                (&white_mips, 1, 1),
                (&white_mips, 1, 1),
                MaterialParams {
                    base_color: [
                        material.diffuse[0].clamp(0.0, 1.0),
                        material.diffuse[1].clamp(0.0, 1.0),
                        material.diffuse[2].clamp(0.0, 1.0),
                        material.dissolve.clamp(0.0, 1.0),
                    ],
                    roughness: (2.0 / (shininess + 2.0)).sqrt().clamp(0.045, 1.0),
                    metallic: 0.0,
                    alpha_mode: u32::from(material.dissolve < 1.0) * 2,
                    normal_scale: if normal.is_some() { 1.0 } else { 0.0 },
                    specular_factor: (material.specular.iter().sum::<f32>() / 3.0).clamp(0.0, 1.0),
                    ..Default::default()
                },
                Some(&label),
            ));
        }

        let mut instances = Vec::with_capacity(model.primitives.len());
        for primitive in model.primitives {
            let mesh_id = MeshId(self.meshes.len());
            let label = if primitive.name.is_empty() {
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("OBJ")
                    .to_owned()
            } else {
                format!(
                    "{} - {}",
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("OBJ"),
                    primitive.name
                )
            };
            self.meshes.push(Mesh::from_mesh_data(
                &self.device,
                &primitive.mesh,
                Some(&label),
            ));
            instances.push(LoadedObjInstance {
                name: label,
                mesh_id,
                material_id: primitive
                    .material_index
                    .map(|index| MaterialId(first_material_id + index))
                    .unwrap_or(MATERIAL_DEFAULT),
            });
        }

        Ok(LoadedObjModel { instances })
    }

    fn upload_gltf(
        &mut self,
        mut model: GltfModelData,
        path: &Path,
    ) -> Result<LoadedGltfModel, String> {
        let limits = self.device.limits();
        model
            .limit_texture_dimensions(limits.max_texture_dimension_2d)
            .map_err(|error| format!("{}: {error}", path.display()))?;
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
                || material
                    .clearcoat_image
                    .is_some_and(|image| image >= model.images.len())
                || material
                    .clearcoat_roughness_image
                    .is_some_and(|image| image >= model.images.len())
                || material
                    .clearcoat_normal_image
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
            let clearcoat = material_data
                .clearcoat_image
                .map(|image| &model.images[image]);
            let (clearcoat_mips, clearcoat_width, clearcoat_height) =
                clearcoat.map_or((&default_texture_mip_levels[..], 1, 1), |image| {
                    (
                        image
                            .data_mip_levels
                            .as_deref()
                            .unwrap_or(&default_texture_mip_levels),
                        image.width,
                        image.height,
                    )
                });
            let clearcoat_roughness = material_data
                .clearcoat_roughness_image
                .map(|image| &model.images[image]);
            let (clearcoat_roughness_mips, clearcoat_roughness_width, clearcoat_roughness_height) =
                clearcoat_roughness.map_or((&default_texture_mip_levels[..], 1, 1), |image| {
                    (
                        image
                            .data_mip_levels
                            .as_deref()
                            .unwrap_or(&default_texture_mip_levels),
                        image.width,
                        image.height,
                    )
                });
            let clearcoat_normal = material_data
                .clearcoat_normal_image
                .map(|image| &model.images[image]);
            let (clearcoat_normal_mips, clearcoat_normal_width, clearcoat_normal_height) =
                clearcoat_normal.map_or((&flat_normal_mip_levels[..], 1, 1), |image| {
                    (
                        image
                            .normal_mip_levels
                            .as_deref()
                            .unwrap_or(&flat_normal_mip_levels),
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
                (clearcoat_mips, clearcoat_width, clearcoat_height),
                (
                    clearcoat_roughness_mips,
                    clearcoat_roughness_width,
                    clearcoat_roughness_height,
                ),
                (
                    clearcoat_normal_mips,
                    clearcoat_normal_width,
                    clearcoat_normal_height,
                ),
                MaterialParams {
                    base_color: material_data.base_color_factor,
                    roughness: material_data.roughness_factor,
                    metallic: material_data.metallic_factor,
                    unlit: if material_data.unlit { 1.0 } else { 0.0 },
                    alpha_mode: material_data.alpha_mode,
                    alpha_cutoff: material_data.alpha_cutoff,
                    double_sided: u32::from(material_data.double_sided),
                    transmission_factor: material_data.transmission_factor,
                    clearcoat_factor: material_data.clearcoat_factor,
                    clearcoat_roughness_factor: material_data.clearcoat_roughness_factor,
                    clearcoat_normal_scale: if material_data.clearcoat_normal_image.is_some() {
                        material_data.clearcoat_normal_scale
                    } else {
                        0.0
                    },
                    clearcoat_tex_coord: material_data.clearcoat_tex_coord,
                    clearcoat_roughness_tex_coord: material_data.clearcoat_roughness_tex_coord,
                    clearcoat_normal_tex_coord: material_data.clearcoat_normal_tex_coord,
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
                    clearcoat_uv_transform: material_data.clearcoat_transform,
                    clearcoat_roughness_uv_transform: material_data.clearcoat_roughness_transform,
                    clearcoat_normal_uv_transform: material_data.clearcoat_normal_transform,
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
        if !self.surface_ready {
            return;
        }
        let gpu_timing_result = self
            .gpu_timestamp_profiler
            .as_mut()
            .map(|profiler| profiler.poll(&self.device));
        match gpu_timing_result {
            Some(Ok(Some(sample))) => {
                self.timings.gpu_frame_ms = Some(sample.frame_ms);
                self.timings.gpu_shadow_map_ms = Some(sample.shadow_map_ms);
                self.timings.gpu_opaque_pass_ms = Some(sample.opaque_pass_ms);
                self.timings.gpu_sample_id = self.timings.gpu_sample_id.wrapping_add(1);
            }
            Some(Ok(None)) => {}
            Some(Err(error)) => {
                eprintln!("GPU timestamp profiling disabled: {error}");
                self.gpu_timestamp_profiler = None;
                self.timings.gpu_frame_ms = None;
                self.timings.gpu_shadow_map_ms = None;
                self.timings.gpu_opaque_pass_ms = None;
            }
            None => {
                self.timings.gpu_frame_ms = None;
                self.timings.gpu_shadow_map_ms = None;
                self.timings.gpu_opaque_pass_ms = None;
            }
        }
        let ui_started = Instant::now();
        let previous_timings = self.timings;
        debug_ui.prepare_frame(
            camera,
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
        for object in objects {
            if !object.morph_weights().is_empty()
                && let Some(mesh) = self.meshes.get_mut(object.mesh_id.0)
            {
                mesh.update_morph_weights(&self.queue, object.morph_weights());
            }
        }
        let scene_started = Instant::now();
        let light_uniform = build_light_uniform(self.settings, world);
        self.queue
            .write_buffer(&self._light_buffer, 0, bytemuck::bytes_of(&light_uniform));
        let mut camera_uniform = CameraUniform::new();
        camera_uniform.update_view_proj(camera);
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera_uniform));

        for instances in self.batches.values_mut() {
            instances.clear();
        }
        for instances in self.shadow_batches.values_mut() {
            instances.clear();
        }
        self.shadow_bounds.clear();
        let mut has_skinned_shadow_object = false;
        self.transparent_objects.clear();
        self.skin_matrices.clear();
        self.skin_matrices.push(Mat4::IDENTITY);
        let view_projection = camera.build_view_projection_matrix();
        let camera_forward = Vec3::new(
            camera.yaw.cos() * camera.pitch.cos(),
            camera.pitch.sin(),
            camera.yaw.sin() * camera.pitch.cos(),
        )
        .normalize();
        for (object_index, obj) in objects.iter().enumerate() {
            let Some(object_id) = world.object_id_at(object_index) else {
                continue;
            };
            let model_matrix = world
                .group_model_matrix_for_id(object_id)
                .unwrap_or_else(|| obj.model_matrix());
            let mesh = self.meshes.get(obj.mesh_id.0);
            let bounds = mesh.map(|mesh| (mesh.bounds_center, mesh.bounds_radius));
            let camera_visible = object_in_view(
                bounds,
                model_matrix,
                !obj.skin_matrices().is_empty(),
                view_projection,
            );
            let material = self.materials.get(obj.material_id.0);
            let is_transparent = material.is_some_and(|material| {
                is_transparent_material(material.alpha_mode, material.params.transmission_factor)
            });
            let casts_shadow = material
                .is_some_and(|material| material_casts_shadow(material.params.transmission_factor));
            if !should_prepare_instance(camera_visible, casts_shadow) {
                continue;
            }
            // The built-in floor receives shadows but should not set the shadow-map footprint.
            if casts_shadow && obj.mesh_id != MESH_PLANE {
                if !obj.skin_matrices().is_empty() {
                    has_skinned_shadow_object = true;
                } else if let Some((bounds_center, bounds_radius)) = bounds {
                    let conservative_scale = (model_matrix.x_axis.truncate().length_squared()
                        + model_matrix.y_axis.truncate().length_squared()
                        + model_matrix.z_axis.truncate().length_squared())
                    .sqrt();
                    self.shadow_bounds.push((
                        model_matrix.transform_point3(bounds_center),
                        bounds_radius * conservative_scale,
                    ));
                }
            }
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
            if casts_shadow {
                self.shadow_batches
                    .entry((obj.mesh_id, obj.material_id))
                    .or_default()
                    .push(instance);
            }
            if !camera_visible {
                continue;
            }
            if is_transparent {
                let local_center = mesh.map_or(Vec3::ZERO, |mesh| mesh.bounds_center);
                let world_center = model_matrix.transform_point3(local_center);
                let view_depth = transparent_depth(world_center, camera.eye, camera_forward);
                self.transparent_objects.push((
                    view_depth,
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
        sort_transparent_objects(&mut self.transparent_objects);

        self.all_instances.clear();
        self.shadow_instances.clear();
        self.draw_batches.clear();
        self.shadow_draw_batches.clear();

        let mut shadow_batches: Vec<_> = self.shadow_batches.iter().collect();
        shadow_batches.sort_by_key(|((mesh_id, material_id), _)| (mesh_id.0, material_id.0));
        for (&(mesh_id, material_id), instances) in shadow_batches {
            if instances.is_empty() {
                continue;
            }
            let start = self.shadow_instances.len() as u32;
            let count = instances.len() as u32;
            self.shadow_instances.extend_from_slice(instances);
            self.shadow_draw_batches
                .push((mesh_id, material_id, start..start + count));
        }

        let mut opaque_batches: Vec<_> = self.batches.iter().collect();
        opaque_batches.sort_by_key(|((mesh_id, material_id), _)| (mesh_id.0, material_id.0));
        for (&(mesh_id, material_id), instances) in opaque_batches {
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

        let visible_instance_count = self.all_instances.len();

        self.active_mesh_ids.clear();
        self.active_material_ids.clear();
        for (mesh_id, material_id, _, _) in &self.draw_batches {
            self.active_mesh_ids.insert(*mesh_id);
            self.active_material_ids.insert(*material_id);
        }
        let triangle_count = self
            .draw_batches
            .iter()
            .filter_map(|(mesh_id, _, range, _)| {
                self.meshes
                    .get(mesh_id.0)
                    .map(|mesh| mesh.num_indices as usize / 3 * range.len())
            })
            .sum();
        let opaque_draw_calls = self
            .draw_batches
            .iter()
            .filter(|(mesh_id, material_id, _, transparent)| {
                !transparent
                    && self.meshes.get(mesh_id.0).is_some()
                    && self.materials.get(material_id.0).is_some()
            })
            .count();
        let transparent_draw_calls = self
            .draw_batches
            .iter()
            .filter(|(mesh_id, material_id, _, transparent)| {
                *transparent
                    && self.meshes.get(mesh_id.0).is_some()
                    && self.materials.get(material_id.0).is_some_and(|material| {
                        !uses_glass_pipeline(material.params.transmission_factor)
                    })
            })
            .count();
        let glass_draw_calls = self
            .draw_batches
            .iter()
            .filter(|(mesh_id, material_id, _, transparent)| {
                *transparent
                    && self.meshes.get(mesh_id.0).is_some()
                    && self.materials.get(material_id.0).is_some_and(|material| {
                        uses_glass_pipeline(material.params.transmission_factor)
                    })
            })
            .count();
        self.timings.draw_calls = 2 + opaque_draw_calls + transparent_draw_calls + glass_draw_calls;
        self.timings.triangle_count = triangle_count;
        self.timings.active_meshes = self.active_mesh_ids.len();
        self.timings.active_materials = self.active_material_ids.len();
        self.timings.render_batches = self.draw_batches.len();
        self.timings.active_instances = visible_instance_count;

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

        let key_direction = self.settings.key_light_direction();
        let shadow_matrix = if has_skinned_shadow_object {
            directional_shadow_matrix(key_direction)
        } else {
            directional_shadow_matrix_for_bounds(key_direction, &self.shadow_bounds)
        };
        let render_shadows = self.settings.shadows_enabled
            && (!self.shadow_cache_valid
                || self.cached_shadow_matrix != shadow_matrix
                || self.cached_shadow_instances != self.shadow_instances
                || self.cached_shadow_skin_matrices != self.skin_matrices
                || self.shadow_draw_batches != self.cached_shadow_batches);
        if render_shadows {
            if self.shadow_instances.len() > self.shadow_instance_capacity {
                self.shadow_instance_capacity = self
                    .shadow_instances
                    .len()
                    .checked_next_power_of_two()
                    .unwrap_or(self.shadow_instances.len());
                self.shadow_instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("shadow instance buffer"),
                    size: (self.shadow_instance_capacity * std::mem::size_of::<InstanceRaw>())
                        as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            if !self.shadow_instances.is_empty() {
                self.queue.write_buffer(
                    &self.shadow_instance_buffer,
                    0,
                    bytemuck::cast_slice(&self.shadow_instances),
                );
            }
        }

        self.timings.scene_cpu_ms = scene_started.elapsed().as_secs_f32() * 1000.0;
        let acquire_started = Instant::now();
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(
                wgpu::SurfaceError::Lost
                | wgpu::SurfaceError::Outdated
                | wgpu::SurfaceError::Timeout,
            ) => {
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
        let capture_gpu_frame = self
            .gpu_timestamp_profiler
            .as_ref()
            .is_some_and(|profiler| profiler.write_start(&mut encoder));

        if capture_gpu_frame {
            if let Some(profiler) = self.gpu_timestamp_profiler.as_ref() {
                profiler.write_timestamp(&mut encoder, GPU_TIMESTAMP_SHADOW_START);
            }
        }
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
            pass.set_vertex_buffer(1, self.shadow_instance_buffer.slice(..));
            self.timings.draw_calls += self
                .shadow_draw_batches
                .iter()
                .filter(|(mesh_id, material_id, _)| {
                    self.meshes.get(mesh_id.0).is_some()
                        && self.materials.get(material_id.0).is_some()
                })
                .count();

            for (mesh_id, material_id, range) in &self.shadow_draw_batches {
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
                .extend_from_slice(&self.shadow_instances);
            self.cached_shadow_skin_matrices
                .clone_from(&self.skin_matrices);
            self.cached_shadow_batches.clear();
            self.cached_shadow_batches
                .clone_from(&self.shadow_draw_batches);
            self.cached_shadow_matrix = shadow_matrix;
            self.shadow_cache_valid = true;
        }

        if capture_gpu_frame {
            if let Some(profiler) = self.gpu_timestamp_profiler.as_ref() {
                profiler.write_timestamp(&mut encoder, GPU_TIMESTAMP_SHADOW_END);
                profiler.write_timestamp(&mut encoder, GPU_TIMESTAMP_OPAQUE_START);
            }
        }
        let multisampled = self.settings.msaa_samples > 1;
        {
            let (color_view, resolve_target) = if multisampled {
                (
                    self.msaa_view
                        .as_ref()
                        .expect("MSAA color target exists for multisampled rendering"),
                    Some(&self.scene_color_view),
                )
            } else {
                (&self.scene_color_view, None)
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("opaque scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target,
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

        if capture_gpu_frame {
            if let Some(profiler) = self.gpu_timestamp_profiler.as_ref() {
                profiler.write_timestamp(&mut encoder, GPU_TIMESTAMP_OPAQUE_END);
            }
        }
        if !multisampled {
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self._scene_color_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self._hdr_output_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: scaled_render_dimension(self.config.width, self.settings.render_scale),
                    height: scaled_render_dimension(self.config.height, self.settings.render_scale),
                    depth_or_array_layers: 1,
                },
            );
        }

        {
            let (color_view, resolve_target) = if multisampled {
                (
                    self.msaa_view
                        .as_ref()
                        .expect("MSAA color target exists for multisampled rendering"),
                    Some(&self.hdr_output_view),
                )
            } else {
                (&self.hdr_output_view, None)
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("transparent scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target,
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
                        if uses_glass_pipeline(material.params.transmission_factor) {
                            continue;
                        }
                        pass.set_pipeline(&self.transparent_pipeline);
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

        let has_glass = self
            .draw_batches
            .iter()
            .any(|(_, material_id, _, transparent)| {
                *transparent
                    && self.materials.get(material_id.0).is_some_and(|material| {
                        uses_glass_pipeline(material.params.transmission_factor)
                    })
            });
        if has_glass {
            // Glass must sample the opaque scene plus alpha-blended cabin geometry.
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self._hdr_output_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self._scene_color_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: scaled_render_dimension(self.config.width, self.settings.render_scale),
                    height: scaled_render_dimension(self.config.height, self.settings.render_scale),
                    depth_or_array_layers: 1,
                },
            );

            let (color_view, resolve_target) = if multisampled {
                (
                    self.msaa_view
                        .as_ref()
                        .expect("MSAA color target exists for multisampled rendering"),
                    Some(&self.hdr_output_view),
                )
            } else {
                (&self.hdr_output_view, None)
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glass refraction pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target,
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
            pass.set_bind_group(3, &self.scene_color_bind_group, &[]);
            pass.set_pipeline(&self.glass_pipeline);
            pass.set_vertex_buffer(1, self.instance_buffer.slice(..));

            for (mesh_id, material_id, range, transparent) in &self.draw_batches {
                if !transparent {
                    continue;
                }
                let (Some(mesh), Some(material)) = (
                    self.meshes.get(mesh_id.0),
                    self.materials.get(material_id.0),
                ) else {
                    continue;
                };
                if !uses_glass_pipeline(material.params.transmission_factor) {
                    continue;
                }
                pass.set_bind_group(1, &material.bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_vertex_buffer(2, mesh.skin_buffer.slice(..));
                pass.set_vertex_buffer(3, mesh.tangent_buffer.slice(..));
                pass.set_vertex_buffer(4, mesh.uv_sets_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.num_indices, 0, range.clone());
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
        if capture_gpu_frame {
            if let Some(profiler) = self.gpu_timestamp_profiler.as_ref() {
                profiler.write_end_and_resolve(&mut encoder);
            }
        }
        command_buffers.push(encoder.finish());
        self.queue.submit(command_buffers);
        if capture_gpu_frame {
            if let Some(profiler) = self.gpu_timestamp_profiler.as_mut() {
                profiler.map_after_submit();
            }
        }
        self.timings.encode_submit_cpu_ms = encode_started.elapsed().as_secs_f32() * 1000.0;
        frame.present();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_timestamps_require_encoder_timestamp_support() {
        let timestamp_features =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;

        assert_eq!(
            gpu_timestamp_features(timestamp_features),
            timestamp_features
        );
        assert!(gpu_timestamp_features(wgpu::Features::TIMESTAMP_QUERY).is_empty());
        assert!(gpu_timestamp_features(wgpu::Features::empty()).is_empty());
    }

    #[test]
    fn gpu_timestamp_delta_converts_ticks_to_milliseconds() {
        assert_eq!(timestamp_delta_ms(100, 1_100, 1_000.0), Some(1.0));
        assert_eq!(timestamp_delta_ms(1_100, 100, 1_000.0), None);
        assert_eq!(timestamp_delta_ms(0, 1, 0.0), None);
    }

    #[test]
    fn gpu_timestamp_sample_separates_shadow_and_opaque_passes() {
        let sample = gpu_timing_sample([100, 200, 300, 300, 700, 1_100], 1_000.0).unwrap();

        assert_eq!(
            sample,
            GpuTimingSample {
                frame_ms: 1.0,
                shadow_map_ms: 0.1,
                opaque_pass_ms: 0.4,
            }
        );
    }

    #[test]
    fn transparent_sort_uses_camera_depth_instead_of_radial_distance() {
        let camera_eye = Vec3::ZERO;
        let camera_forward = Vec3::X;
        let near_off_axis = Vec3::new(2.0, 100.0, 0.0);
        let far_on_axis = Vec3::new(10.0, 0.0, 0.0);
        let instance = InstanceRaw::from_model(Mat4::IDENTITY, 0, 0);
        let mut objects = vec![
            (
                transparent_depth(near_off_axis, camera_eye, camera_forward),
                0,
                MeshId(0),
                MaterialId(0),
                instance,
            ),
            (
                transparent_depth(far_on_axis, camera_eye, camera_forward),
                1,
                MeshId(1),
                MaterialId(1),
                instance,
            ),
        ];

        sort_transparent_objects(&mut objects);

        assert_eq!(objects[0].1, 1);
        assert_eq!(objects[1].1, 0);
    }

    #[test]
    fn offscreen_opaque_objects_are_still_prepared_for_shadow_pass() {
        assert!(should_prepare_instance(false, true));
        assert!(should_prepare_instance(true, false));
        assert!(!should_prepare_instance(false, false));
    }

    #[test]
    fn alpha_blended_opaque_materials_cast_cutout_shadows_but_transmission_does_not() {
        assert!(material_casts_shadow(0.0));
        assert!(!material_casts_shadow(0.5));
        assert!(is_transparent_material(2, 0.0));
        assert!(is_transparent_material(0, 0.5));
        assert!(uses_glass_pipeline(0.5));
        assert!(!uses_glass_pipeline(0.0));
    }

    #[test]
    fn light_uniform_matches_wgsl_layout() {
        assert_eq!(std::mem::size_of::<LocalLightUniform>(), 64);
        assert_eq!(std::mem::size_of::<LightUniform>(), 1136);
    }

    #[test]
    fn light_uniform_packs_enabled_point_and_spot_lights() {
        let point = LocalLight::point("Point", [1.0, 2.0, 3.0]);
        let spot = LocalLight::spot("Spot", [4.0, 5.0, 6.0], [0.0, -2.0, 0.0]);
        let mut disabled = LocalLight::point("Disabled", [0.0; 3]);
        disabled.enabled = false;
        let mut world = World::new();
        world.add_local_light(point).unwrap();
        world.add_local_light(spot).unwrap();
        world.add_local_light(disabled).unwrap();
        let uniform = build_light_uniform(RendererSettings::default(), &world);

        assert_eq!(uniform.local_light_info[0], 2);
        assert_eq!(uniform.local_lights[0].position_range, [1.0, 2.0, 3.0, 8.0]);
        assert_eq!(uniform.local_lights[0].direction_type[3], 0.0);
        assert_eq!(uniform.local_lights[1].position_range[..3], [4.0, 5.0, 6.0]);
        assert_eq!(
            uniform.local_lights[1].direction_type,
            [0.0, -1.0, 0.0, 1.0]
        );
        assert!(uniform.local_lights[1].spot_angles[0] > uniform.local_lights[1].spot_angles[1]);
    }

    #[test]
    fn render_scale_reduces_internal_resolution_and_keeps_small_sizes_valid() {
        assert_eq!(scaled_render_dimension(1680, 1.0), 1680);
        assert_eq!(scaled_render_dimension(1680, 0.75), 1260);
        assert_eq!(scaled_render_dimension(1, 0.5), 1);
    }

    #[test]
    fn msaa_modes_require_color_resolve_and_depth_support() {
        let hdr_flags = wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE
            | wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X2
            | wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4;
        let depth_flags = wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X2
            | wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4
            | wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X8;

        assert_eq!(
            supported_msaa_sample_counts_for_flags(hdr_flags, depth_flags),
            [1, 2, 4]
        );
        assert_eq!(
            supported_msaa_sample_counts_for_flags(
                wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4,
                depth_flags,
            ),
            [1]
        );
    }

    #[test]
    fn default_msaa_prefers_four_samples_and_never_exceeds_it() {
        assert_eq!(default_msaa_sample_count(&[1, 2, 4, 8]), 4);
        assert_eq!(default_msaa_sample_count(&[1, 2]), 2);
        assert_eq!(default_msaa_sample_count(&[1]), 1);
    }

    #[test]
    fn surface_format_selection_requires_a_capability() {
        let capabilities = wgpu::SurfaceCapabilities {
            formats: vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            present_modes: vec![wgpu::PresentMode::Fifo],
            alpha_modes: vec![wgpu::CompositeAlphaMode::Opaque],
            usages: wgpu::TextureUsages::RENDER_ATTACHMENT,
        };

        assert_eq!(
            select_surface_format(&capabilities).unwrap(),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
        assert_eq!(
            select_present_mode(&capabilities).unwrap(),
            wgpu::PresentMode::Fifo
        );
        assert_eq!(
            select_alpha_mode(&capabilities).unwrap(),
            wgpu::CompositeAlphaMode::Opaque
        );

        let empty = wgpu::SurfaceCapabilities {
            formats: vec![],
            present_modes: vec![],
            alpha_modes: vec![],
            usages: wgpu::TextureUsages::RENDER_ATTACHMENT,
        };
        assert!(select_surface_format(&empty).is_err());
        assert!(select_present_mode(&empty).is_err());
        assert!(select_alpha_mode(&empty).is_err());
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
    fn scene_shadow_camera_fits_bounds_outside_the_legacy_volume() {
        let ray_direction = Vec3::new(0.3, -1.0, 0.2).normalize();
        let bounds = [
            (Vec3::new(25.0, 4.0, -15.0), 2.0),
            (Vec3::new(-18.0, 1.0, 24.0), 1.5),
        ];
        let matrix = directional_shadow_matrix_for_bounds(ray_direction, &bounds);
        let up = if ray_direction.dot(Vec3::Y).abs() > 0.98 {
            Vec3::X
        } else {
            Vec3::Y
        };
        let light_orientation = crate::math::look_at_rh(Vec3::ZERO, ray_direction, up);
        let light_to_world = light_orientation.inverse();

        for (center, radius) in bounds {
            let light_center = light_orientation.transform_point3(center);
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                for sign in [-1.0, 1.0] {
                    let light_point = light_center + axis * (radius * sign);
                    let world_point = light_to_world.transform_point3(light_point);
                    let clip = matrix * world_point.extend(1.0);
                    let ndc = clip.truncate() / clip.w;

                    assert!((-1.0..=1.0).contains(&ndc.x));
                    assert!((-1.0..=1.0).contains(&ndc.y));
                    assert!((0.0..=1.0).contains(&ndc.z));
                }
            }
        }
    }

    #[test]
    fn scene_shadow_camera_falls_back_without_valid_bounds() {
        let ray_direction = RendererSettings::default().key_light_direction();

        assert_eq!(
            directional_shadow_matrix_for_bounds(ray_direction, &[]),
            directional_shadow_matrix(ray_direction)
        );
        assert_eq!(
            directional_shadow_matrix_for_bounds(
                ray_direction,
                &[(Vec3::splat(f32::NAN), f32::NAN)]
            ),
            directional_shadow_matrix(ray_direction)
        );
    }

    #[test]
    fn frustum_culling_keeps_large_scene_mesh_at_all_camera_yaws() {
        let view_distance = 20.0;
        let proj = crate::math::perspective_rh(
            45.0_f32.to_radians(),
            16.0 / 9.0,
            crate::config::Z_NEAR,
            crate::config::Z_FAR,
        );

        for step in 0..360 {
            let yaw = (step as f32).to_radians();
            let eye = Vec3::new(yaw.sin() * view_distance, 4.0, yaw.cos() * view_distance);
            let forward = Vec3::ZERO - eye;
            let view = crate::math::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
            let _ = forward;
            assert!(
                sphere_in_view(Vec3::ZERO, 12.0, proj * view),
                "large mesh was culled at yaw {step}"
            );
        }
    }

    #[test]
    fn frustum_culling_uses_wgpu_zero_to_one_depth_range() {
        let projection = crate::math::perspective_rh(
            45.0_f32.to_radians(),
            1.0,
            crate::config::Z_NEAR,
            crate::config::Z_FAR,
        );
        let view = crate::math::look_at_rh(Vec3::ZERO, Vec3::X, Vec3::Y);
        let view_projection = projection * view;

        assert!(!sphere_in_view(
            Vec3::new(0.05, 0.0, 0.0),
            0.01,
            view_projection
        ));
        assert!(sphere_in_view(
            Vec3::new(0.12, 0.0, 0.0),
            0.03,
            view_projection
        ));
        assert!(!sphere_in_view(
            Vec3::new(1001.0, 0.0, 0.0),
            0.1,
            view_projection
        ));
        assert!(sphere_in_view(
            Vec3::new(999.95, 0.0, 0.0),
            0.1,
            view_projection
        ));
    }

    #[test]
    fn skinned_meshes_are_not_culled_by_bind_pose_bounds() {
        let projection = crate::math::perspective_rh(
            45.0_f32.to_radians(),
            1.0,
            crate::config::Z_NEAR,
            crate::config::Z_FAR,
        );
        let view = crate::math::look_at_rh(Vec3::ZERO, Vec3::X, Vec3::Y);
        let bounds = Some((Vec3::new(0.05, 0.0, 0.0), 0.01));

        assert!(!object_in_view(
            bounds,
            Mat4::IDENTITY,
            false,
            projection * view
        ));
        assert!(object_in_view(
            bounds,
            Mat4::IDENTITY,
            true,
            projection * view
        ));
    }

    #[test]
    fn default_floor_bounds_remain_visible_while_camera_rotates() {
        let projection = crate::math::perspective_rh(
            45.0_f32.to_radians(),
            16.0 / 9.0,
            crate::config::Z_NEAR,
            crate::config::Z_FAR,
        );
        let eye = Vec3::new(0.0, 2.0, 6.0);
        for step in 0..360 {
            let yaw = (step as f32).to_radians();
            let forward = Vec3::new(yaw.cos(), -0.2, yaw.sin()).normalize();
            let view = crate::math::look_at_rh(eye, eye + forward, Vec3::Y);
            assert!(
                sphere_in_view(Vec3::new(0.0, -1.0, 0.0), 15.0, projection * view),
                "default floor bounds were culled at yaw {step}"
            );
        }
    }

    #[test]
    fn glass_sphere_bounds_remain_visible_when_camera_tracks_it() {
        let projection = crate::math::perspective_rh(
            45.0_f32.to_radians(),
            16.0 / 9.0,
            crate::config::Z_NEAR,
            crate::config::Z_FAR,
        );
        let sphere_center = Vec3::new(2.0, -0.32, 0.0);
        for step in 0..360 {
            let angle = (step as f32).to_radians();
            let eye = sphere_center + Vec3::new(angle.cos(), 0.0, angle.sin()) * 6.0;
            let view = crate::math::look_at_rh(eye, sphere_center, Vec3::Y);
            assert!(
                sphere_in_view(sphere_center, 0.68, projection * view),
                "glass sphere was culled at camera angle {step}"
            );
        }
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
