use half::f16;
use std::path::Path;
use std::sync::OnceLock;

pub struct Environment {
    _texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    _prefiltered_texture: wgpu::Texture,
    pub prefiltered_view: wgpu::TextureView,
    _diffuse_texture: wgpu::Texture,
    pub diffuse_view: wgpu::TextureView,
    _brdf_lut_texture: wgpu::Texture,
    pub brdf_lut_view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    pub label: String,
    pub mip_count: u32,
}

impl Environment {
    pub fn fallback(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        fn create_pixel_texture(
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            label: &str,
            pixel: [u16; 4],
        ) -> (wgpu::Texture, wgpu::TextureView) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&pixel),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(8),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            (texture, view)
        }

        let neutral = [
            f16::from_f32(0.12).to_bits(),
            f16::from_f32(0.14).to_bits(),
            f16::from_f32(0.18).to_bits(),
            f16::ONE.to_bits(),
        ];
        let (texture, view) = create_pixel_texture(device, queue, "Fallback environment", neutral);
        let (prefiltered_texture, prefiltered_view) =
            create_pixel_texture(device, queue, "Fallback specular environment", neutral);
        let (diffuse_texture, diffuse_view) =
            create_pixel_texture(device, queue, "Fallback diffuse environment", neutral);
        let (brdf_lut_texture, brdf_lut_view) = create_pixel_texture(
            device,
            queue,
            "Fallback BRDF LUT",
            [f16::ONE.to_bits(), 0, 0, f16::ONE.to_bits()],
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("HDR environment sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            _texture: texture,
            view,
            _prefiltered_texture: prefiltered_texture,
            prefiltered_view,
            _diffuse_texture: diffuse_texture,
            diffuse_view,
            _brdf_lut_texture: brdf_lut_texture,
            brdf_lut_view,
            sampler,
            label: "Loading environment...".to_owned(),
            mip_count: 1,
        }
    }

    pub fn studio(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let (width, height) = (512, 256);
        let mut pixels = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            let v = (y as f32 + 0.5) / height as f32;
            let latitude = (0.5 - v) * std::f32::consts::PI;
            let sky = (latitude.sin().max(0.0) * 0.35 + 0.08).powf(1.2);
            for x in 0..width {
                let u = (x as f32 + 0.5) / width as f32;
                let background = [0.12 + sky * 0.48, 0.14 + sky * 0.55, 0.18 + sky * 0.68];
                let left_softbox = softbox(u, v, 0.27, 0.36, 0.055, 0.18, 2.2);
                let right_softbox = softbox(u, v, 0.72, 0.42, 0.045, 0.22, 1.5);
                let top_softbox = softbox(u, v, 0.5, 0.12, 0.12, 0.045, 1.0);
                let floor = ((v - 0.64).max(0.0) * 2.0).min(1.0);
                pixels.extend_from_slice(&[
                    background[0] * (1.0 - floor)
                        + 0.12 * floor
                        + left_softbox * 0.94
                        + right_softbox * 0.58
                        + top_softbox * 0.8,
                    background[1] * (1.0 - floor)
                        + 0.105 * floor
                        + left_softbox * 0.78
                        + right_softbox * 0.78
                        + top_softbox * 0.84,
                    background[2] * (1.0 - floor)
                        + 0.09 * floor
                        + left_softbox * 0.58
                        + right_softbox * 1.0
                        + top_softbox,
                ]);
            }
        }
        Self::from_rgb32f(
            device,
            queue,
            width,
            height,
            pixels,
            "Procedural studio HDR",
        )
    }

    pub fn load(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        path: &Path,
        max_dimension: u32,
    ) -> Result<Self, String> {
        let image = image::ImageReader::open(path)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .with_guessed_format()
            .map_err(|error| format!("{}: {error}", path.display()))?
            .decode()
            .map_err(|error| format!("{}: {error}", path.display()))?
            .into_rgb32f();
        let width = image.width();
        let height = image.height();
        if width == 0 || height == 0 {
            return Err(format!("{}: HDR image has no pixels", path.display()));
        }
        if width > max_dimension || height > max_dimension {
            return Err(format!(
                "{}: HDR dimensions {width}x{height} exceed the device limit of {max_dimension}",
                path.display()
            ));
        }
        let aspect_ratio = width as f32 / height as f32;
        if !(1.9..=2.1).contains(&aspect_ratio) {
            return Err(format!(
                "{}: expected a 2:1 equirectangular HDR environment, got {width}x{height}",
                path.display()
            ));
        }
        Ok(Self::from_rgb32f(
            device,
            queue,
            width,
            height,
            image.into_raw(),
            &path.display().to_string(),
        ))
    }

    fn from_rgb32f(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        rgb: Vec<f32>,
        label: &str,
    ) -> Self {
        let mut base = Vec::with_capacity((width * height * 4) as usize);
        for pixel in rgb.chunks_exact(3) {
            base.extend_from_slice(&[
                sanitize_hdr(pixel[0]),
                sanitize_hdr(pixel[1]),
                sanitize_hdr(pixel[2]),
                1.0,
            ]);
        }
        let irradiance = convolve_diffuse(&base, width, height);
        let mut levels = vec![base];
        let mut mip_width = width;
        let mut mip_height = height;
        while mip_width > 1 || mip_height > 1 {
            let source = levels.last().expect("base level is always present");
            let next_width = (mip_width / 2).max(1);
            let next_height = (mip_height / 2).max(1);
            let mut next = vec![0.0; (next_width * next_height * 4) as usize];
            for y in 0..next_height {
                for x in 0..next_width {
                    for channel in 0..3 {
                        let mut sum = 0.0;
                        let mut count = 0.0;
                        for offset_y in 0..2 {
                            for offset_x in 0..2 {
                                let source_x = (x * 2 + offset_x) % mip_width;
                                let source_y = (y * 2 + offset_y).min(mip_height - 1);
                                let source_index =
                                    ((source_y * mip_width + source_x) * 4 + channel) as usize;
                                sum += source[source_index];
                                count += 1.0;
                            }
                        }
                        next[((y * next_width + x) * 4 + channel) as usize] = sum / count;
                    }
                    next[((y * next_width + x) * 4 + 3) as usize] = 1.0;
                }
            }
            levels.push(next);
            mip_width = next_width;
            mip_height = next_height;
        }

        fn sanitize_hdr(value: f32) -> f32 {
            if value.is_finite() {
                value.clamp(0.0, 65_504.0)
            } else {
                0.0
            }
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("HDR environment equirectangular texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        mip_width = width;
        mip_height = height;
        for (mip, pixels) in levels.iter().enumerate() {
            let half_pixels: Vec<u16> = pixels
                .iter()
                .map(|value| f16::from_f32(*value).to_bits())
                .collect();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: mip as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&half_pixels),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(mip_width * 8),
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
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("HDR environment mip chain"),
            mip_level_count: Some(levels.len() as u32),
            ..Default::default()
        });
        let (prefiltered_width, prefiltered_height) = (512, 256);
        let prefiltered_levels = prefilter_specular(
            levels.first().expect("base level is always present"),
            width,
            height,
            prefiltered_width,
            prefiltered_height,
        );
        let prefiltered_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("GGX prefiltered HDR environment"),
            size: wgpu::Extent3d {
                width: prefiltered_width,
                height: prefiltered_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: prefiltered_levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        mip_width = prefiltered_width;
        mip_height = prefiltered_height;
        for (mip, pixels) in prefiltered_levels.iter().enumerate() {
            let half_pixels: Vec<u16> = pixels
                .iter()
                .map(|value| f16::from_f32(*value).to_bits())
                .collect();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &prefiltered_texture,
                    mip_level: mip as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&half_pixels),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(mip_width * 8),
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
        let prefiltered_view = prefiltered_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("GGX prefiltered environment mip chain"),
            mip_level_count: Some(prefiltered_levels.len() as u32),
            ..Default::default()
        });
        let diffuse_width = 64;
        let diffuse_height = 32;
        let diffuse_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("HDR diffuse irradiance"),
            size: wgpu::Extent3d {
                width: diffuse_width,
                height: diffuse_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let half_irradiance: Vec<u16> = irradiance
            .iter()
            .map(|value| f16::from_f32(*value).to_bits())
            .collect();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &diffuse_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&half_irradiance),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(diffuse_width * 8),
                rows_per_image: Some(diffuse_height),
            },
            wgpu::Extent3d {
                width: diffuse_width,
                height: diffuse_height,
                depth_or_array_layers: 1,
            },
        );
        let diffuse_view = diffuse_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let (brdf_lut_texture, brdf_lut_view) = create_brdf_lut(device, queue);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("HDR environment sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            _texture: texture,
            view,
            _prefiltered_texture: prefiltered_texture,
            prefiltered_view,
            _diffuse_texture: diffuse_texture,
            diffuse_view,
            _brdf_lut_texture: brdf_lut_texture,
            brdf_lut_view,
            sampler,
            label: label.to_owned(),
            mip_count: prefiltered_levels.len() as u32,
        }
    }
}

