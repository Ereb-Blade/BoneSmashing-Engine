#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MaterialParams {
    pub base_color: [f32; 4], // Множитель цвета (тонирование)
    pub roughness: f32,       // Шероховатость (чем меньше, тем резче блик)
    pub metallic: f32,        // Металличность / интенсивность блика
    pub unlit: f32,
    pub alpha_mode: u32,
    pub alpha_cutoff: f32,
    pub normal_scale: f32,
    pub transmission_factor: f32,
    pub ior: f32,
    pub specular_factor: f32,
    pub double_sided: u32,
    pub occlusion_strength: f32,
    pub emissive_strength: f32,
    pub specular_color_factor: [f32; 3],
    pub base_color_tex_coord: u32,
    pub metallic_roughness_tex_coord: u32,
    pub normal_tex_coord: u32,
    pub occlusion_tex_coord: u32,
    pub emissive_tex_coord: u32,
    pub emissive_factor: [f32; 3],
    pub specular_tex_coord: u32,
    pub specular_color_tex_coord: u32,
    pub transmission_tex_coord: u32,
    pub pbr_workflow: u32,
    pub _pad: u32,
    pub thickness_factor: f32,
    pub attenuation_distance: f32,
    pub thickness_tex_coord: u32,
    pub _volume_pad0: u32,
    pub attenuation_color: [f32; 3],
    pub _volume_pad1: u32,
    pub thickness_uv_transform: [[f32; 4]; 2],
    pub base_color_uv_transform: [[f32; 4]; 2],
    pub metallic_roughness_uv_transform: [[f32; 4]; 2],
    pub normal_uv_transform: [[f32; 4]; 2],
    pub occlusion_uv_transform: [[f32; 4]; 2],
    pub emissive_uv_transform: [[f32; 4]; 2],
    pub specular_uv_transform: [[f32; 4]; 2],
    pub specular_color_uv_transform: [[f32; 4]; 2],
    pub transmission_uv_transform: [[f32; 4]; 2],
}

impl Default for MaterialParams {
    fn default() -> Self {
        Self {
            base_color: [1.0, 1.0, 1.0, 1.0],
            roughness: 0.5,
            metallic: 0.0,
            unlit: 0.0,
            alpha_mode: 0,
            alpha_cutoff: 0.5,
            normal_scale: 0.0,
            transmission_factor: 0.0,
            ior: 1.5,
            specular_factor: 1.0,
            double_sided: 0,
            occlusion_strength: 1.0,
            emissive_strength: 1.0,
            specular_color_factor: [1.0; 3],
            base_color_tex_coord: 0,
            metallic_roughness_tex_coord: 0,
            normal_tex_coord: 0,
            occlusion_tex_coord: 0,
            emissive_tex_coord: 0,
            emissive_factor: [0.0; 3],
            specular_tex_coord: 0,
            specular_color_tex_coord: 0,
            transmission_tex_coord: 0,
            pbr_workflow: 0,
            _pad: 0,
            thickness_factor: 0.0,
            attenuation_distance: 0.0,
            thickness_tex_coord: 0,
            _volume_pad0: 0,
            attenuation_color: [1.0; 3],
            _volume_pad1: 0,
            thickness_uv_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            base_color_uv_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            metallic_roughness_uv_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            normal_uv_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            occlusion_uv_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            emissive_uv_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            specular_uv_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            specular_color_uv_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            transmission_uv_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
        }
    }
}

pub struct Material {
    pub bind_group: wgpu::BindGroup,
    pub _params_buffer: wgpu::Buffer,
    pub alpha_mode: u32,
    pub params: MaterialParams,
    pub label: String,
}

#[cfg(test)]
pub(crate) fn generate_mip_levels(
    rgba: &[u8],
    width: u32,
    height: u32,
    is_srgb: bool,
    is_normal_map: bool,
) -> Vec<Vec<u8>> {
    generate_mip_levels_with_alpha(rgba, width, height, is_srgb, is_normal_map, true)
}

