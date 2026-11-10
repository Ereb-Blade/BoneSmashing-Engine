use std::{collections::HashMap, path::Path};

use glam::{Mat4, Quat, Vec3};

use crate::core::{MaterialId, MeshId};

use super::mesh::{
    MeshData, SkinVertex, UvSetsVertex, Vertex, calculate_normals, calculate_tangents,
    calculate_tangents_with_uv,
};

const MAX_IMPORTED_TEXTURE_DIMENSION: u32 = 1024;
const MAX_IMPORTED_IMAGE_DATA_BYTES: usize = 128 * 1024 * 1024;
const MAX_IMPORTED_MIPMAP_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug)]
pub struct GltfImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub binary_alpha_mask: bool,
    pub base_color_mip_levels: Option<Vec<Vec<u8>>>,
    pub metallic_roughness_mip_levels: Option<Vec<Vec<u8>>>,
    pub normal_mip_levels: Option<Vec<Vec<u8>>>,
    pub data_mip_levels: Option<Vec<Vec<u8>>>,
    pub emissive_mip_levels: Option<Vec<Vec<u8>>>,
}

fn instance_name(node: &gltf::Node<'_>, mesh: &gltf::Mesh<'_>, primitive_index: usize) -> String {
    let node_name = node.name().map(str::trim).filter(|name| !name.is_empty());
    let mesh_name = mesh.name().map(str::trim).filter(|name| !name.is_empty());
    let base_name = match (node_name, mesh_name) {
        (Some(node_name), Some(mesh_name)) if node_name != mesh_name => {
            format!("{node_name} / {mesh_name}")
        }
        (Some(name), _) | (_, Some(name)) => name.to_owned(),
        (None, None) => format!("Node {}", node.index() + 1),
    };
    if mesh.primitives().len() > 1 {
        format!("{base_name} - primitive {}", primitive_index + 1)
    } else {
        base_name
    }
}

#[derive(Debug)]
pub struct GltfMaterialData {
    pub base_color_factor: [f32; 4],
    pub roughness_factor: f32,
    pub metallic_factor: f32,
    pub specular_glossiness: bool,
    pub unlit: bool,
    pub alpha_mode: u32,
    pub alpha_cutoff: f32,
    pub double_sided: bool,
    pub transmission_factor: f32,
    pub ior: f32,
    pub specular_factor: f32,
    pub specular_color_factor: [f32; 3],
    pub emissive_factor: [f32; 3],
    pub emissive_strength: f32,
    pub occlusion_strength: f32,
    pub base_color_image: Option<usize>,
    pub base_color_tex_coord: u32,
    pub base_color_transform: [[f32; 4]; 2],
    pub metallic_roughness_image: Option<usize>,
    pub metallic_roughness_tex_coord: u32,
    pub metallic_roughness_transform: [[f32; 4]; 2],
    pub normal_image: Option<usize>,
    pub normal_tex_coord: u32,
    pub normal_scale: f32,
    pub normal_transform: [[f32; 4]; 2],
    pub occlusion_image: Option<usize>,
    pub occlusion_tex_coord: u32,
    pub occlusion_transform: [[f32; 4]; 2],
    pub emissive_image: Option<usize>,
    pub emissive_tex_coord: u32,
    pub emissive_transform: [[f32; 4]; 2],
    pub specular_image: Option<usize>,
    pub specular_tex_coord: u32,
    pub specular_transform: [[f32; 4]; 2],
    pub specular_color_image: Option<usize>,
    pub specular_color_tex_coord: u32,
    pub specular_color_transform: [[f32; 4]; 2],
    pub transmission_image: Option<usize>,
    pub transmission_tex_coord: u32,
    pub transmission_transform: [[f32; 4]; 2],
    pub thickness_factor: f32,
    pub thickness_image: Option<usize>,
    pub thickness_tex_coord: u32,
    pub thickness_transform: [[f32; 4]; 2],
    pub attenuation_distance: f32,
    pub attenuation_color: [f32; 3],
}

const IDENTITY_UV_TRANSFORM: [[f32; 4]; 2] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]];

fn texture_transform(
    extension: Option<&gltf::json::Value>,
) -> Result<([[f32; 4]; 2], Option<u32>), String> {
    let Some(transform) = extension.and_then(|value| value.get("KHR_texture_transform")) else {
        return Ok((IDENTITY_UV_TRANSFORM, None));
    };
    let parse_vec2 = |name: &str, default| -> Result<[f32; 2], String> {
        let Some(value) = transform.get(name) else {
            return Ok(default);
        };
        let values = value
            .as_array()
            .filter(|values| values.len() == 2)
            .ok_or_else(|| format!("texture transform {name} must contain two numbers"))?;
        let x = values[0]
            .as_f64()
            .ok_or_else(|| format!("texture transform {name} contains a non-number"))?
            as f32;
        let y = values[1]
            .as_f64()
            .ok_or_else(|| format!("texture transform {name} contains a non-number"))?
            as f32;
        if !x.is_finite() || !y.is_finite() {
            return Err(format!(
                "texture transform {name} contains a non-finite value"
            ));
        }
        Ok([x, y])
    };
    let offset = parse_vec2("offset", [0.0; 2])?;
    let scale = parse_vec2("scale", [1.0; 2])?;
    let rotation = transform
        .get("rotation")
        .map(|value| {
            value
                .as_f64()
                .map(|value| value as f32)
                .filter(|value| value.is_finite())
                .ok_or_else(|| "texture transform rotation must be finite".to_owned())
        })
        .transpose()?
        .unwrap_or(0.0);
    let tex_coord = transform
        .get("texCoord")
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| "texture transform texCoord must be an unsigned integer".to_owned())
        })
        .transpose()?;
    let (sin, cos) = rotation.sin_cos();
    let transform = [
        [cos * scale[0], -sin * scale[1], offset[0], 0.0],
        [sin * scale[0], cos * scale[1], offset[1], 0.0],
    ];
    if transform.iter().flatten().any(|value| !value.is_finite()) {
        return Err("texture transform produces a non-finite UV coordinate".to_owned());
    }
    Ok((transform, tex_coord))
}

fn texture_info_transform(
    info: Option<&gltf::texture::Info<'_>>,
) -> Result<([[f32; 4]; 2], Option<u32>), String> {
    let Some(transform) = info.and_then(|info| info.texture_transform()) else {
        return Ok((IDENTITY_UV_TRANSFORM, None));
    };
    let offset = transform.offset();
    let scale = transform.scale();
    let rotation = transform.rotation();
    if offset
        .iter()
        .chain(scale.iter())
        .any(|value| !value.is_finite())
        || !rotation.is_finite()
    {
        return Err("texture transform contains a non-finite value".to_owned());
    }
    let tex_coord = transform.tex_coord();
    let (sin, cos) = rotation.sin_cos();
    let transform = [
        [cos * scale[0], -sin * scale[1], offset[0], 0.0],
        [sin * scale[0], cos * scale[1], offset[1], 0.0],
    ];
    if transform.iter().flatten().any(|value| !value.is_finite()) {
        return Err("texture transform produces a non-finite UV coordinate".to_owned());
    }
    Ok((transform, tex_coord))
}

#[derive(Debug)]
pub struct GltfPrimitiveData {
    pub mesh: MeshData,
    pub material_index: usize,
}

#[derive(Debug)]
pub struct GltfPrimitiveInstance {
    pub name: String,
    pub mesh_id: MeshId,
    pub material_id: MaterialId,
    pub node_transform: Mat4,
    pub skin_matrices: Vec<Mat4>,
}

#[derive(Debug)]
pub struct LoadedGltfModel {
    pub instances: Vec<GltfPrimitiveInstance>,
    pub animations: Vec<GltfAnimationInfo>,
    animation_data: Option<GltfAnimationData>,
}

#[derive(Clone, Debug)]
pub struct GltfAnimationInfo {
    pub name: String,
    pub duration: f32,
}

impl LoadedGltfModel {
    pub fn sample_animation(
        &self,
        animation_index: usize,
        time: f32,
    ) -> Result<Vec<GltfPrimitiveInstance>, String> {
        let data = self
            .animation_data
            .as_ref()
            .ok_or_else(|| "model does not contain skeletal animation data".to_owned())?;
        data.sample(animation_index, time)
    }

    pub fn sample_animation_into(
        &mut self,
        animation_index: usize,
        time: f32,
        output: &mut Vec<GltfPrimitiveInstance>,
    ) -> Result<(), String> {
        let data = self
            .animation_data
            .as_mut()
            .ok_or_else(|| "model does not contain skeletal animation data".to_owned())?;
        data.sample_into(animation_index, time, output)
    }
}

#[derive(Clone, Copy, Debug)]
struct NodePose {
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
    matrix: Mat4,
    animated: bool,
}