fn prefilter_specular(
    source: &[f32],
    width: u32,
    height: u32,
    output_width: u32,
    output_height: u32,
) -> Vec<Vec<f32>> {
    const SAMPLE_COUNT: u32 = 64;
    let mut levels = vec![resample_equirectangular(
        source,
        width,
        height,
        output_width,
        output_height,
    )];
    let mut mip_width = output_width;
    let mut mip_height = output_height;
    let mip_count = output_width.max(output_height).ilog2() + 1;
    for mip in 1..mip_count {
        mip_width = (mip_width / 2).max(1);
        mip_height = (mip_height / 2).max(1);
        let roughness = mip as f32 / (mip_count - 1) as f32;
        levels.push(prefilter_specular_level(
            levels.first().expect("base level is always present"),
            output_width,
            output_height,
            mip_width,
            mip_height,
            roughness,
            SAMPLE_COUNT,
        ));
    }
    levels
}

fn resample_equirectangular(
    source: &[f32],
    width: u32,
    height: u32,
    output_width: u32,
    output_height: u32,
) -> Vec<f32> {
    let mut output = vec![0.0; (output_width * output_height * 4) as usize];
    for y in 0..output_height {
        for x in 0..output_width {
            let color = sample_equirectangular(
                source,
                width,
                height,
                (x as f32 + 0.5) / output_width as f32,
                (y as f32 + 0.5) / output_height as f32,
            );
            let index = ((y * output_width + x) * 4) as usize;
            output[index..index + 3].copy_from_slice(&color);
            output[index + 3] = 1.0;
        }
    }
    output
}