pub(crate) fn generate_mip_levels_with_alpha(
    rgba: &[u8],
    width: u32,
    height: u32,
    is_srgb: bool,
    is_normal_map: bool,
    alpha_weighted: bool,
) -> Vec<Vec<u8>> {
    let mut levels = vec![rgba.to_vec()];
    let (mut source_width, mut source_height) = (width, height);

    while source_width > 1 || source_height > 1 {
        let source = levels.last().expect("base mip level is always present");
        let target_width = (source_width / 2).max(1);
        let target_height = (source_height / 2).max(1);
        let mut target = vec![0; target_width as usize * target_height as usize * 4];

        for y in 0..target_height {
            for x in 0..target_width {
                let mut alpha_sum = 0.0;
                let mut color_sum = [0.0; 3];
                let source_x_start = x as f32 * source_width as f32 / target_width as f32;
                let source_x_end = (x + 1) as f32 * source_width as f32 / target_width as f32;
                let source_y_start = y as f32 * source_height as f32 / target_height as f32;
                let source_y_end = (y + 1) as f32 * source_height as f32 / target_height as f32;
                let mut sample_weight = 0.0;

                for source_y in source_y_start.floor() as u32..source_y_end.ceil() as u32 {
                    for source_x in source_x_start.floor() as u32..source_x_end.ceil() as u32 {
                        let overlap_x = source_x_end.min(source_x as f32 + 1.0)
                            - source_x_start.max(source_x as f32);
                        let overlap_y = source_y_end.min(source_y as f32 + 1.0)
                            - source_y_start.max(source_y as f32);
                        let weight = overlap_x * overlap_y;
                        let source_index = ((source_y * source_width + source_x) * 4) as usize;
                        let alpha = source[source_index + 3] as f32 / 255.0;
                        alpha_sum += alpha * weight;
                        for channel in 0..3 {
                            let encoded = source[source_index + channel] as f32 / 255.0;
                            let linear = if is_srgb {
                                srgb_to_linear(encoded)
                            } else {
                                encoded
                            };
                            color_sum[channel] +=
                                linear * if alpha_weighted { alpha } else { 1.0 } * weight;
                        }
                        sample_weight += weight;
                    }
                }

                let alpha = alpha_sum / sample_weight;
                let target_index = ((y * target_width + x) * 4) as usize;
                let mut linear_color = [0.0; 3];
                for channel in 0..3 {
                    let color_weight = if alpha_weighted {
                        alpha_sum
                    } else {
                        sample_weight
                    };
                    let linear = if color_weight > f32::EPSILON {
                        color_sum[channel] / color_weight
                    } else {
                        0.0
                    };
                    linear_color[channel] = linear;
                }
                if is_normal_map {
                    let normal = glam::Vec3::new(
                        linear_color[0] * 2.0 - 1.0,
                        linear_color[1] * 2.0 - 1.0,
                        linear_color[2] * 2.0 - 1.0,
                    )
                    .normalize_or_zero();
                    let normal = if normal.length_squared() > f32::EPSILON {
                        normal
                    } else {
                        glam::Vec3::Z
                    };
                    linear_color = (normal * 0.5 + glam::Vec3::splat(0.5)).to_array();
                }
                for channel in 0..3 {
                    let encoded = if is_srgb && !is_normal_map {
                        linear_to_srgb(linear_color[channel])
                    } else {
                        linear_color[channel]
                    };
                    target[target_index + channel] = (encoded * 255.0).round() as u8;
                }
                target[target_index + 3] = (alpha * 255.0).round() as u8;
            }
        }

        levels.push(target);
        source_width = target_width;
        source_height = target_height;
    }

    levels
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn apply_linear_color_gain(rgba: &mut [u8], gain: f32) {
    for pixel in rgba.chunks_exact_mut(4) {
        for channel in &mut pixel[..3] {
            let linear = srgb_to_linear(*channel as f32 / 255.0) * gain;
            *channel = (linear_to_srgb(linear.clamp(0.0, 1.0)) * 255.0).round() as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_uniform_matches_wgsl_uniform_alignment() {
        assert_eq!(std::mem::size_of::<MaterialParams>(), 448);
        assert_eq!(
            std::mem::offset_of!(MaterialParams, specular_color_factor),
            64
        );
        assert_eq!(std::mem::offset_of!(MaterialParams, emissive_factor), 96);
        assert_eq!(
            std::mem::offset_of!(MaterialParams, thickness_uv_transform),
            160
        );
        assert_eq!(
            std::mem::offset_of!(MaterialParams, base_color_uv_transform),
            192
        );
        assert_eq!(
            std::mem::offset_of!(MaterialParams, transmission_uv_transform),
            416
        );
    }

    #[test]
    fn mipmaps_preserve_color_across_transparent_edges() {
        let mipmaps = generate_mip_levels(&[255, 0, 0, 255, 0, 0, 0, 0], 2, 1, true, false);

        assert_eq!(mipmaps.len(), 2);
        assert_eq!(mipmaps[1], [255, 0, 0, 128]);
    }

    #[test]
    fn data_texture_mipmaps_ignore_alpha_when_filtering_color_channels() {
        let mipmaps = generate_mip_levels_with_alpha(
            &[0, 0, 0, 0, 255, 0, 0, 255],
            2,
            1,
            false,
            false,
            false,
        );

        assert_eq!(mipmaps[1], [128, 0, 0, 128]);
    }

    #[test]
    fn mipmaps_reach_one_by_one_for_non_power_of_two_textures() {
        let mipmaps = generate_mip_levels(&[255; 3 * 5 * 4], 3, 5, false, false);

        assert_eq!(mipmaps.len(), 3);
        assert_eq!(mipmaps[1].len(), 1 * 2 * 4);
        assert_eq!(mipmaps[2].len(), 4);
    }

    #[test]
    fn normal_map_mipmaps_renormalize_filtered_normals() {
        let mipmaps =
            generate_mip_levels(&[255, 128, 255, 255, 0, 128, 255, 255], 2, 1, false, true);
        let normal = glam::Vec3::new(
            mipmaps[1][0] as f32 / 127.5 - 1.0,
            mipmaps[1][1] as f32 / 127.5 - 1.0,
            mipmaps[1][2] as f32 / 127.5 - 1.0,
        );

        assert!(normal.z > 0.99);
        assert!(normal.length() > 0.99);
    }

    #[test]
    fn linear_base_color_gain_brightens_color_without_changing_alpha() {
        let rgba = [64, 96, 128, 173];
        let mut gained = rgba;
        apply_linear_color_gain(&mut gained, 2.0);

        assert!(gained[0] > rgba[0]);
        assert!(gained[1] > rgba[1]);
        assert!(gained[2] > rgba[2]);
        assert_eq!(gained[3], rgba[3]);
    }
}

impl Material {
    pub fn from_rgba(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        rgba: &[u8],
        width: u32,
        height: u32,
        params: MaterialParams,
        label: Option<&str>,
    ) -> Self {
        Self::from_rgba_with_metallic_roughness(
            device,
            queue,
            layout,
            rgba,
            width,
            height,
            &[255, 255, 255, 255],
            1,
            1,
            params,
            label,
        )
    }

    pub fn from_rgba_with_metallic_roughness(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        base_color_rgba: &[u8],
        base_color_width: u32,
        base_color_height: u32,
        metallic_roughness_rgba: &[u8],
        metallic_roughness_width: u32,
        metallic_roughness_height: u32,
        params: MaterialParams,
        label: Option<&str>,
    ) -> Self {
        Self::from_rgba_with_metallic_roughness_and_normal(
            device,
            queue,
            layout,
            base_color_rgba,
            base_color_width,
            base_color_height,
            metallic_roughness_rgba,
            metallic_roughness_width,
            metallic_roughness_height,
            None,
            params,
            label,
        )
    }

    pub fn from_rgba_with_metallic_roughness_and_normal(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        base_color_rgba: &[u8],
        base_color_width: u32,
        base_color_height: u32,
        metallic_roughness_rgba: &[u8],
        metallic_roughness_width: u32,
        metallic_roughness_height: u32,
        normal: Option<(&[u8], u32, u32)>,
        params: MaterialParams,
        label: Option<&str>,
    ) -> Self {
        let base_color_texture = Self::create_rgba_texture(
            device,
            queue,
            base_color_rgba,
            base_color_width,
            base_color_height,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            label,
            false,
            true,
        );
        let metallic_roughness_texture = Self::create_rgba_texture(
            device,
            queue,
            metallic_roughness_rgba,
            metallic_roughness_width,
            metallic_roughness_height,
            wgpu::TextureFormat::Rgba8Unorm,
            label
                .map(|name| format!("{name}_metallic_roughness"))
                .as_deref(),
            false,
            false,
        );
        let flat_normal = [128, 128, 255, 255];
        let (normal_rgba, normal_width, normal_height) = normal.unwrap_or((&flat_normal, 1, 1));
        let normal_texture = Self::create_rgba_texture(
            device,
            queue,
            normal_rgba,
            normal_width,
            normal_height,
            wgpu::TextureFormat::Rgba8Unorm,
            label.map(|name| format!("{name}_normal")).as_deref(),
            true,
            false,
        );
        let white = [255, 255, 255, 255];
        let occlusion_texture = Self::create_rgba_texture(
            device,
            queue,
            &white,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            label,
            false,
            false,
        );
        let emissive_texture = Self::create_rgba_texture(
            device,
            queue,
            &white,
            1,
            1,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            label,
            false,
            false,
        );
        let specular_texture = Self::create_rgba_texture(
            device,
            queue,
            &white,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            label,
            false,
            false,
        );
        let specular_color_texture = Self::create_rgba_texture(
            device,
            queue,
            &white,
            1,
            1,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            label,
            false,
            false,
        );
        let transmission_texture = Self::create_rgba_texture(
            device,
            queue,
            &white,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            label,
            false,
            false,
        );
        let thickness_texture = Self::create_rgba_texture(
            device,
            queue,
            &white,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            label,
            false,
            false,
        );
        Self::from_textures(
            device,
            layout,
            base_color_texture,
            metallic_roughness_texture,
            normal_texture,
            occlusion_texture,
            emissive_texture,
            specular_texture,
            specular_color_texture,
            transmission_texture,
            thickness_texture,
            params,
            label,
        )
    }

    pub(crate) fn from_precomputed_mipmaps(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        base_color: (&[Vec<u8>], u32, u32),
        metallic_roughness: (&[Vec<u8>], u32, u32),
        normal: (&[Vec<u8>], u32, u32),
        occlusion: (&[Vec<u8>], u32, u32),
        emissive: (&[Vec<u8>], u32, u32),
        specular: (&[Vec<u8>], u32, u32),
        specular_color: (&[Vec<u8>], u32, u32),
        transmission: (&[Vec<u8>], u32, u32),
        thickness: (&[Vec<u8>], u32, u32),
        params: MaterialParams,
        label: Option<&str>,
    ) -> Self {
        let base_color_texture = Self::create_texture_from_mip_levels(
            device,
            queue,
            base_color.0,
            base_color.1,
            base_color.2,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            label,
        );
        let metallic_roughness_texture = Self::create_texture_from_mip_levels(
            device,
            queue,
            metallic_roughness.0,
            metallic_roughness.1,
            metallic_roughness.2,
            wgpu::TextureFormat::Rgba8Unorm,
            label
                .map(|name| format!("{name}_metallic_roughness"))
                .as_deref(),
        );
        let normal_texture = Self::create_texture_from_mip_levels(
            device,
            queue,
            normal.0,
            normal.1,
            normal.2,
            wgpu::TextureFormat::Rgba8Unorm,
            label.map(|name| format!("{name}_normal")).as_deref(),
        );
        let occlusion_texture = Self::create_texture_from_mip_levels(
            device,
            queue,
            occlusion.0,
            occlusion.1,
            occlusion.2,
            wgpu::TextureFormat::Rgba8Unorm,
            label.map(|name| format!("{name}_occlusion")).as_deref(),
        );
        let emissive_texture = Self::create_texture_from_mip_levels(
            device,
            queue,
            emissive.0,
            emissive.1,
            emissive.2,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            label.map(|name| format!("{name}_emissive")).as_deref(),
        );
        let specular_texture = Self::create_texture_from_mip_levels(
            device,
            queue,
            specular.0,
            specular.1,
            specular.2,
            wgpu::TextureFormat::Rgba8Unorm,
            label.map(|name| format!("{name}_specular")).as_deref(),
        );
        let specular_color_texture = Self::create_texture_from_mip_levels(
            device,
            queue,
            specular_color.0,
            specular_color.1,
            specular_color.2,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            label
                .map(|name| format!("{name}_specular_color"))
                .as_deref(),
        );
        let transmission_texture = Self::create_texture_from_mip_levels(
            device,
            queue,
            transmission.0,
            transmission.1,
            transmission.2,
            wgpu::TextureFormat::Rgba8Unorm,
            label.map(|name| format!("{name}_transmission")).as_deref(),
        );
        let thickness_texture = Self::create_texture_from_mip_levels(
            device,
            queue,
            thickness.0,
            thickness.1,
            thickness.2,
            wgpu::TextureFormat::Rgba8Unorm,
            label.map(|name| format!("{name}_thickness")).as_deref(),
        );
        Self::from_textures(
            device,
            layout,
            base_color_texture,
            metallic_roughness_texture,
            normal_texture,
            occlusion_texture,
            emissive_texture,
            specular_texture,
            specular_color_texture,
            transmission_texture,
            thickness_texture,
            params,
            label,
        )
    }

    fn from_textures(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        base_color_texture: wgpu::Texture,
        metallic_roughness_texture: wgpu::Texture,
        normal_texture: wgpu::Texture,
        occlusion_texture: wgpu::Texture,
        emissive_texture: wgpu::Texture,
        specular_texture: wgpu::Texture,
        specular_color_texture: wgpu::Texture,
        transmission_texture: wgpu::Texture,
        thickness_texture: wgpu::Texture,
        params: MaterialParams,
        label: Option<&str>,
    ) -> Self {
        let base_color_view =
            base_color_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let metallic_roughness_view =
            metallic_roughness_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let normal_view = normal_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let occlusion_view = occlusion_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let emissive_view = emissive_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let specular_view = specular_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let specular_color_view =
            specular_color_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let transmission_view =
            transmission_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let thickness_view = thickness_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        use wgpu::util::DeviceExt;
        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: label.map(|name| format!("{name}_params_buffer")).as_deref(),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&base_color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&metallic_roughness_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&normal_view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&occlusion_view),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(&emissive_view),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: wgpu::BindingResource::TextureView(&specular_view),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: wgpu::BindingResource::TextureView(&specular_color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(&transmission_view),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::TextureView(&thickness_view),
                },
            ],
            label: label.map(|name| format!("{name}_bind_group")).as_deref(),
        });

        Self {
            bind_group,
            _params_buffer: params_buffer,
            alpha_mode: params.alpha_mode,
            params,
            label: label.unwrap_or("unnamed material").to_owned(),
        }
    }

    fn create_rgba_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rgba: &[u8],
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        label: Option<&str>,
        is_normal_map: bool,
        alpha_weighted: bool,
    ) -> wgpu::Texture {
        let is_srgb = format.is_srgb();
        let mip_levels = generate_mip_levels_with_alpha(
            rgba,
            width,
            height,
            is_srgb,
            is_normal_map,
            alpha_weighted,
        );
        Self::create_texture_from_mip_levels(
            device,
            queue,
            &mip_levels,
            width,
            height,
            format,
            label,
        )
    }

    fn create_texture_from_mip_levels(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mip_levels: &[Vec<u8>],
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        label: Option<&str>,
    ) -> wgpu::Texture {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label,
            size,
            mip_level_count: mip_levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let mut mip_width = width;
        let mut mip_height = height;
        for (mip_level, pixels) in mip_levels.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: mip_level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * mip_width),
                    rows_per_image: Some(mip_height),
                },
                wgpu::Extent3d {
                    width: mip_width,
                    height: mip_height,
                    depth_or_array_layers: 1,
                },
            );
            mip_width = (mip_width / 2).max(1);
            mip_height = (mip_height / 2).max(1);
        }
        texture
    }

    pub fn from_image_bytes(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        bytes: &[u8],
        params: MaterialParams,
        label: Option<&str>,
    ) -> Result<Self, image::ImageError> {
        use image::GenericImageView;
        let img = image::load_from_memory(bytes)?;
        let rgba = img.to_rgba8();
        let (width, height) = img.dimensions();
        Ok(Self::from_rgba(
            device, queue, layout, &rgba, width, height, params, label,
        ))
    }

    pub(crate) fn from_image_bytes_with_linear_gain(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        bytes: &[u8],
        linear_gain: f32,
        params: MaterialParams,
        label: Option<&str>,
    ) -> Result<Self, String> {
        if !linear_gain.is_finite() || linear_gain < 0.0 {
            return Err("base-color gain must be finite and non-negative".to_owned());
        }
        use image::GenericImageView;
        let image = image::load_from_memory(bytes).map_err(|error| error.to_string())?;
        let (width, height) = image.dimensions();
        let mut rgba = image.to_rgba8();
        apply_linear_color_gain(&mut rgba, linear_gain);
        Ok(Self::from_rgba(
            device, queue, layout, &rgba, width, height, params, label,
        ))
    }

    pub fn solid_color(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        color: [u8; 4],
        params: MaterialParams,
        label: Option<&str>,
    ) -> Self {
        Self::from_rgba(device, queue, layout, &color, 1, 1, params, label)
    }
}