impl NodePose {
    fn matrix(self) -> Mat4 {
        if self.animated {
            Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
        } else {
            self.matrix
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum AnimationPath {
    Translation,
    Rotation,
    Scale,
}

#[derive(Clone, Copy, Debug)]
enum Interpolation {
    Step,
    Linear,
    CubicSpline,
}

#[derive(Debug)]
struct AnimationChannelData {
    node_index: usize,
    path: AnimationPath,
    interpolation: Interpolation,
    times: Vec<f32>,
    values: Vec<[f32; 4]>,
}

#[derive(Debug)]
struct AnimationClipData {
    channels: Vec<AnimationChannelData>,
}

#[derive(Debug)]
struct SkinData {
    joints: Vec<usize>,
    inverse_bind_matrices: Vec<Mat4>,
}

#[derive(Debug)]
struct AnimationInstanceData {
    name: String,
    primitive_index: usize,
    node_index: usize,
    skin_index: Option<usize>,
}

#[derive(Debug)]
struct GltfAnimationData {
    base_nodes: Vec<NodePose>,
    parents: Vec<Option<usize>>,
    skins: Vec<SkinData>,
    instances: Vec<AnimationInstanceData>,
    clips: Vec<AnimationClipData>,
    animation_infos: Vec<GltfAnimationInfo>,
    mesh_ids: Vec<MeshId>,
    material_ids: Vec<MaterialId>,
    sample_nodes: Vec<NodePose>,
    sample_globals: Vec<Mat4>,
    sample_computed: Vec<bool>,
    sample_visiting: Vec<bool>,
}

impl GltfAnimationData {
    fn sample(
        &self,
        animation_index: usize,
        time: f32,
    ) -> Result<Vec<GltfPrimitiveInstance>, String> {
        if !time.is_finite() {
            return Err("animation time must be finite".to_owned());
        }
        let clip = self
            .clips
            .get(animation_index)
            .ok_or_else(|| format!("animation index {animation_index} is out of range"))?;
        let mut nodes = self.base_nodes.clone();
        let duration = self.animation_infos[animation_index].duration;
        apply_animation_channels(clip, duration, time, &mut nodes)?;

        let globals = calculate_global_transforms(&nodes, &self.parents)?;
        self.instances
            .iter()
            .map(|instance| {
                let node_transform = globals[instance.node_index];
                let skin_matrices = if let Some(skin_index) = instance.skin_index {
                    let skin = &self.skins[skin_index];
                    let inverse_mesh = node_transform.inverse();
                    if !inverse_mesh.is_finite() {
                        return Err("skinned mesh node has a non-invertible transform".to_owned());
                    }
                    skin.joints
                        .iter()
                        .zip(&skin.inverse_bind_matrices)
                        .map(|(&joint, inverse_bind)| inverse_mesh * globals[joint] * *inverse_bind)
                        .collect()
                } else {
                    Vec::new()
                };
                Ok(GltfPrimitiveInstance {
                    name: instance.name.clone(),
                    mesh_id: self.mesh_ids[instance.primitive_index],
                    material_id: self.material_ids[instance.primitive_index],
                    node_transform,
                    skin_matrices,
                })
            })
            .collect()
    }

    fn sample_into(
        &mut self,
        animation_index: usize,
        time: f32,
        output: &mut Vec<GltfPrimitiveInstance>,
    ) -> Result<(), String> {
        if !time.is_finite() {
            return Err("animation time must be finite".to_owned());
        }
        let clip = self
            .clips
            .get(animation_index)
            .ok_or_else(|| format!("animation index {animation_index} is out of range"))?;
        self.sample_nodes.copy_from_slice(&self.base_nodes);
        let duration = self.animation_infos[animation_index].duration;
        apply_animation_channels(clip, duration, time, &mut self.sample_nodes)?;

        calculate_global_transforms_into(
            &self.sample_nodes,
            &self.parents,
            &mut self.sample_globals,
            &mut self.sample_computed,
            &mut self.sample_visiting,
        )?;

        output.truncate(self.instances.len());
        for index in 0..self.instances.len() {
            let instance = &self.instances[index];
            let node_transform = self.sample_globals[instance.node_index];
            if index == output.len() {
                output.push(GltfPrimitiveInstance {
                    name: instance.name.clone(),
                    mesh_id: self.mesh_ids[instance.primitive_index],
                    material_id: self.material_ids[instance.primitive_index],
                    node_transform,
                    skin_matrices: Vec::new(),
                });
            }
            let sampled = &mut output[index];
            sampled.mesh_id = self.mesh_ids[instance.primitive_index];
            sampled.material_id = self.material_ids[instance.primitive_index];
            sampled.node_transform = node_transform;
            sampled.skin_matrices.clear();
            if let Some(skin_index) = instance.skin_index {
                let skin = &self.skins[skin_index];
                let inverse_mesh = node_transform.inverse();
                if !inverse_mesh.is_finite() {
                    return Err("skinned mesh node has a non-invertible transform".to_owned());
                }
                sampled.skin_matrices.reserve(skin.joints.len());
                for (&joint, inverse_bind) in skin.joints.iter().zip(&skin.inverse_bind_matrices) {
                    sampled
                        .skin_matrices
                        .push(inverse_mesh * self.sample_globals[joint] * *inverse_bind);
                }
            }
        }
        Ok(())
    }
}

fn apply_animation_channels(
    clip: &AnimationClipData,
    duration: f32,
    time: f32,
    nodes: &mut [NodePose],
) -> Result<(), String> {
    let time = time.clamp(0.0, duration);
    for channel in &clip.channels {
        let mut value = sample_channel(channel, time);
        if matches!(channel.path, AnimationPath::Rotation) {
            let rotation = Quat::from_array(value);
            if !rotation.is_finite() || rotation.length_squared() <= f32::EPSILON {
                return Err("animation produced an invalid joint rotation".to_owned());
            }
            value = rotation.normalize().to_array();
        }
        let node = nodes
            .get_mut(channel.node_index)
            .ok_or_else(|| "animation targets a missing node".to_owned())?;
        match channel.path {
            AnimationPath::Translation => {
                node.translation = Vec3::new(value[0], value[1], value[2]);
                node.animated = true;
            }
            AnimationPath::Scale => {
                node.scale = Vec3::new(value[0], value[1], value[2]);
                node.animated = true;
            }
            AnimationPath::Rotation => {
                node.rotation = Quat::from_xyzw(value[0], value[1], value[2], value[3]).normalize();
                node.animated = true;
            }
        }
    }
    Ok(())
}

fn sample_channel(channel: &AnimationChannelData, time: f32) -> [f32; 4] {
    let last = channel.times.len() - 1;
    if time <= channel.times[0] {
        return channel_value(channel, 0);
    }
    if time >= channel.times[last] {
        return channel_value(channel, last);
    }
    let upper = channel.times.partition_point(|&key_time| key_time <= time);
    let lower = upper - 1;
    if matches!(channel.interpolation, Interpolation::Step) {
        return channel_value(channel, lower);
    }

    let start_time = channel.times[lower];
    let end_time = channel.times[upper];
    let duration = end_time - start_time;
    let factor = (time - start_time) / duration;
    let start = channel_value(channel, lower);
    let end = channel_value(channel, upper);
    if matches!(channel.interpolation, Interpolation::CubicSpline) {
        let out_tangent = channel.values[lower * 3 + 2];
        let in_tangent = channel.values[upper * 3];
        let t2 = factor * factor;
        let t3 = t2 * factor;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + factor;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        let mut value = [0.0; 4];
        for index in 0..4 {
            value[index] = h00 * start[index]
                + h10 * duration * out_tangent[index]
                + h01 * end[index]
                + h11 * duration * in_tangent[index];
        }
        if matches!(channel.path, AnimationPath::Rotation) {
            let rotation = Quat::from_xyzw(value[0], value[1], value[2], value[3]).normalize();
            return rotation.to_array();
        }
        return value;
    }
    if matches!(channel.path, AnimationPath::Rotation) {
        Quat::from_array(start)
            .normalize()
            .slerp(Quat::from_array(end).normalize(), factor)
            .normalize()
            .to_array()
    } else {
        std::array::from_fn(|index| start[index] + (end[index] - start[index]) * factor)
    }
}

fn channel_value(channel: &AnimationChannelData, index: usize) -> [f32; 4] {
    channel.values[if matches!(channel.interpolation, Interpolation::CubicSpline) {
        index * 3 + 1
    } else {
        index
    }]
}

#[derive(Debug)]
pub struct GltfModelData {
    pub images: Vec<GltfImageData>,
    pub materials: Vec<GltfMaterialData>,
    pub primitives: Vec<GltfPrimitiveData>,
    instances: Vec<AnimationInstanceData>,
    nodes: Vec<NodePose>,
    parents: Vec<Option<usize>>,
    skins: Vec<SkinData>,
    clips: Vec<AnimationClipData>,
    animation_infos: Vec<GltfAnimationInfo>,
}

impl GltfModelData {
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let (document, buffers, images) =
            gltf::import(path).map_err(|error| format!("{}: {error}", path.display()))?;

        let mut converted_images = Vec::with_capacity(images.len());
        let mut converted_image_bytes = 0usize;
        for image in images {
            let converted =
                convert_image(&image).map_err(|error| format!("{}: {error}", path.display()))?;
            converted_image_bytes = converted_image_bytes
                .checked_add(converted.rgba.len())
                .ok_or_else(|| format!("{}: converted image data size overflow", path.display()))?;
            if converted_image_bytes > MAX_IMPORTED_IMAGE_DATA_BYTES {
                return Err(format!(
                    "{}: resized image data exceeds the {} MiB import budget",
                    path.display(),
                    MAX_IMPORTED_IMAGE_DATA_BYTES / (1024 * 1024)
                ));
            }
            converted_images.push(converted);
        }
        let mut images = converted_images;

        let mut materials = document
            .materials()
            .map(|material| {
                if let Some(extension) = material.extensions().and_then(|extensions| {
                    extensions
                        .keys()
                        .find(|name| name.starts_with("KHR_materials_"))
                }) {
                    return Err(format!(
                        "material '{}' uses unsupported extension '{extension}'",
                        material.name().unwrap_or("unnamed")
                    ));
                }
                let pbr = material.pbr_metallic_roughness();
                let specular_glossiness = material.pbr_specular_glossiness();
                let base_color_texture = match specular_glossiness.as_ref() {
                    Some(extension) => extension.diffuse_texture(),
                    None => pbr.base_color_texture(),
                };
                let metallic_roughness_texture = pbr.metallic_roughness_texture();
                let normal_texture = material.normal_texture();
                let occlusion_texture = material.occlusion_texture();
                let emissive_texture = material.emissive_texture();
                let transmission = material.transmission();
                let ior = material.ior().unwrap_or(1.5);
                let specular = material.specular();
                let transmission_texture = transmission
                    .as_ref()
                    .and_then(|extension| extension.transmission_texture());
                let volume = material.volume();
                let thickness_texture = volume
                    .as_ref()
                    .and_then(|extension| extension.thickness_texture());
                let specular_texture =
                    specular.as_ref().and_then(|extension| extension.specular_texture());
                let specular_color_texture = specular
                    .as_ref()
                    .and_then(|extension| extension.specular_color_texture());
                let specular_glossiness_texture = specular_glossiness
                    .as_ref()
                    .and_then(|extension| extension.specular_glossiness_texture());
                if specular_glossiness.is_some() && specular.is_some() {
                    return Err(format!(
                        "material '{}' cannot combine KHR_materials_pbrSpecularGlossiness with KHR_materials_specular",
                        material.name().unwrap_or("unnamed")
                    ));
                }

                let (base_color_transform, base_color_transform_uv) =
                    texture_info_transform(base_color_texture.as_ref())?;
                let (metallic_roughness_transform, metallic_roughness_transform_uv) =
                    texture_info_transform(metallic_roughness_texture.as_ref())?;
                let (normal_transform, normal_transform_uv) = texture_transform(
                    normal_texture
                        .as_ref()
                        .and_then(|texture| texture.extension_value("KHR_texture_transform")),
                )?;
                let (occlusion_transform, occlusion_transform_uv) = texture_transform(
                    occlusion_texture
                        .as_ref()
                        .and_then(|texture| texture.extension_value("KHR_texture_transform")),
                )?;
                let (emissive_transform, emissive_transform_uv) =
                    texture_info_transform(emissive_texture.as_ref())?;
                let (specular_transform, specular_transform_uv) =
                    texture_info_transform(specular_texture.as_ref())?;
                let (specular_color_transform, specular_color_transform_uv) =
                    texture_info_transform(
                        specular_glossiness_texture
                            .as_ref()
                            .or(specular_color_texture.as_ref()),
                    )?;
                let (transmission_transform, transmission_transform_uv) =
                    texture_info_transform(transmission_texture.as_ref())?;
                let (thickness_transform, thickness_transform_uv) =
                    texture_info_transform(thickness_texture.as_ref())?;
                let texture_coords = [
                    base_color_transform_uv
                        .unwrap_or_else(|| base_color_texture.as_ref().map_or(0, |info| info.tex_coord())),
                    metallic_roughness_transform_uv.unwrap_or_else(|| {
                        metallic_roughness_texture.as_ref().map_or(0, |info| info.tex_coord())
                    }),
                    normal_transform_uv.unwrap_or_else(|| {
                        normal_texture.as_ref().map_or(0, |info| info.tex_coord())
                    }),
                    occlusion_transform_uv.unwrap_or_else(|| {
                        occlusion_texture.as_ref().map_or(0, |info| info.tex_coord())
                    }),
                    emissive_transform_uv
                        .unwrap_or_else(|| emissive_texture.as_ref().map_or(0, |info| info.tex_coord())),
                    specular_transform_uv.unwrap_or_else(|| {
                        specular_texture.as_ref().map_or(0, |info| info.tex_coord())
                    }),
                    specular_color_transform_uv.unwrap_or_else(|| {
                        specular_glossiness_texture
                            .as_ref()
                            .or(specular_color_texture.as_ref())
                            .map_or(0, |info| info.tex_coord())
                    }),
                    transmission_transform_uv.unwrap_or_else(|| {
                        transmission_texture.as_ref().map_or(0, |info| info.tex_coord())
                    }),
                    thickness_transform_uv.unwrap_or_else(|| {
                        thickness_texture.as_ref().map_or(0, |info| info.tex_coord())
                    }),
                ];
                if texture_coords.iter().any(|&tex_coord| tex_coord > 3) {
                    return Err(format!(
                        "material '{}' uses an unsupported texture coordinate set (supported: TEXCOORD_0 through TEXCOORD_3)",
                        material.name().unwrap_or("unnamed")
                    ));
                }

                let base_color_factor = specular_glossiness
                    .as_ref()
                    .map_or_else(|| pbr.base_color_factor(), |extension| extension.diffuse_factor());
                let roughness_factor = specular_glossiness.as_ref().map_or_else(
                    || pbr.roughness_factor(),
                    |extension| 1.0 - extension.glossiness_factor(),
                );
                let metallic_factor = if specular_glossiness.is_some() {
                    0.0
                } else {
                    pbr.metallic_factor()
                };
                let alpha_cutoff = material.alpha_cutoff().unwrap_or(0.5);
                let normal_scale = normal_texture
                    .as_ref()
                    .map_or(1.0, |texture| texture.scale());
                let transmission_factor = transmission
                    .as_ref()
                    .map_or(0.0, |extension| extension.transmission_factor());
                let specular_factor = specular
                    .as_ref()
                    .map_or(1.0, |extension| extension.specular_factor());
                let specular_color_factor = specular_glossiness.as_ref().map_or_else(
                    || {
                        specular
                            .as_ref()
                            .map_or([1.0; 3], |extension| extension.specular_color_factor())
                    },
                    |extension| extension.specular_factor(),
                );
                let emissive_factor = material.emissive_factor();
                let emissive_strength = material.emissive_strength().unwrap_or(1.0);
                let occlusion_strength = occlusion_texture
                    .as_ref()
                    .map_or(1.0, |texture| texture.strength());
                let thickness_factor = volume
                    .as_ref()
                    .map_or(0.0, |extension| extension.thickness_factor());
                let attenuation_distance = volume
                    .as_ref()
                    .map_or(f32::INFINITY, |extension| {
                        extension.attenuation_distance()
                    });
                let attenuation_color = volume
                    .as_ref()
                    .map_or([1.0; 3], |extension| extension.attenuation_color());
                let alpha_mode = match material.alpha_mode() {
                    gltf::material::AlphaMode::Opaque => 0,
                    gltf::material::AlphaMode::Mask => 1,
                    gltf::material::AlphaMode::Blend => {
                        let is_binary_cutout = base_color_texture
                            .as_ref()
                            .and_then(|texture| images.get(texture.texture().source().index()))
                            .is_some_and(is_binary_cutout_texture);
                        if is_binary_cutout { 1 } else { 2 }
                    }
                };
                if base_color_factor.iter().any(|value| !value.is_finite())
                    || !roughness_factor.is_finite()
                    || !metallic_factor.is_finite()
                    || !alpha_cutoff.is_finite()
                    || !normal_scale.is_finite()
                    || !transmission_factor.is_finite()
                    || !ior.is_finite()
                    || !specular_factor.is_finite()
                    || !emissive_strength.is_finite()
                    || !occlusion_strength.is_finite()
                    || !thickness_factor.is_finite()
                    || attenuation_distance.is_nan()
                    || attenuation_distance <= 0.0
                        && attenuation_distance != f32::INFINITY
                    || attenuation_color.iter().any(|value| !value.is_finite())
                    || emissive_factor.iter().any(|value| !value.is_finite())
                    || specular_color_factor.iter().any(|value| !value.is_finite())
                {
                    return Err(format!(
                        "material '{}' contains non-finite PBR values",
                        material.name().unwrap_or("unnamed")
                    ));
                }
                if base_color_factor
                    .iter()
                    .any(|value| !(0.0..=1.0).contains(value))
                    || !(0.0..=1.0).contains(&roughness_factor)
                    || !(0.0..=1.0).contains(&metallic_factor)
                    || !(0.0..=1.0).contains(&alpha_cutoff)
                    || !(0.0..=1.0).contains(&transmission_factor)
                    || ior < 1.0
                    || !(0.0..=1.0).contains(&specular_factor)
                    || !(0.0..=1.0).contains(&occlusion_strength)
                    || emissive_strength < 0.0
                    || thickness_factor < 0.0
                    || attenuation_color
                        .iter()
                        .any(|value| !(0.0..=1.0).contains(value))
                    || emissive_factor
                        .iter()
                        .any(|value| !(0.0..=1.0).contains(value))
                    || specular_color_factor
                        .iter()
                        .any(|value| !(0.0..=1.0).contains(value))
                {
                    return Err(format!(
                        "material '{}' has PBR factors outside the glTF range",
                        material.name().unwrap_or("unnamed")
                    ));
                }

                Ok(GltfMaterialData {
                    base_color_factor,
                    roughness_factor,
                    metallic_factor,
                    specular_glossiness: specular_glossiness.is_some(),
                    unlit: material.unlit(),
                    alpha_mode,
                    alpha_cutoff,
                    double_sided: material.double_sided(),
                    transmission_factor,
                    ior,
                    specular_factor,
                    specular_color_factor,
                    emissive_factor,
                    emissive_strength,
                    occlusion_strength,
                    base_color_image: base_color_texture
                        .as_ref()
                        .map(|info| info.texture().source().index()),
                    base_color_tex_coord: texture_coords[0],
                    base_color_transform,
                    metallic_roughness_image: metallic_roughness_texture
                        .as_ref()
                        .map(|info| info.texture().source().index()),
                    metallic_roughness_tex_coord: texture_coords[1],
                    metallic_roughness_transform,
                    normal_image: normal_texture
                        .as_ref()
                        .map(|info| info.texture().source().index()),
                    normal_tex_coord: texture_coords[2],
                    normal_scale,
                    normal_transform,
                    occlusion_image: occlusion_texture
                        .as_ref()
                        .map(|info| info.texture().source().index()),
                    occlusion_tex_coord: texture_coords[3],
                    occlusion_transform,
                    emissive_image: emissive_texture
                        .as_ref()
                        .map(|info| info.texture().source().index()),
                    emissive_tex_coord: texture_coords[4],
                    emissive_transform,
                    specular_image: specular_texture
                        .as_ref()
                        .map(|info| info.texture().source().index()),
                    specular_tex_coord: texture_coords[5],
                    specular_transform,
                    specular_color_image: specular_glossiness_texture
                        .as_ref()
                        .or(specular_color_texture.as_ref())
                        .map(|info| info.texture().source().index()),
                    specular_color_tex_coord: texture_coords[6],
                    specular_color_transform,
                    transmission_image: transmission_texture
                        .as_ref()
                        .map(|info| info.texture().source().index()),
                    transmission_tex_coord: texture_coords[7],
                    transmission_transform,
                    thickness_factor,
                    thickness_image: thickness_texture
                        .as_ref()
                        .map(|info| info.texture().source().index()),
                    thickness_tex_coord: texture_coords[8],
                    thickness_transform,
                    attenuation_distance: if attenuation_distance.is_infinite() {
                        0.0
                    } else {
                        attenuation_distance
                    },
                    attenuation_color,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        let default_material_index = materials.len();
        materials.push(GltfMaterialData {
            base_color_factor: [1.0; 4],
            roughness_factor: 1.0,
            metallic_factor: 1.0,
            specular_glossiness: false,
            unlit: false,
            alpha_mode: 0,
            alpha_cutoff: 0.5,
            double_sided: false,
            transmission_factor: 0.0,
            ior: 1.5,
            specular_factor: 1.0,
            specular_color_factor: [1.0; 3],
            emissive_factor: [0.0; 3],
            emissive_strength: 1.0,
            occlusion_strength: 1.0,
            base_color_image: None,
            base_color_tex_coord: 0,
            base_color_transform: IDENTITY_UV_TRANSFORM,
            metallic_roughness_image: None,
            metallic_roughness_tex_coord: 0,
            metallic_roughness_transform: IDENTITY_UV_TRANSFORM,
            normal_image: None,
            normal_tex_coord: 0,
            normal_scale: 1.0,
            normal_transform: IDENTITY_UV_TRANSFORM,
            occlusion_image: None,
            occlusion_tex_coord: 0,
            occlusion_transform: IDENTITY_UV_TRANSFORM,
            emissive_image: None,
            emissive_tex_coord: 0,
            emissive_transform: IDENTITY_UV_TRANSFORM,
            specular_image: None,
            specular_tex_coord: 0,
            specular_transform: IDENTITY_UV_TRANSFORM,
            specular_color_image: None,
            specular_color_tex_coord: 0,
            specular_color_transform: IDENTITY_UV_TRANSFORM,
            transmission_image: None,
            transmission_tex_coord: 0,
            transmission_transform: IDENTITY_UV_TRANSFORM,
            thickness_factor: 0.0,
            thickness_image: None,
            thickness_tex_coord: 0,
            thickness_transform: IDENTITY_UV_TRANSFORM,
            attenuation_distance: 0.0,
            attenuation_color: [1.0; 3],
        });
        if materials.iter().any(|material| {
            material
                .base_color_image
                .is_some_and(|image| image >= images.len())
                || material
                    .metallic_roughness_image
                    .is_some_and(|image| image >= images.len())
                || material
                    .normal_image
                    .is_some_and(|image| image >= images.len())
                || material
                    .occlusion_image
                    .is_some_and(|image| image >= images.len())
                || material
                    .emissive_image
                    .is_some_and(|image| image >= images.len())
                || material
                    .specular_image
                    .is_some_and(|image| image >= images.len())
                || material
                    .specular_color_image
                    .is_some_and(|image| image >= images.len())
                || material
                    .transmission_image
                    .is_some_and(|image| image >= images.len())
        }) {
            return Err(format!(
                "{}: material references an image that was not imported",
                path.display()
            ));
        }

        let mut primitives = Vec::new();
        let mut primitive_indices = HashMap::new();
        for mesh in document.meshes() {
            for (primitive_index, primitive) in mesh.primitives().enumerate() {
                if primitive.mode() != gltf::mesh::Mode::Triangles {
                    return Err(format!(
                        "mesh '{}' contains a non-triangle primitive",
                        mesh.name().unwrap_or("unnamed")
                    ));
                }
                if primitive.morph_targets().next().is_some() {
                    return Err(format!(
                        "mesh '{}' uses unsupported morph targets",
                        mesh.name().unwrap_or("unnamed")
                    ));
                }

                let reader = primitive.reader(|buffer| {
                    buffers
                        .get(buffer.index())
                        .map(|buffer_data| buffer_data.0.as_slice())
                });
                let positions: Vec<[f32; 3]> = reader
                    .read_positions()
                    .ok_or_else(|| {
                        format!(
                            "mesh '{}' primitive has no POSITION attribute",
                            mesh.name().unwrap_or("unnamed")
                        )
                    })?
                    .collect();
                if positions.is_empty() || positions.len() > u32::MAX as usize {
                    return Err(format!(
                        "mesh '{}' has an invalid vertex count",
                        mesh.name().unwrap_or("unnamed")
                    ));
                }

                let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(Iterator::collect);
                let source_tangents: Option<Vec<[f32; 4]>> =
                    reader.read_tangents().map(Iterator::collect);
                let tex_coords: [Option<Vec<[f32; 2]>>; 4] = std::array::from_fn(|set| {
                    reader
                        .read_tex_coords(set as u32)
                        .map(|coordinates| coordinates.into_f32().collect())
                });
                let colors: Option<Vec<[f32; 4]>> = reader
                    .read_colors(0)
                    .map(|colors| colors.into_rgba_f32().collect());
                let joints: Option<Vec<[u16; 4]>> = reader
                    .read_joints(0)
                    .map(|joints| joints.into_u16().collect());
                let weights: Option<Vec<[f32; 4]>> = reader
                    .read_weights(0)
                    .map(|weights| weights.into_f32().collect());
                let joints1: Option<Vec<[u16; 4]>> = reader
                    .read_joints(1)
                    .map(|joints| joints.into_u16().collect());
                let weights1: Option<Vec<[f32; 4]>> = reader
                    .read_weights(1)
                    .map(|weights| weights.into_f32().collect());
                let indices: Vec<u32> = reader
                    .read_indices()
                    .map(|indices| indices.into_u32().collect())
                    .unwrap_or_else(|| (0..positions.len() as u32).collect());

                if indices.is_empty()
                    || indices.len() % 3 != 0
                    || u32::try_from(indices.len()).is_err()
                    || indices
                        .iter()
                        .any(|&index| index as usize >= positions.len())
                {
                    return Err(format!(
                        "mesh '{}' primitive has invalid triangle indices",
                        mesh.name().unwrap_or("unnamed")
                    ));
                }
                if normals
                    .as_ref()
                    .is_some_and(|values| values.len() != positions.len())
                    || source_tangents
                        .as_ref()
                        .is_some_and(|values| values.len() != positions.len())
                    || source_tangents.as_ref().is_some_and(|values| {
                        values.iter().any(|tangent| {
                            glam::Vec3::new(tangent[0], tangent[1], tangent[2]).length_squared()
                                <= f32::EPSILON
                                || tangent[3].abs() < 0.5
                        })
                    })
                    || tex_coords
                        .iter()
                        .flatten()
                        .any(|values| values.len() != positions.len())
                    || colors
                        .as_ref()
                        .is_some_and(|values| values.len() != positions.len())
                    || joints
                        .as_ref()
                        .is_some_and(|values| values.len() != positions.len())
                    || weights
                        .as_ref()
                        .is_some_and(|values| values.len() != positions.len())
                    || joints.is_some() != weights.is_some()
                    || joints1
                        .as_ref()
                        .is_some_and(|values| values.len() != positions.len())
                    || weights1
                        .as_ref()
                        .is_some_and(|values| values.len() != positions.len())
                    || joints1.is_some() != weights1.is_some()
                {
                    return Err(format!(
                        "mesh '{}' primitive has mismatched vertex attributes",
                        mesh.name().unwrap_or("unnamed")
                    ));
                }
                if positions.iter().flatten().any(|value| !value.is_finite())
                    || normals
                        .iter()
                        .flatten()
                        .flatten()
                        .any(|value| !value.is_finite())
                    || source_tangents
                        .iter()
                        .flatten()
                        .flatten()
                        .any(|value| !value.is_finite())
                    || tex_coords
                        .iter()
                        .flatten()
                        .flatten()
                        .flatten()
                        .any(|value| !value.is_finite())
                    || colors
                        .iter()
                        .flatten()
                        .flatten()
                        .any(|value| !value.is_finite())
                    || weights
                        .iter()
                        .flatten()
                        .flatten()
                        .any(|value| !value.is_finite() || *value < 0.0)
                    || weights1
                        .iter()
                        .flatten()
                        .flatten()
                        .any(|value| !value.is_finite() || *value < 0.0)
                {
                    return Err(format!(
                        "mesh '{}' primitive contains non-finite vertex data",
                        mesh.name().unwrap_or("unnamed")
                    ));
                }

                let positions_flat: Vec<f32> = positions.iter().flatten().copied().collect();
                let fallback_normals = calculate_normals(&positions_flat, &indices);
                let vertices: Vec<Vertex> = positions
                    .iter()
                    .enumerate()
                    .map(|(index, position)| {
                        let normal = normals
                            .as_ref()
                            .map(|normals| glam::Vec3::from_array(normals[index]))
                            .filter(|normal| normal.length_squared() > f32::EPSILON)
                            .map_or_else(
                                || fallback_normals[index],
                                |normal| normal.normalize().to_array(),
                            );
                        let color = colors
                            .as_ref()
                            .map(|colors| {
                                let color = colors[index];
                                [color[0], color[1], color[2]]
                            })
                            .unwrap_or([1.0; 3]);

                        Vertex {
                            position: *position,
                            normal,
                            color,
                            tex_coords: tex_coords[0]
                                .as_ref()
                                .map(|coordinates| coordinates[index])
                                .unwrap_or([0.0; 2]),
                        }
                    })
                    .collect();
                let uv_sets: Vec<UvSetsVertex> = (0..positions.len())
                    .map(|index| {
                        let get_uv = |set: usize| {
                            tex_coords[set]
                                .as_ref()
                                .map(|coordinates| coordinates[index])
                                .unwrap_or([0.0; 2])
                        };
                        let uv0 = get_uv(0);
                        let uv1 = get_uv(1);
                        let uv2 = get_uv(2);
                        let uv3 = get_uv(3);
                        UvSetsVertex([
                            [uv0[0], uv0[1], uv1[0], uv1[1]],
                            [uv2[0], uv2[1], uv3[0], uv3[1]],
                        ])
                    })
                    .collect();
                let material_index = primitive
                    .material()
                    .index()
                    .unwrap_or(default_material_index);
                let material = materials.get(material_index).ok_or_else(|| {
                    format!(
                        "mesh '{}' refers to a missing material",
                        mesh.name().unwrap_or("unnamed")
                    )
                })?;
                for (image, tex_coord, texture_name) in [
                    (
                        material.base_color_image,
                        material.base_color_tex_coord,
                        "base-color",
                    ),
                    (
                        material.metallic_roughness_image,
                        material.metallic_roughness_tex_coord,
                        "metallic-roughness",
                    ),
                    (material.normal_image, material.normal_tex_coord, "normal"),
                ] {
                    if image.is_some()
                        && tex_coords
                            .get(tex_coord as usize)
                            .and_then(Option::as_ref)
                            .is_none()
                    {
                        return Err(format!(
                            "mesh '{}' uses material '{}' with a {texture_name} texture on TEXCOORD_{tex_coord}, but the primitive has no matching attribute",
                            mesh.name().unwrap_or("unnamed"),
                            document
                                .materials()
                                .nth(material_index)
                                .and_then(|material| material.name())
                                .unwrap_or("unnamed")
                        ));
                    }
                }
                let tangent_tex_coord = if material.normal_image.is_some() {
                    material.normal_tex_coord
                } else {
                    0
                };
                let tangent_uvs = tex_coords[tangent_tex_coord as usize]
                    .as_deref()
                    .unwrap_or(&[]);
                let tangents = if tangent_tex_coord == 0 {
                    source_tangents.map_or_else(
                        || calculate_tangents(&vertices, &indices),
                        |tangents| {
                            tangents
                                .into_iter()
                                .map(|tangent| {
                                    let direction = glam::Vec3::from_array([
                                        tangent[0], tangent[1], tangent[2],
                                    ])
                                    .normalize_or_zero();
                                    [direction.x, direction.y, direction.z, tangent[3].signum()]
                                })
                                .collect()
                        },
                    )
                } else {
                    calculate_tangents_with_uv(&vertices, tangent_uvs, &indices)
                };
                let skin_vertices = positions
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        let has_skin_data = joints.is_some() || joints1.is_some();
                        let mut influences = Vec::with_capacity(8);
                        if let (Some(joints), Some(weights)) = (&joints, &weights) {
                            influences.extend(
                                joints[index]
                                    .iter()
                                    .zip(weights[index])
                                    .map(|(&joint, weight)| (u32::from(joint), weight)),
                            );
                        }
                        if let (Some(joints), Some(weights)) = (&joints1, &weights1) {
                            influences.extend(
                                joints[index]
                                    .iter()
                                    .zip(weights[index])
                                    .map(|(&joint, weight)| (u32::from(joint), weight)),
                            );
                        }
                        influences.sort_by(|left, right| right.1.total_cmp(&left.1));
                        influences.truncate(4);
                        let sum = influences.iter().map(|(_, weight)| weight).sum::<f32>();
                        if has_skin_data && (!sum.is_finite() || sum <= f32::EPSILON) {
                            return Err(format!(
                                "mesh '{}' has a vertex with no valid skin weights",
                                mesh.name().unwrap_or("unnamed")
                            ));
                        }
                        let mut joint_indices = [0; 4];
                        let mut normalized_weights = [0.0; 4];
                        for (slot, (joint, weight)) in influences.into_iter().enumerate() {
                            joint_indices[slot] = joint;
                            normalized_weights[slot] = weight / sum;
                        }
                        Ok(SkinVertex {
                            joints: joint_indices,
                            weights: normalized_weights,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;

                let primitive_data_index = primitives.len();
                primitive_indices.insert((mesh.index(), primitive_index), primitive_data_index);
                primitives.push(GltfPrimitiveData {
                    mesh: MeshData {
                        vertices,
                        uv_sets,
                        indices,
                        skin_vertices,
                        tangents,
                    },
                    material_index,
                });
            }
        }

        let (nodes, parents) =
            read_nodes(&document).map_err(|error| format!("{}: {error}", path.display()))?;
        let skins = read_skins(&document, &buffers)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let (clips, animation_infos) = read_animations(&document, &buffers)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let scene = document
            .default_scene()
            .or_else(|| document.scenes().next());
        let mut instances = Vec::new();
        let (nodes, parents) = if scene.is_none() {
            let mut nodes = nodes;
            let mut parents = parents;
            let synthetic_node = nodes.len();
            nodes.push(NodePose {
                translation: Vec3::ZERO,
                rotation: Quat::IDENTITY,
                scale: Vec3::ONE,
                matrix: Mat4::IDENTITY,
                animated: false,
            });
            parents.push(None);
            for primitive_index in 0..primitives.len() {
                instances.push(AnimationInstanceData {
                    name: format!("Mesh {}", primitive_index + 1),
                    primitive_index,
                    node_index: synthetic_node,
                    skin_index: None,
                });
            }
            (nodes, parents)
        } else {
            (nodes, parents)
        };
        if let Some(scene) = scene {
            for node in scene.nodes() {
                collect_node_instances(
                    node,
                    &primitive_indices,
                    &skins,
                    &primitives,
                    &mut instances,
                )?;
            }
        }

        if primitives.is_empty() || instances.is_empty() {
            return Err(format!(
                "{}: glTF does not contain any renderable triangle instances",
                path.display()
            ));
        }
        prepare_image_mip_levels(&mut images, &materials)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let globals = calculate_global_transforms(&nodes, &parents)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let mesh_ids = vec![MeshId(0); primitives.len()];
        let material_ids = primitives
            .iter()
            .map(|primitive| MaterialId(primitive.material_index))
            .collect::<Vec<_>>();
        build_instances(&instances, &globals, &skins, &mesh_ids, &material_ids)
            .map_err(|error| format!("{}: {error}", path.display()))?;

        Ok(Self {
            images,
            materials,
            primitives,
            instances,
            nodes,
            parents,
            skins,
            clips,
            animation_infos,
        })
    }

    pub fn into_loaded(
        self,
        first_mesh_id: usize,
        first_material_id: usize,
    ) -> Result<LoadedGltfModel, String> {
        let globals = calculate_global_transforms(&self.nodes, &self.parents)?;
        let mesh_ids = (0..self.primitives.len())
            .map(|index| MeshId(first_mesh_id + index))
            .collect::<Vec<_>>();
        let material_ids = self
            .primitives
            .iter()
            .map(|primitive| MaterialId(first_material_id + primitive.material_index))
            .collect::<Vec<_>>();
        let instances = build_instances(
            &self.instances,
            &globals,
            &self.skins,
            &mesh_ids,
            &material_ids,
        )?;
        let animation_data = if self.clips.is_empty() {
            None
        } else {
            let sample_nodes = self.nodes.clone();
            let node_count = sample_nodes.len();
            Some(GltfAnimationData {
                base_nodes: self.nodes,
                parents: self.parents,
                skins: self.skins,
                instances: self.instances,
                clips: self.clips,
                animation_infos: self.animation_infos.clone(),
                mesh_ids,
                material_ids,
                sample_nodes,
                sample_globals: Vec::with_capacity(node_count),
                sample_computed: Vec::with_capacity(node_count),
                sample_visiting: Vec::with_capacity(node_count),
            })
        };
        Ok(LoadedGltfModel {
            instances,
            animations: self.animation_infos,
            animation_data,
        })
    }
}

fn prepare_image_mip_levels(
    images: &mut [GltfImageData],
    materials: &[GltfMaterialData],
) -> Result<(), String> {
    let mut mipmap_bytes = 0_u64;
    for material in materials {
        if let Some(index) = material.base_color_image {
            let image = images
                .get_mut(index)
                .ok_or_else(|| format!("base color references missing image {index}"))?;
            if image.base_color_mip_levels.is_none() {
                reserve_mipmap_budget(image, &mut mipmap_bytes)?;
                image.base_color_mip_levels =
                    Some(super::material::generate_mip_levels_with_alpha(
                        &image.rgba,
                        image.width,
                        image.height,
                        true,
                        false,
                        true,
                    ));
            }
        }
        if let Some(index) = material.metallic_roughness_image {
            let image = images
                .get_mut(index)
                .ok_or_else(|| format!("metallic-roughness references missing image {index}"))?;
            if image.metallic_roughness_mip_levels.is_none() {
                reserve_mipmap_budget(image, &mut mipmap_bytes)?;
                image.metallic_roughness_mip_levels =
                    Some(super::material::generate_mip_levels_with_alpha(
                        &image.rgba,
                        image.width,
                        image.height,
                        false,
                        false,
                        false,
                    ));
            }
        }
        if let Some(index) = material.normal_image {
            let image = images
                .get_mut(index)
                .ok_or_else(|| format!("normal map references missing image {index}"))?;
            if image.normal_mip_levels.is_none() {
                reserve_mipmap_budget(image, &mut mipmap_bytes)?;
                image.normal_mip_levels = Some(super::material::generate_mip_levels_with_alpha(
                    &image.rgba,
                    image.width,
                    image.height,
                    false,
                    true,
                    false,
                ));
            }
        }
        for index in [
            material.occlusion_image,
            material.specular_image,
            material.transmission_image,
            material.thickness_image,
        ]
        .into_iter()
        .flatten()
        {
            let image = images.get_mut(index).ok_or_else(|| {
                format!("linear material texture references missing image {index}")
            })?;
            if image.data_mip_levels.is_none() {
                reserve_mipmap_budget(image, &mut mipmap_bytes)?;
                image.data_mip_levels = Some(super::material::generate_mip_levels_with_alpha(
                    &image.rgba,
                    image.width,
                    image.height,
                    false,
                    false,
                    false,
                ));
            }
        }
        for index in [material.emissive_image, material.specular_color_image]
            .into_iter()
            .flatten()
        {
            let image = images
                .get_mut(index)
                .ok_or_else(|| format!("sRGB material texture references missing image {index}"))?;
            if image.emissive_mip_levels.is_none() {
                reserve_mipmap_budget(image, &mut mipmap_bytes)?;
                image.emissive_mip_levels = Some(super::material::generate_mip_levels_with_alpha(
                    &image.rgba,
                    image.width,
                    image.height,
                    true,
                    false,
                    false,
                ));
            }
        }
    }
    Ok(())
}

fn reserve_mipmap_budget(image: &GltfImageData, total_bytes: &mut u64) -> Result<(), String> {
    let mut width = image.width as u64;
    let mut height = image.height as u64;
    let mut image_bytes = 0_u64;
    loop {
        image_bytes = image_bytes
            .checked_add(
                width
                    .checked_mul(height)
                    .and_then(|pixels| pixels.checked_mul(4))
                    .ok_or_else(|| "mipmap size overflow".to_owned())?,
            )
            .ok_or_else(|| "mipmap size overflow".to_owned())?;
        if width == 1 && height == 1 {
            break;
        }
        width = (width / 2).max(1);
        height = (height / 2).max(1);
    }

    let next_total = total_bytes
        .checked_add(image_bytes)
        .ok_or_else(|| "mipmap size overflow".to_owned())?;
    if next_total > MAX_IMPORTED_MIPMAP_BYTES {
        return Err(format!(
            "material textures exceed the {} MiB mipmap import budget",
            MAX_IMPORTED_MIPMAP_BYTES / (1024 * 1024)
        ));
    }
    *total_bytes = next_total;
    Ok(())
}

fn is_binary_cutout_texture(image: &GltfImageData) -> bool {
    image.binary_alpha_mask
}

fn has_binary_alpha_mask(rgba: &[u8]) -> bool {
    let mut transparent_pixels = 0;
    let mut opaque_pixels = 0;
    let mut partially_transparent_pixels = 0;

    for alpha in rgba.iter().skip(3).step_by(4) {
        match alpha {
            0 => transparent_pixels += 1,
            255 => opaque_pixels += 1,
            _ => partially_transparent_pixels += 1,
        }
    }

    let pixel_count = transparent_pixels + opaque_pixels + partially_transparent_pixels;
    pixel_count > 0
        && transparent_pixels > 0
        && opaque_pixels > 0
        && partially_transparent_pixels * 100 <= pixel_count
}

fn read_nodes(document: &gltf::Document) -> Result<(Vec<NodePose>, Vec<Option<usize>>), String> {
    let mut nodes = Vec::new();
    let mut parents = vec![None; document.nodes().len()];
    for node in document.nodes() {
        let matrix = Mat4::from_cols_array_2d(&node.transform().matrix());
        if matrix
            .to_cols_array()
            .iter()
            .any(|value| !value.is_finite())
        {
            return Err(format!(
                "node '{}' has a non-finite transform",
                node.name().unwrap_or("unnamed")
            ));
        }
        let (translation, rotation, scale) = node.transform().decomposed();
        let rotation = Quat::from_array(rotation);
        if !translation.iter().all(|value| value.is_finite())
            || !scale.iter().all(|value| value.is_finite())
            || !rotation.is_finite()
            || rotation.length_squared() <= f32::EPSILON
        {
            return Err(format!(
                "node '{}' has an invalid rotation",
                node.name().unwrap_or("unnamed")
            ));
        }
        nodes.push(NodePose {
            translation: Vec3::from_array(translation),
            rotation: rotation.normalize(),
            scale: Vec3::from_array(scale),
            matrix,
            animated: false,
        });
        for child in node.children() {
            let parent = &mut parents[child.index()];
            if parent.replace(node.index()).is_some() {
                return Err(format!(
                    "node '{}' has more than one parent",
                    child.name().unwrap_or("unnamed")
                ));
            }
        }
    }
    Ok((nodes, parents))
}

fn read_skins(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
) -> Result<Vec<SkinData>, String> {
    document
        .skins()
        .map(|skin| {
            let joints = skin.joints().map(|joint| joint.index()).collect::<Vec<_>>();
            if joints.is_empty() {
                return Err(format!(
                    "skin '{}' does not contain any joints",
                    skin.name().unwrap_or("unnamed")
                ));
            }
            let reader = skin.reader(|buffer| {
                buffers
                    .get(buffer.index())
                    .map(|buffer_data| buffer_data.0.as_slice())
            });
            let inverse_bind_matrices = reader
                .read_inverse_bind_matrices()
                .map(|matrices| {
                    matrices
                        .map(|matrix| Mat4::from_cols_array_2d(&matrix))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec![Mat4::IDENTITY; joints.len()]);
            if inverse_bind_matrices.len() != joints.len()
                || inverse_bind_matrices.iter().any(|matrix| {
                    matrix
                        .to_cols_array()
                        .iter()
                        .any(|value| !value.is_finite())
                })
            {
                return Err(format!(
                    "skin '{}' has invalid inverse bind matrices",
                    skin.name().unwrap_or("unnamed")
                ));
            }
            Ok(SkinData {
                joints,
                inverse_bind_matrices,
            })
        })
        .collect()
}

fn read_animations(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
) -> Result<(Vec<AnimationClipData>, Vec<GltfAnimationInfo>), String> {
    use gltf::animation::util::ReadOutputs;

    let mut clips = Vec::new();
    let mut infos = Vec::new();
    for animation in document.animations() {
        let mut channels = Vec::new();
        let mut duration = 0.0_f32;
        for channel in animation.channels() {
            let reader = channel.reader(|buffer| {
                buffers
                    .get(buffer.index())
                    .map(|buffer_data| buffer_data.0.as_slice())
            });
            let times = reader
                .read_inputs()
                .ok_or_else(|| "animation channel is missing input keyframes".to_owned())?
                .collect::<Vec<_>>();
            if times.is_empty()
                || times.iter().any(|time| !time.is_finite() || *time < 0.0)
                || times.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(format!(
                    "animation '{}' has invalid keyframe times",
                    animation.name().unwrap_or("unnamed")
                ));
            }
            duration = duration.max(*times.last().unwrap());
            let path = match channel.target().property() {
                gltf::animation::Property::Translation => AnimationPath::Translation,
                gltf::animation::Property::Rotation => AnimationPath::Rotation,
                gltf::animation::Property::Scale => AnimationPath::Scale,
                gltf::animation::Property::MorphTargetWeights => {
                    return Err(format!(
                        "animation '{}' uses unsupported morph target weights",
                        animation.name().unwrap_or("unnamed")
                    ));
                }
            };
            if channel.target().node().index() >= document.nodes().len() {
                return Err(format!(
                    "animation '{}' targets a missing node",
                    animation.name().unwrap_or("unnamed")
                ));
            }
            let sampler = channel.sampler();
            let interpolation = match sampler.interpolation() {
                gltf::animation::Interpolation::Step => Interpolation::Step,
                gltf::animation::Interpolation::Linear => Interpolation::Linear,
                gltf::animation::Interpolation::CubicSpline => Interpolation::CubicSpline,
            };
            let outputs = reader
                .read_outputs()
                .ok_or_else(|| "animation channel is missing output values".to_owned())?;
            let values = match (path, outputs) {
                (AnimationPath::Translation, ReadOutputs::Translations(values)) => values
                    .map(|value| [value[0], value[1], value[2], 0.0])
                    .collect::<Vec<_>>(),
                (AnimationPath::Scale, ReadOutputs::Scales(values)) => values
                    .map(|value| [value[0], value[1], value[2], 0.0])
                    .collect::<Vec<_>>(),
                (AnimationPath::Rotation, ReadOutputs::Rotations(values)) => {
                    values.into_f32().collect::<Vec<_>>()
                }
                _ => {
                    return Err(format!(
                        "animation '{}' has output data incompatible with its target",
                        animation.name().unwrap_or("unnamed")
                    ));
                }
            };
            let expected_values = if matches!(interpolation, Interpolation::CubicSpline) {
                times.len().checked_mul(3)
            } else {
                Some(times.len())
            };
            if expected_values != Some(values.len())
                || values.iter().flatten().any(|value| !value.is_finite())
                || matches!(path, AnimationPath::Rotation)
                    && (0..times.len()).any(|index| {
                        let value = values[if matches!(interpolation, Interpolation::CubicSpline) {
                            index * 3 + 1
                        } else {
                            index
                        }];
                        Quat::from_array(value).length_squared() <= f32::EPSILON
                    })
            {
                return Err(format!(
                    "animation '{}' has invalid channel output values",
                    animation.name().unwrap_or("unnamed")
                ));
            }
            channels.push(AnimationChannelData {
                node_index: channel.target().node().index(),
                path,
                interpolation,
                times,
                values,
            });
        }
        infos.push(GltfAnimationInfo {
            name: animation.name().unwrap_or("unnamed").to_owned(),
            duration,
        });
        clips.push(AnimationClipData { channels });
    }
    Ok((clips, infos))
}

fn calculate_global_transforms(
    nodes: &[NodePose],
    parents: &[Option<usize>],
) -> Result<Vec<Mat4>, String> {
    let mut globals = Vec::new();
    let mut computed = Vec::new();
    let mut visiting = Vec::new();
    calculate_global_transforms_into(nodes, parents, &mut globals, &mut computed, &mut visiting)?;
    Ok(globals)
}

fn calculate_global_transforms_into(
    nodes: &[NodePose],
    parents: &[Option<usize>],
    globals: &mut Vec<Mat4>,
    computed: &mut Vec<bool>,
    visiting: &mut Vec<bool>,
) -> Result<(), String> {
    fn calculate(
        index: usize,
        nodes: &[NodePose],
        parents: &[Option<usize>],
        globals: &mut [Mat4],
        computed: &mut [bool],
        visiting: &mut [bool],
    ) -> Result<Mat4, String> {
        if computed[index] {
            return Ok(globals[index]);
        }
        if visiting[index] {
            return Err("node hierarchy contains a cycle".to_owned());
        }
        visiting[index] = true;
        let local = nodes[index].matrix();
        let global = if let Some(parent) = parents[index] {
            if parent >= nodes.len() {
                return Err(format!("node {index} references missing parent {parent}"));
            }
            calculate(parent, nodes, parents, globals, computed, visiting)? * local
        } else {
            local
        };
        visiting[index] = false;
        if global
            .to_cols_array()
            .iter()
            .any(|value| !value.is_finite())
        {
            return Err(format!("node {index} has a non-finite world transform"));
        }
        globals[index] = global;
        computed[index] = true;
        Ok(global)
    }

    if nodes.len() != parents.len() {
        return Err("node and parent arrays have different lengths".to_owned());
    }
    globals.resize(nodes.len(), Mat4::IDENTITY);
    computed.resize(nodes.len(), false);
    computed.fill(false);
    visiting.resize(nodes.len(), false);
    visiting.fill(false);
    for index in 0..nodes.len() {
        calculate(index, nodes, parents, globals, computed, visiting)?;
    }
    Ok(())
}

fn build_instances(
    instances: &[AnimationInstanceData],
    globals: &[Mat4],
    skins: &[SkinData],
    mesh_ids: &[MeshId],
    material_ids: &[MaterialId],
) -> Result<Vec<GltfPrimitiveInstance>, String> {
    instances
        .iter()
        .map(|instance| {
            let node_transform = *globals
                .get(instance.node_index)
                .ok_or_else(|| "model instance references a missing node".to_owned())?;
            let skin_matrices = if let Some(skin_index) = instance.skin_index {
                let skin = skins
                    .get(skin_index)
                    .ok_or_else(|| "model instance references a missing skin".to_owned())?;
                let inverse_mesh = node_transform.inverse();
                if !inverse_mesh.is_finite() {
                    return Err("skinned mesh node has a non-invertible transform".to_owned());
                }
                let matrices = skin
                    .joints
                    .iter()
                    .zip(&skin.inverse_bind_matrices)
                    .map(|(&joint, inverse_bind)| {
                        let joint_transform = globals
                            .get(joint)
                            .ok_or_else(|| "skin references a missing joint node".to_owned())?;
                        let matrix = inverse_mesh * *joint_transform * *inverse_bind;
                        if matrix
                            .to_cols_array()
                            .iter()
                            .any(|value| !value.is_finite())
                        {
                            return Err("skin contains a non-finite joint matrix".to_owned());
                        }
                        Ok(matrix)
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                matrices
            } else {
                Vec::new()
            };
            let mesh_id = *mesh_ids
                .get(instance.primitive_index)
                .ok_or_else(|| "model instance references a missing primitive".to_owned())?;
            let material_id = *material_ids
                .get(instance.primitive_index)
                .ok_or_else(|| "model primitive references a missing material".to_owned())?;
            Ok(GltfPrimitiveInstance {
                name: instance.name.clone(),
                mesh_id,
                material_id,
                node_transform,
                skin_matrices,
            })
        })
        .collect()
}

fn collect_node_instances(
    node: gltf::Node<'_>,
    primitive_indices: &HashMap<(usize, usize), usize>,
    skins: &[SkinData],
    primitives: &[GltfPrimitiveData],
    instances: &mut Vec<AnimationInstanceData>,
) -> Result<(), String> {
    let node_index = node.index();
    let skin_index = node.skin().map(|skin| skin.index());
    if let Some(skin_index) = skin_index {
        let skin = skins.get(skin_index).ok_or_else(|| {
            format!(
                "node '{}' references a missing skin",
                node.name().unwrap_or("unnamed")
            )
        })?;
        if skin.joints.is_empty() {
            return Err(format!("skin {skin_index} does not contain any joints"));
        }
    }
    if let Some(mesh) = node.mesh() {
        for (primitive_index, _) in mesh.primitives().enumerate() {
            let primitive_data_index = primitive_indices
                .get(&(mesh.index(), primitive_index))
                .copied()
                .ok_or_else(|| format!("mesh '{}' primitive was not imported", mesh.index()))?;
            if let Some(skin_index) = skin_index {
                let skin_vertex_data = &primitives[primitive_data_index].mesh.skin_vertices;
                if skin_vertex_data.iter().any(|vertex| {
                    vertex.weights.iter().any(|&weight| weight > 0.0)
                        && vertex
                            .joints
                            .iter()
                            .zip(vertex.weights)
                            .any(|(&joint, weight)| {
                                weight > 0.0 && joint as usize >= skins[skin_index].joints.len()
                            })
                }) || skin_vertex_data
                    .iter()
                    .all(|vertex| vertex.weights.iter().all(|&weight| weight == 0.0))
                {
                    return Err(format!(
                        "node '{}' has skinning data incompatible with skin {skin_index}",
                        node.name().unwrap_or("unnamed")
                    ));
                }
            }
            instances.push(AnimationInstanceData {
                name: instance_name(&node, &mesh, primitive_index),
                primitive_index: primitive_data_index,
                node_index,
                skin_index,
            });
        }
    }

    for child in node.children() {
        collect_node_instances(child, primitive_indices, skins, primitives, instances)?;
    }
    Ok(())
}

fn convert_image(image: &gltf::image::Data) -> Result<GltfImageData, String> {
    use gltf::image::Format;

    let (channels, bytes_per_channel) = match image.format {
        Format::R8 => (1, 1),
        Format::R8G8 => (2, 1),
        Format::R8G8B8 => (3, 1),
        Format::R8G8B8A8 => (4, 1),
        Format::R16 => (1, 2),
        Format::R16G16 => (2, 2),
        Format::R16G16B16 => (3, 2),
        Format::R16G16B16A16 => (4, 2),
        Format::R32G32B32FLOAT => (3, 4),
        Format::R32G32B32A32FLOAT => (4, 4),
    };
    let pixel_count = (image.width as usize)
        .checked_mul(image.height as usize)
        .ok_or_else(|| "image dimensions overflow".to_owned())?;
    let expected_len = pixel_count
        .checked_mul(channels * bytes_per_channel)
        .ok_or_else(|| "image byte count overflow".to_owned())?;
    if image.width == 0 || image.height == 0 || image.pixels.len() != expected_len {
        return Err("image has invalid dimensions or pixel data".to_owned());
    }

    let rgba_len = pixel_count
        .checked_mul(4)
        .ok_or_else(|| "converted image byte count overflow".to_owned())?;
    let mut rgba = Vec::with_capacity(rgba_len);
    for pixel in image.pixels.chunks_exact(channels * bytes_per_channel) {
        let mut values = [0.0; 4];
        for channel in 0..channels {
            let offset = channel * bytes_per_channel;
            values[channel] = match bytes_per_channel {
                1 => pixel[offset] as f32 / 255.0,
                2 => {
                    u16::from_le_bytes([pixel[offset], pixel[offset + 1]]) as f32 / u16::MAX as f32
                }
                4 => {
                    let value = f32::from_le_bytes([
                        pixel[offset],
                        pixel[offset + 1],
                        pixel[offset + 2],
                        pixel[offset + 3],
                    ]);
                    if !value.is_finite() {
                        return Err("image contains non-finite pixel data".to_owned());
                    }
                    value.clamp(0.0, 1.0)
                }
                _ => unreachable!(),
            };
        }

        let color = match channels {
            1 => [values[0], values[0], values[0], 1.0],
            2 => [values[0], values[0], values[0], values[1]],
            3 => [values[0], values[1], values[2], 1.0],
            4 => values,
            _ => unreachable!(),
        };
        rgba.extend(color.map(|value| (value * 255.0).round() as u8));
    }

    let mut width = image.width;
    let mut height = image.height;
    let binary_alpha_mask = has_binary_alpha_mask(&rgba);
    if width.max(height) > MAX_IMPORTED_TEXTURE_DIMENSION {
        let scale = MAX_IMPORTED_TEXTURE_DIMENSION as f32 / width.max(height) as f32;
        width = ((width as f32 * scale).round() as u32).max(1);
        height = ((height as f32 * scale).round() as u32).max(1);
        let rgba_image = image::RgbaImage::from_raw(image.width, image.height, rgba)
            .ok_or_else(|| "converted image has invalid RGBA data".to_owned())?;
        rgba = image::imageops::resize(
            &rgba_image,
            width,
            height,
            image::imageops::FilterType::Triangle,
        )
        .into_raw();
    }

    Ok(GltfImageData {
        width,
        height,
        rgba,
        binary_alpha_mask,
        base_color_mip_levels: None,
        metallic_roughness_mip_levels: None,
        normal_mip_levels: None,
        data_mip_levels: None,
        emissive_mip_levels: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_sample_scene_with_multiple_primitives_and_materials() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("models")
            .join("sample.gltf");
        let model = GltfModelData::load(path).unwrap();

        assert_eq!(model.primitives.len(), 2);
        assert_eq!(model.instances.len(), 2);
        assert_eq!(
            model.instances[0].name,
            "two_material_triangles - primitive 1"
        );
        assert_eq!(
            model.instances[1].name,
            "two_material_triangles - primitive 2"
        );
        assert_eq!(model.materials.len(), 3);
        assert!(model.materials[0].unlit);
        assert!(!model.materials[1].unlit);
        assert_eq!(model.materials[0].alpha_mode, 1);
        assert_eq!(model.materials[0].alpha_cutoff, 0.4);
        assert_eq!(model.materials[1].alpha_mode, 2);
        assert!(
            model
                .materials
                .iter()
                .take(2)
                .all(|material| material.base_color_image == Some(0))
        );
        assert_eq!(model.primitives[0].mesh.indices, [0, 1, 2]);
        assert_eq!(model.primitives[1].mesh.indices, [3, 4, 5]);
        let globals = calculate_global_transforms(&model.nodes, &model.parents).unwrap();
        assert_eq!(
            globals[model.instances[0].node_index].transform_point3(glam::Vec3::ZERO),
            glam::Vec3::new(0.2, 0.0, 0.0)
        );
    }

    #[test]
    fn imports_normal_texture_and_scale_from_gltf_material() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sample_path = manifest_dir
            .join("assets")
            .join("models")
            .join("sample.gltf");
        let sample = std::fs::read_to_string(sample_path)
            .unwrap()
            .replace("\r\n", "\n");
        let with_normal_map = sample.replacen(
            "\"roughnessFactor\": 0.9\n      }\n    },",
            "\"roughnessFactor\": 0.9\n      },\n      \"normalTexture\": {\n        \"index\": 0,\n        \"scale\": 0.35\n      }\n    },",
            1,
        );
        assert_ne!(with_normal_map, sample);

        let fixture_dir = std::env::temp_dir().join(format!(
            "my_engine-normal-map-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&fixture_dir).unwrap();
        std::fs::copy(
            manifest_dir.join("texture.png"),
            fixture_dir.join("normal.png"),
        )
        .unwrap();
        let fixture_path = fixture_dir.join("normal-map.gltf");
        let fixture = with_normal_map.replace("../../texture.png", "normal.png");
        std::fs::write(&fixture_path, fixture).unwrap();

        let loaded = GltfModelData::load(&fixture_path);
        std::fs::remove_dir_all(&fixture_dir).unwrap();
        let model = loaded.unwrap();

        assert_eq!(model.materials[0].normal_image, Some(0));
        assert_eq!(model.materials[0].normal_scale, 0.35);
    }

    #[test]
    fn imports_specular_glossiness_material() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sample_path = manifest_dir
            .join("assets")
            .join("models")
            .join("sample.gltf");
        let sample = std::fs::read_to_string(sample_path)
            .unwrap()
            .replace("\r\n", "\n");
        let with_extension = sample
            .replace(
                "\"extensionsUsed\": [\"KHR_materials_unlit\"]",
                "\"extensionsUsed\": [\"KHR_materials_unlit\", \"KHR_materials_pbrSpecularGlossiness\"]",
            )
            .replace(
                "\"extensionsUsed\": [\"KHR_materials_unlit\", \"KHR_materials_pbrSpecularGlossiness\"],",
                "\"extensionsUsed\": [\"KHR_materials_unlit\", \"KHR_materials_pbrSpecularGlossiness\"],\n  \"extensionsRequired\": [\"KHR_materials_pbrSpecularGlossiness\"],",
            )
            .replace(
                "\"name\": \"cool\",\n      \"alphaMode\": \"BLEND\",",
                "\"name\": \"cool\",\n      \"alphaMode\": \"BLEND\",\n      \"extensions\": {\n        \"KHR_materials_pbrSpecularGlossiness\": {\n          \"diffuseFactor\": [0.2, 0.3, 0.4, 0.5],\n          \"diffuseTexture\": {\"index\": 0},\n          \"specularFactor\": [0.6, 0.7, 0.8],\n          \"glossinessFactor\": 0.25,\n          \"specularGlossinessTexture\": {\"index\": 0, \"texCoord\": 2}\n        }\n      },",
            );
        assert_ne!(with_extension, sample);

        let fixture_dir = std::env::temp_dir().join(format!(
            "my_engine-specular-glossiness-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&fixture_dir).unwrap();
        std::fs::copy(
            manifest_dir.join("texture.png"),
            fixture_dir.join("texture.png"),
        )
        .unwrap();
        let fixture_path = fixture_dir.join("specular-glossiness.gltf");
        let fixture = with_extension.replace("../../texture.png", "texture.png");
        std::fs::write(&fixture_path, fixture).unwrap();

        let loaded = GltfModelData::load(&fixture_path);
        std::fs::remove_dir_all(&fixture_dir).unwrap();
        let model = loaded.unwrap();
        let material = &model.materials[1];

        assert!(material.specular_glossiness);
        assert_eq!(material.base_color_factor, [0.2, 0.3, 0.4, 0.5]);
        assert_eq!(material.base_color_image, Some(0));
        assert_eq!(material.roughness_factor, 0.75);
        assert_eq!(material.metallic_factor, 0.0);
        assert_eq!(material.specular_color_factor, [0.6, 0.7, 0.8]);
        assert_eq!(material.specular_color_image, Some(0));
        assert_eq!(material.specular_color_tex_coord, 2);
    }

    #[test]
    fn imports_transmission_and_specular_material_factors() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sample_path = manifest_dir
            .join("assets")
            .join("models")
            .join("sample.gltf");
        let sample = std::fs::read_to_string(sample_path)
            .unwrap()
            .replace("\r\n", "\n");
        let with_extensions = sample
            .replace(
                "\"extensionsUsed\": [\"KHR_materials_unlit\"]",
                "\"extensionsUsed\": [\"KHR_materials_unlit\", \"KHR_materials_ior\", \"KHR_materials_transmission\", \"KHR_materials_volume\", \"KHR_materials_specular\", \"KHR_materials_emissive_strength\", \"KHR_texture_transform\"]",
            )
            .replace(
                "\"name\": \"cool\",\n      \"alphaMode\": \"BLEND\",",
                "\"name\": \"cool\",\n      \"alphaMode\": \"BLEND\",\n      \"doubleSided\": true,\n      \"emissiveFactor\": [0.2, 0.3, 0.4],\n      \"emissiveTexture\": {\"index\": 0, \"extensions\": {\"KHR_texture_transform\": {\"offset\": [0.25, 0.5], \"rotation\": 1.5707963, \"scale\": [2.0, 3.0], \"texCoord\": 2}}},\n      \"occlusionTexture\": {\"index\": 0, \"strength\": 0.4, \"texCoord\": 1},\n      \"extensions\": {\n        \"KHR_materials_ior\": {\"ior\": 1.33},\n        \"KHR_materials_emissive_strength\": {\"emissiveStrength\": 2.5},\n        \"KHR_materials_transmission\": {\"transmissionFactor\": 0.8, \"transmissionTexture\": {\"index\": 0}},\n        \"KHR_materials_volume\": {\"thicknessFactor\": 0.75, \"thicknessTexture\": {\"index\": 0}, \"attenuationDistance\": 1.5, \"attenuationColor\": [0.8, 0.7, 0.6]},\n        \"KHR_materials_specular\": {\"specularFactor\": 0.5, \"specularColorFactor\": [0.8, 0.9, 1.0], \"specularTexture\": {\"index\": 0}, \"specularColorTexture\": {\"index\": 0}}\n      },",
            );
        assert_ne!(with_extensions, sample);

        let fixture_dir = std::env::temp_dir().join(format!(
            "my_engine-transmission-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&fixture_dir).unwrap();
        std::fs::copy(
            manifest_dir.join("texture.png"),
            fixture_dir.join("texture.png"),
        )
        .unwrap();
        let fixture_path = fixture_dir.join("transmission.gltf");
        let fixture = with_extensions.replace("../../texture.png", "texture.png");
        std::fs::write(&fixture_path, fixture).unwrap();

        let loaded = GltfModelData::load(&fixture_path);
        std::fs::remove_dir_all(&fixture_dir).unwrap();
        let model = loaded.unwrap();

        assert_eq!(model.materials[1].transmission_factor, 0.8);
        assert_eq!(model.materials[1].ior, 1.33);
        assert_eq!(model.materials[1].specular_factor, 0.5);
        assert_eq!(model.materials[1].specular_color_factor, [0.8, 0.9, 1.0]);
        assert!(model.materials[1].double_sided);
        assert_eq!(model.materials[1].emissive_factor, [0.2, 0.3, 0.4]);
        assert_eq!(model.materials[1].emissive_strength, 2.5);
        assert_eq!(model.materials[1].occlusion_strength, 0.4);
        assert_eq!(model.materials[1].occlusion_image, Some(0));
        assert_eq!(model.materials[1].emissive_image, Some(0));
        assert_eq!(model.materials[1].specular_image, Some(0));
        assert_eq!(model.materials[1].specular_color_image, Some(0));
        assert_eq!(model.materials[1].transmission_image, Some(0));
        assert_eq!(model.materials[1].thickness_factor, 0.75);
        assert_eq!(model.materials[1].thickness_image, Some(0));
        assert_eq!(model.materials[1].attenuation_distance, 1.5);
        assert_eq!(model.materials[1].attenuation_color, [0.8, 0.7, 0.6]);
        assert_eq!(model.materials[1].emissive_tex_coord, 2);
        assert!((model.materials[1].emissive_transform[0][0]).abs() < 1e-5);
        assert!((model.materials[1].emissive_transform[0][1] + 3.0).abs() < 1e-5);
        assert_eq!(model.materials[1].emissive_transform[0][2], 0.25);
        assert_eq!(model.materials[1].emissive_transform[1][2], 0.5);
    }

    #[test]
    fn imports_multiple_uv_sets_selected_by_material_textures() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sample_path = manifest_dir
            .join("assets")
            .join("models")
            .join("sample.gltf");
        let sample = std::fs::read_to_string(sample_path)
            .unwrap()
            .replace("\r\n", "\n");
        let with_uv_sets = sample
            .replace(
                "\"TEXCOORD_0\": 2",
                "\"TEXCOORD_0\": 2,\n            \"TEXCOORD_1\": 2,\n            \"TEXCOORD_2\": 2",
            )
            .replace("\"name\": \"cool\",", "\"name\": \"cool\",\n      \"normalTexture\": {\"index\": 0, \"texCoord\": 2},")
            .replace(
                "\"metallicFactor\": 0.2,",
                "\"metallicRoughnessTexture\": {\"index\": 0, \"texCoord\": 1},\n        \"metallicFactor\": 0.2,",
            );
        assert_ne!(with_uv_sets, sample);

        let fixture_dir = std::env::temp_dir().join(format!(
            "my_engine-uv-sets-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&fixture_dir).unwrap();
        std::fs::copy(
            manifest_dir.join("texture.png"),
            fixture_dir.join("texture.png"),
        )
        .unwrap();
        let fixture_path = fixture_dir.join("uv-sets.gltf");
        let fixture = with_uv_sets.replace("../../texture.png", "texture.png");
        std::fs::write(&fixture_path, fixture).unwrap();

        let loaded = GltfModelData::load(&fixture_path);
        std::fs::remove_dir_all(&fixture_dir).unwrap();
        let model = loaded.unwrap();

        assert_eq!(model.materials[1].metallic_roughness_tex_coord, 1);
        assert_eq!(model.materials[1].normal_tex_coord, 2);
        let primitive = &model.primitives[1].mesh;
        assert_eq!(primitive.uv_sets.len(), primitive.vertices.len());
        let uv0 = primitive.vertices[0].tex_coords;
        assert_eq!(primitive.uv_sets[0].0[0][..2], uv0);
        assert_eq!(primitive.uv_sets[0].0[0][2..], uv0);
        assert_eq!(primitive.uv_sets[0].0[1][..2], uv0);
    }

    #[test]
    fn imports_skeleton_and_samples_animation_from_glb() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("models")
            .join("Verity.glb");
        let model = GltfModelData::load(path).unwrap();

        assert_eq!(model.skins.len(), 1);
        assert_eq!(model.skins[0].joints.len(), 33);
        assert_eq!(model.clips.len(), 1);
        assert_eq!(model.clips[0].channels.len(), 99);
        assert_eq!(model.materials[0].alpha_mode, 1);
        assert!(
            model
                .instances
                .iter()
                .any(|instance| instance.skin_index.is_some())
        );
        assert!(model.primitives.iter().any(|primitive| {
            primitive
                .mesh
                .skin_vertices
                .iter()
                .any(|vertex| vertex.weights.iter().any(|&weight| weight > 0.0))
        }));

        let mut loaded = model.into_loaded(10, 5).unwrap();
        assert_eq!(loaded.instances[0].mesh_id, MeshId(10));
        assert_eq!(loaded.animations.len(), 1);
        assert_eq!(
            loaded
                .instances
                .iter()
                .find(|instance| !instance.skin_matrices.is_empty())
                .unwrap()
                .skin_matrices
                .len(),
            33
        );
        let animated = loaded
            .sample_animation(0, loaded.animations[0].duration * 0.5)
            .unwrap();
        assert_eq!(animated.len(), loaded.instances.len());
        assert!(
            animated
                .iter()
                .zip(&loaded.instances)
                .any(|(animated, bind_pose)| animated.skin_matrices != bind_pose.skin_matrices)
        );

        let mut reused = Vec::new();
        loaded
            .sample_animation_into(0, loaded.animations[0].duration * 0.5, &mut reused)
            .unwrap();
        assert_eq!(reused.len(), animated.len());
        for (sampled, expected) in reused.iter().zip(&animated) {
            assert_eq!(sampled.mesh_id, expected.mesh_id);
            assert_eq!(sampled.material_id, expected.material_id);
            assert_eq!(sampled.node_transform, expected.node_transform);
            assert_eq!(sampled.skin_matrices, expected.skin_matrices);
        }
        let capacities = reused
            .iter()
            .map(|instance| instance.skin_matrices.capacity())
            .collect::<Vec<_>>();
        let output_capacity = reused.capacity();
        loaded
            .sample_animation_into(0, loaded.animations[0].duration * 0.75, &mut reused)
            .unwrap();
        assert_eq!(
            reused
                .iter()
                .map(|instance| instance.skin_matrices.capacity())
                .collect::<Vec<_>>(),
            capacities
        );
        assert_eq!(reused.capacity(), output_capacity);
    }

    #[test]
    fn samples_step_linear_and_cubic_animation_channels() {
        let channel = |interpolation, values| AnimationChannelData {
            node_index: 0,
            path: AnimationPath::Translation,
            interpolation,
            times: vec![0.0, 1.0],
            values,
        };
        let step = channel(Interpolation::Step, vec![[0.0; 4], [2.0, 0.0, 0.0, 0.0]]);
        let linear = channel(Interpolation::Linear, vec![[0.0; 4], [2.0, 0.0, 0.0, 0.0]]);
        let cubic = channel(
            Interpolation::CubicSpline,
            vec![
                [0.0; 4],
                [0.0; 4],
                [0.0; 4],
                [0.0; 4],
                [1.0, 0.0, 0.0, 0.0],
                [0.0; 4],
            ],
        );

        assert_eq!(sample_channel(&step, 0.75)[0], 0.0);
        assert_eq!(sample_channel(&linear, 0.25)[0], 0.5);
        assert_eq!(sample_channel(&cubic, 0.5)[0], 0.5);
    }

    #[test]
    fn expands_single_channel_images_to_rgba() {
        let image = gltf::image::Data {
            pixels: vec![128, 64],
            format: gltf::image::Format::R8G8,
            width: 1,
            height: 1,
        };
        let converted = convert_image(&image).unwrap();

        assert_eq!(converted.rgba, [128, 128, 128, 64]);
    }

    #[test]
    fn downscales_large_imported_images_to_the_texture_budget() {
        let image = gltf::image::Data {
            pixels: vec![128; 2048],
            format: gltf::image::Format::R8,
            width: 2048,
            height: 1,
        };
        let converted = convert_image(&image).unwrap();

        assert_eq!(converted.width, MAX_IMPORTED_TEXTURE_DIMENSION);
        assert_eq!(converted.height, 1);
        assert_eq!(
            converted.rgba.len(),
            (MAX_IMPORTED_TEXTURE_DIMENSION * 4) as usize
        );
    }

    #[test]
    fn prepares_mipmaps_only_for_material_images() {
        let mut images = vec![
            GltfImageData {
                width: 2,
                height: 2,
                rgba: vec![255; 16],
                binary_alpha_mask: false,
                base_color_mip_levels: None,
                metallic_roughness_mip_levels: None,
                normal_mip_levels: None,
                data_mip_levels: None,
                emissive_mip_levels: None,
            },
            GltfImageData {
                width: 2,
                height: 2,
                rgba: vec![128; 16],
                binary_alpha_mask: false,
                base_color_mip_levels: None,
                metallic_roughness_mip_levels: None,
                normal_mip_levels: None,
                data_mip_levels: None,
                emissive_mip_levels: None,
            },
        ];
        let materials = [GltfMaterialData {
            base_color_factor: [1.0; 4],
            roughness_factor: 1.0,
            metallic_factor: 0.0,
            specular_glossiness: false,
            unlit: false,
            alpha_mode: 0,
            alpha_cutoff: 0.5,
            double_sided: false,
            transmission_factor: 0.0,
            ior: 1.5,
            specular_factor: 1.0,
            specular_color_factor: [1.0; 3],
            emissive_factor: [0.0; 3],
            emissive_strength: 1.0,
            occlusion_strength: 1.0,
            base_color_image: Some(0),
            base_color_tex_coord: 0,
            base_color_transform: IDENTITY_UV_TRANSFORM,
            metallic_roughness_image: None,
            metallic_roughness_tex_coord: 0,
            metallic_roughness_transform: IDENTITY_UV_TRANSFORM,
            normal_image: None,
            normal_tex_coord: 0,
            normal_scale: 1.0,
            normal_transform: IDENTITY_UV_TRANSFORM,
            occlusion_image: Some(1),
            occlusion_tex_coord: 0,
            occlusion_transform: IDENTITY_UV_TRANSFORM,
            emissive_image: Some(1),
            emissive_tex_coord: 0,
            emissive_transform: IDENTITY_UV_TRANSFORM,
            specular_image: Some(1),
            specular_tex_coord: 0,
            specular_transform: IDENTITY_UV_TRANSFORM,
            specular_color_image: None,
            specular_color_tex_coord: 0,
            specular_color_transform: IDENTITY_UV_TRANSFORM,
            transmission_image: Some(1),
            transmission_tex_coord: 0,
            transmission_transform: IDENTITY_UV_TRANSFORM,
            thickness_factor: 1.0,
            thickness_image: Some(1),
            thickness_tex_coord: 0,
            thickness_transform: IDENTITY_UV_TRANSFORM,
            attenuation_distance: 0.0,
            attenuation_color: [1.0; 3],
        }];

        prepare_image_mip_levels(&mut images, &materials).unwrap();

        assert_eq!(images[0].base_color_mip_levels.as_ref().unwrap().len(), 2);
        assert!(images[0].metallic_roughness_mip_levels.is_none());
        assert!(images[0].normal_mip_levels.is_none());
        assert!(images[0].data_mip_levels.is_none());
        assert!(images[0].emissive_mip_levels.is_none());
        assert_eq!(images[1].data_mip_levels.as_ref().unwrap().len(), 2);
        assert_eq!(images[1].emissive_mip_levels.as_ref().unwrap().len(), 2);
        assert!(images[1].base_color_mip_levels.is_none());
    }

    #[test]
    fn rejects_texture_mipmaps_above_the_memory_budget() {
        let image = GltfImageData {
            width: 8192,
            height: 8192,
            rgba: Vec::new(),
            binary_alpha_mask: false,
            base_color_mip_levels: None,
            metallic_roughness_mip_levels: None,
            normal_mip_levels: None,
            data_mip_levels: None,
            emissive_mip_levels: None,
        };
        let mut total_bytes = 0;

        assert!(reserve_mipmap_budget(&image, &mut total_bytes).is_err());
        assert_eq!(total_bytes, 0);
    }

    #[test]
    fn binary_transparency_is_detected_as_a_cutout_mask() {
        let binary_alpha = GltfImageData {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 0, 0, 255, 0, 255],
            binary_alpha_mask: true,
            base_color_mip_levels: None,
            metallic_roughness_mip_levels: None,
            normal_mip_levels: None,
            data_mip_levels: None,
            emissive_mip_levels: None,
        };
        assert!(is_binary_cutout_texture(&binary_alpha));

        let smooth_alpha = GltfImageData {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 128, 0, 255, 0, 255],
            binary_alpha_mask: false,
            ..binary_alpha
        };

        assert!(!is_binary_cutout_texture(&smooth_alpha));
    }
}