fn prefilter_specular_level(
    source: &[f32],
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    roughness: f32,
    sample_count: u32,
) -> Vec<f32> {
    let mut output = vec![0.0; (output_width * output_height * 4) as usize];
    for y in 0..output_height {
        let v = (y as f32 + 0.5) / output_height as f32;
        let latitude = (0.5 - v) * std::f32::consts::PI;
        for x in 0..output_width {
            let u = (x as f32 + 0.5) / output_width as f32;
            let longitude = (u - 0.5) * 2.0 * std::f32::consts::PI;
            let normal = [
                latitude.cos() * longitude.cos(),
                latitude.sin(),
                latitude.cos() * longitude.sin(),
            ];
            let view = normal;
            let mut color = [0.0; 3];
            let mut total_weight = 0.0;
            for sample in 0..sample_count {
                let xi = [
                    (sample as f32 + 0.5) / sample_count as f32,
                    radical_inverse_vdc(sample),
                ];
                let half_vector = importance_sample_ggx(xi, roughness, normal);
                let view_dot_half = dot(view, half_vector).max(0.0);
                let light = normalize(subtract(scale(half_vector, 2.0 * view_dot_half), view));
                let normal_dot_light = dot(normal, light).max(0.0);
                if normal_dot_light > 0.0 {
                    let sample_u = light[2].atan2(light[0]) / (2.0 * std::f32::consts::PI) + 0.5;
                    let sample_v = light[1].clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
                    let sample_color = sample_equirectangular(
                        source,
                        source_width,
                        source_height,
                        sample_u,
                        sample_v,
                    );
                    for channel in 0..3 {
                        color[channel] += sample_color[channel] * normal_dot_light;
                    }
                    total_weight += normal_dot_light;
                }
            }
            let index = ((y * output_width + x) * 4) as usize;
            for channel in 0..3 {
                output[index + channel] = color[channel] / total_weight.max(1e-5);
            }
            output[index + 3] = 1.0;
        }
    }
    output
}

fn importance_sample_ggx(xi: [f32; 2], roughness: f32, normal: [f32; 3]) -> [f32; 3] {
    let alpha = roughness * roughness;
    let phi = 2.0 * std::f32::consts::PI * xi[0];
    let cos_theta = ((1.0 - xi[1]) / (1.0 + (alpha * alpha - 1.0) * xi[1])).sqrt();
    let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
    let helper = if normal[1].abs() < 0.999 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let tangent = normalize(cross(helper, normal));
    let bitangent = cross(normal, tangent);
    normalize(add(
        add(
            scale(tangent, phi.cos() * sin_theta),
            scale(bitangent, phi.sin() * sin_theta),
        ),
        scale(normal, cos_theta),
    ))
}

fn radical_inverse_vdc(bits: u32) -> f32 {
    bits.reverse_bits() as f32 * 2.328_306_4e-10
}

fn create_brdf_lut(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::Texture, wgpu::TextureView) {
    const SIZE: u32 = 128;
    const SAMPLE_COUNT: u32 = 128;
    static BRDF_LUT_PIXELS: OnceLock<Vec<u16>> = OnceLock::new();
    let half_pixels = BRDF_LUT_PIXELS.get_or_init(|| {
        let mut pixels = vec![0.0; (SIZE * SIZE * 4) as usize];
        for y in 0..SIZE {
            let roughness = (y as f32 + 0.5) / SIZE as f32;
            for x in 0..SIZE {
                let n_dot_v = ((x as f32 + 0.5) / SIZE as f32).max(0.001);
                let (scale, bias) = integrate_brdf(n_dot_v, roughness, SAMPLE_COUNT);
                let index = ((y * SIZE + x) * 4) as usize;
                pixels[index] = scale;
                pixels[index + 1] = bias;
                pixels[index + 3] = 1.0;
            }
        }
        pixels
            .iter()
            .map(|value| f16::from_f32(*value).to_bits())
            .collect()
    });
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("GGX BRDF integration LUT"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(half_pixels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIZE * 8),
            rows_per_image: Some(SIZE),
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn integrate_brdf(n_dot_v: f32, roughness: f32, sample_count: u32) -> (f32, f32) {
    let normal = [0.0, 0.0, 1.0];
    let view = [(1.0 - n_dot_v * n_dot_v).sqrt(), 0.0, n_dot_v];
    let mut scale_sum = 0.0;
    let mut bias_sum = 0.0;
    for sample in 0..sample_count {
        let xi = [
            (sample as f32 + 0.5) / sample_count as f32,
            radical_inverse_vdc(sample),
        ];
        let half_vector = importance_sample_ggx(xi, roughness, normal);
        let light = normalize(subtract(
            scale(half_vector, 2.0 * dot(view, half_vector)),
            view,
        ));
        let n_dot_l = light[2].max(0.0);
        let n_dot_h = half_vector[2].max(0.0);
        let v_dot_h = dot(view, half_vector).max(0.0);
        if n_dot_l > 0.0 && n_dot_h > 0.0 {
            let geometry = geometry_schlick_ggx_ibl(n_dot_v, roughness)
                * geometry_schlick_ggx_ibl(n_dot_l, roughness);
            let geometry_visibility = geometry * v_dot_h / (n_dot_h * n_dot_v).max(1e-5);
            let fresnel = (1.0 - v_dot_h).powi(5);
            scale_sum += (1.0 - fresnel) * geometry_visibility;
            bias_sum += fresnel * geometry_visibility;
        }
    }
    (
        scale_sum / sample_count as f32,
        bias_sum / sample_count as f32,
    )
}

fn geometry_schlick_ggx_ibl(n_dot_direction: f32, roughness: f32) -> f32 {
    let k = roughness * roughness * 0.5;
    n_dot_direction / (n_dot_direction * (1.0 - k) + k).max(1e-5)
}

fn convolve_diffuse(source: &[f32], width: u32, height: u32) -> Vec<f32> {
    const OUTPUT_WIDTH: u32 = 64;
    const OUTPUT_HEIGHT: u32 = 32;
    const SAMPLE_COUNT: u32 = 64;
    let mut irradiance = vec![0.0; (OUTPUT_WIDTH * OUTPUT_HEIGHT * 4) as usize];
    for y in 0..OUTPUT_HEIGHT {
        let v = (y as f32 + 0.5) / OUTPUT_HEIGHT as f32;
        let latitude = (0.5 - v) * std::f32::consts::PI;
        for x in 0..OUTPUT_WIDTH {
            let u = (x as f32 + 0.5) / OUTPUT_WIDTH as f32;
            let longitude = (u - 0.5) * 2.0 * std::f32::consts::PI;
            let normal = [
                latitude.cos() * longitude.cos(),
                latitude.sin(),
                latitude.cos() * longitude.sin(),
            ];
            let helper = if normal[1].abs() > 0.98 {
                [1.0, 0.0, 0.0]
            } else {
                [0.0, 1.0, 0.0]
            };
            let tangent = normalize(cross(helper, normal));
            let bitangent = cross(normal, tangent);
            let mut color = [0.0; 3];
            for sample in 0..SAMPLE_COUNT {
                let xi = (sample as f32 + 0.5) / SAMPLE_COUNT as f32;
                let radius = xi.sqrt();
                let local_height = (1.0 - xi).sqrt();
                let angle = sample as f32 * 2.39996323;
                let direction = add(
                    add(
                        scale(tangent, radius * angle.cos()),
                        scale(bitangent, radius * angle.sin()),
                    ),
                    scale(normal, local_height),
                );
                let sample_u =
                    direction[2].atan2(direction[0]) / (2.0 * std::f32::consts::PI) + 0.5;
                let sample_v = direction[1].clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
                let sample_color =
                    sample_equirectangular(source, width, height, sample_u, sample_v);
                for channel in 0..3 {
                    color[channel] += sample_color[channel] / SAMPLE_COUNT as f32;
                }
            }
            let index = ((y * OUTPUT_WIDTH + x) * 4) as usize;
            irradiance[index..index + 3].copy_from_slice(&color);
            irradiance[index + 3] = 1.0;
        }
    }
    irradiance
}

fn sample_equirectangular(source: &[f32], width: u32, height: u32, u: f32, v: f32) -> [f32; 3] {
    let x = u.rem_euclid(1.0) * width as f32 - 0.5;
    let y = (v.clamp(0.0, 1.0) * height as f32 - 0.5).clamp(0.0, (height - 1) as f32);
    let x0 = x.floor() as i32;
    let y0 = y.floor() as u32;
    let tx = x - x.floor();
    let ty = y - y.floor();
    let x1 = x0 + 1;
    let y1 = (y0 + 1).min(height - 1);
    let mut result = [0.0; 3];
    for channel in 0..3_usize {
        let read = |sample_x: i32, sample_y: u32| {
            let wrapped_x = sample_x.rem_euclid(width as i32) as u32;
            source[((sample_y * width + wrapped_x) * 4 + channel as u32) as usize]
        };
        let top = read(x0, y0) * (1.0 - tx) + read(x1, y0) * tx;
        let bottom = read(x0, y1) * (1.0 - tx) + read(x1, y1) * tx;
        result[channel] = top * (1.0 - ty) + bottom * ty;
    }
    result
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(vector: [f32; 3]) -> [f32; 3] {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    [vector[0] / length, vector[1] / length, vector[2] / length]
}

fn scale(vector: [f32; 3], factor: f32) -> [f32; 3] {
    [vector[0] * factor, vector[1] * factor, vector[2] * factor]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn subtract(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn softbox(
    u: f32,
    v: f32,
    center_u: f32,
    center_v: f32,
    half_u: f32,
    half_v: f32,
    power: f32,
) -> f32 {
    let dx = ((u - center_u + 0.5).rem_euclid(1.0) - 0.5).abs();
    let dy = (v - center_v).abs();
    let edge_x = ((half_u - dx) / (half_u * 0.18)).clamp(0.0, 1.0);
    let edge_y = ((half_v - dy) / (half_v * 0.18)).clamp(0.0, 1.0);
    let smooth = edge_x * edge_x * (3.0 - 2.0 * edge_x) * edge_y * edge_y * (3.0 - 2.0 * edge_y);
    smooth * power
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ggx_prefilter_preserves_a_constant_environment() {
        let source = [2.0, 1.0, 0.5, 1.0];
        let filtered = prefilter_specular_level(&source, 1, 1, 4, 2, 0.8, 32);
        for pixel in filtered.chunks_exact(4) {
            assert!((pixel[0] - 2.0).abs() < 1e-5);
            assert!((pixel[1] - 1.0).abs() < 1e-5);
            assert!((pixel[2] - 0.5).abs() < 1e-5);
            assert_eq!(pixel[3], 1.0);
        }
    }

    #[test]
    fn integrated_brdf_coefficients_are_finite_and_energy_bounded() {
        for roughness in [0.05, 0.5, 1.0] {
            let (scale, bias) = integrate_brdf(0.5, roughness, 256);
            assert!(scale.is_finite() && scale >= 0.0);
            assert!(bias.is_finite() && bias >= 0.0);
            assert!(scale + bias <= 1.05);
        }
    }

    #[test]
    fn diffuse_convolution_preserves_a_constant_environment() {
        let irradiance = convolve_diffuse(&[2.0, 1.0, 0.5, 1.0], 1, 1);
        for pixel in irradiance.chunks_exact(4) {
            assert!((pixel[0] - 2.0).abs() < 1e-5);
            assert!((pixel[1] - 1.0).abs() < 1e-5);
            assert!((pixel[2] - 0.5).abs() < 1e-5);
            assert_eq!(pixel[3], 1.0);
        }
    }
}
