use std::{
    collections::HashMap,
    error::Error,
    fmt,
    path::{Path, PathBuf},
};

use glam::Vec3;
use wgpu::util::DeviceExt;

pub(crate) const MAX_IMPORTED_TEXTURE_DIMENSION: u32 = 4096;

#[derive(Debug)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub uv_sets: Vec<UvSetsVertex>,
    pub indices: Vec<u32>,
    pub skin_vertices: Vec<SkinVertex>,
    pub tangents: Vec<[f32; 4]>,
}

#[derive(Debug)]
pub struct ObjTextureData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug)]
pub struct ObjMaterialData {
    pub name: String,
    pub diffuse: [f32; 3],
    pub specular: [f32; 3],
    pub shininess: f32,
    pub dissolve: f32,
    pub diffuse_texture: Option<usize>,
    pub ambient_texture: Option<usize>,
    pub normal_texture: Option<usize>,
}

#[derive(Debug)]
pub struct ObjPrimitiveData {
    pub name: String,
    pub mesh: MeshData,
    pub material_index: Option<usize>,
}

#[derive(Debug)]
pub struct ObjModelData {
    pub materials: Vec<ObjMaterialData>,
    pub textures: Vec<ObjTextureData>,
    pub primitives: Vec<ObjPrimitiveData>,
}

impl ObjModelData {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ObjImportError> {
        let path = path.as_ref();
        let (models, materials) =
            tobj::load_obj(path, &tobj::GPU_LOAD_OPTIONS).map_err(ObjImportError::Load)?;
        let materials = materials.map_err(ObjImportError::Load)?;
        let texture_directory = path.parent().unwrap_or_else(|| Path::new("."));
        let mut textures = Vec::new();
        let mut texture_cache = HashMap::new();
        let mut loaded_materials = Vec::with_capacity(materials.len());

        for material in materials {
            let load_texture = |filename: Option<String>,
                                cache: &mut HashMap<PathBuf, usize>,
                                textures: &mut Vec<ObjTextureData>|
             -> Result<Option<usize>, ObjImportError> {
                let Some(filename) = filename else {
                    return Ok(None);
                };
                let Some(filename) = obj_texture_filename(&filename) else {
                    return Ok(None);
                };
                let texture_path = PathBuf::from(filename);
                let texture_path = if texture_path.is_absolute() {
                    texture_path
                } else {
                    texture_directory.join(texture_path)
                };
                let texture_path = std::fs::canonicalize(&texture_path).map_err(|error| {
                    ObjImportError::Invalid(format!(
                        "material '{}' texture {} could not be found: {error}",
                        material.name,
                        texture_path.display()
                    ))
                })?;
                if let Some(&index) = cache.get(&texture_path) {
                    return Ok(Some(index));
                }

                let image = image::open(&texture_path).map_err(|error| {
                    ObjImportError::Invalid(format!(
                        "material '{}' texture {} could not be decoded: {error}",
                        material.name,
                        texture_path.display()
                    ))
                })?;
                let image = downscale_obj_texture(image);
                let image = image.to_rgba8();
                let image = image::imageops::flip_vertical(&image);
                let index = textures.len();
                textures.push(ObjTextureData {
                    width: image.width(),
                    height: image.height(),
                    rgba: image.into_raw(),
                });
                cache.insert(texture_path, index);
                Ok(Some(index))
            };

            let diffuse_texture = load_texture(
                material.diffuse_texture.clone(),
                &mut texture_cache,
                &mut textures,
            )?;
            let ambient_texture = if let Some(filename) = material.ambient_texture.clone() {
                load_texture(Some(filename), &mut texture_cache, &mut textures)?
            } else {
                let inferred_filename = material
                    .diffuse_texture
                    .as_deref()
                    .and_then(obj_texture_filename)
                    .and_then(|filename| {
                        inferred_obj_occlusion_filename(filename, texture_directory)
                    });
                if let Some(filename) = inferred_filename {
                    load_texture(Some(filename), &mut texture_cache, &mut textures)?
                } else {
                    None
                }
            };
            let normal_texture =
                load_texture(material.normal_texture, &mut texture_cache, &mut textures)?;

            loaded_materials.push(ObjMaterialData {
                name: material.name.clone(),
                diffuse: material.diffuse.unwrap_or([1.0; 3]),
                specular: material.specular.unwrap_or([1.0; 3]),
                shininess: material.shininess.unwrap_or(32.0),
                dissolve: material.dissolve.unwrap_or(1.0),
                diffuse_texture,
                ambient_texture,
                normal_texture,
            });
        }

        let mut primitives = Vec::with_capacity(models.len());
        for model in &models {
            if model
                .mesh
                .material_id
                .is_some_and(|index| index >= loaded_materials.len())
            {
                return Err(ObjImportError::Invalid(format!(
                    "model '{}' references an unknown MTL material",
                    model.name
                )));
            }
            primitives.push(ObjPrimitiveData {
                name: model.name.clone(),
                mesh: MeshData::from_obj_models(std::slice::from_ref(model))?,
                material_index: model.mesh.material_id,
            });
        }

        if primitives.is_empty() {
            return Err(ObjImportError::Invalid(
                "file does not contain any triangle meshes".to_owned(),
            ));
        }

        Ok(Self {
            materials: loaded_materials,
            textures,
            primitives,
        })
    }

    pub fn primitive_count(&self) -> usize {
        self.primitives.len()
    }
}

fn obj_texture_filename(value: &str) -> Option<String> {
    let tokens = value.split_whitespace().collect::<Vec<_>>();
    let mut index = 0;
    while index < tokens.len() && tokens[index].starts_with('-') {
        let option = tokens[index];
        index += 1;
        match option {
            "-s" | "-o" | "-t" => {
                let mut consumed = 0;
                while consumed < 3
                    && tokens
                        .get(index)
                        .is_some_and(|token| token.parse::<f32>().is_ok())
                {
                    index += 1;
                    consumed += 1;
                }
            }
            "-mm" => {
                for _ in 0..2 {
                    if tokens
                        .get(index)
                        .is_some_and(|token| token.parse::<f32>().is_ok())
                    {
                        index += 1;
                    }
                }
            }
            "-bm" | "-boost" | "-texres" | "-imfchan" | "-type" | "-clamp" | "-blendu"
            | "-blendv" | "-cc" | "-colorspace" => {
                if index < tokens.len() {
                    index += 1;
                }
            }
            "-halo" => {}
            _ => return Some(value.trim().trim_matches('"').to_owned()),
        }
    }
    let filename = tokens[index..].join(" ");
    let filename = filename.trim().trim_matches('"').to_owned();
    (!filename.is_empty()).then_some(filename)
}

fn inferred_obj_occlusion_filename(
    diffuse_filename: String,
    texture_directory: &Path,
) -> Option<String> {
    let path = Path::new(&diffuse_filename);
    let stem = path.file_stem()?.to_str()?;
    let extension = path.extension()?.to_str()?;
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    ["_occlusion", "_ao"].into_iter().find_map(|suffix| {
        let filename = parent.join(format!("{stem}{suffix}.{extension}"));
        texture_directory
            .join(&filename)
            .is_file()
            .then(|| filename.to_string_lossy().into_owned())
    })
}

fn downscale_obj_texture(image: image::DynamicImage) -> image::DynamicImage {
    if image.width() > MAX_IMPORTED_TEXTURE_DIMENSION
        || image.height() > MAX_IMPORTED_TEXTURE_DIMENSION
    {
        image.thumbnail(
            MAX_IMPORTED_TEXTURE_DIMENSION,
            MAX_IMPORTED_TEXTURE_DIMENSION,
        )
    } else {
        image
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct UvSetsVertex(pub [[f32; 4]; 2]);

impl UvSetsVertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![3 => Float32x4, 15 => Float32x4];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkinVertex {
    pub joints: [u32; 4],
    pub weights: [f32; 4],
}

impl SkinVertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![4 => Uint32x4, 5 => Float32x4];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

#[derive(Debug)]
pub enum ObjImportError {
    Load(tobj::LoadError),
    Invalid(String),
}

impl fmt::Display for ObjImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(error) => write!(f, "failed to load OBJ: {error}"),
            Self::Invalid(message) => write!(f, "invalid OBJ mesh: {message}"),
        }
    }
}

impl Error for ObjImportError {}

impl MeshData {
    pub fn load_obj(path: impl AsRef<Path>) -> Result<Self, ObjImportError> {
        let path = path.as_ref();
        let (models, materials) =
            tobj::load_obj(path, &tobj::GPU_LOAD_OPTIONS).map_err(ObjImportError::Load)?;
        materials.map_err(ObjImportError::Load)?;
        Self::from_obj_models(&models)
    }

    fn from_obj_models(models: &[tobj::Model]) -> Result<Self, ObjImportError> {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();

        for model in models {
            let mesh = &model.mesh;
            if mesh.positions.is_empty() || mesh.positions.len() % 3 != 0 {
                return Err(ObjImportError::Invalid(format!(
                    "model '{}' has no valid positions",
                    model.name
                )));
            }
            if mesh.indices.is_empty() || mesh.indices.len() % 3 != 0 {
                return Err(ObjImportError::Invalid(format!(
                    "model '{}' has no triangles",
                    model.name
                )));
            }

            let vertex_count = mesh.positions.len() / 3;
            if !mesh.normals.is_empty() && mesh.normals.len() != vertex_count * 3 {
                return Err(ObjImportError::Invalid(format!(
                    "model '{}' has an incomplete normal array",
                    model.name
                )));
            }
            if !mesh.texcoords.is_empty() && mesh.texcoords.len() != vertex_count * 2 {
                return Err(ObjImportError::Invalid(format!(
                    "model '{}' has an incomplete texture coordinate array",
                    model.name
                )));
            }
            if mesh.positions.iter().any(|value| !value.is_finite())
                || mesh.normals.iter().any(|value| !value.is_finite())
                || mesh.texcoords.iter().any(|value| !value.is_finite())
            {
                return Err(ObjImportError::Invalid(format!(
                    "model '{}' contains non-finite vertex data",
                    model.name
                )));
            }
            if mesh
                .indices
                .iter()
                .any(|&index| index as usize >= vertex_count)
            {
                return Err(ObjImportError::Invalid(format!(
                    "model '{}' has an out-of-range index",
                    model.name
                )));
            }

            let base_index = u32::try_from(vertices.len()).map_err(|_| {
                ObjImportError::Invalid("vertex count exceeds u32 index range".to_owned())
            })?;
            let fallback_normals = calculate_normals(&mesh.positions, &mesh.indices);

            for (index, &fallback_normal) in fallback_normals.iter().enumerate().take(vertex_count)
            {
                let position = [
                    mesh.positions[index * 3],
                    mesh.positions[index * 3 + 1],
                    mesh.positions[index * 3 + 2],
                ];
                let normal = if mesh.normals.is_empty() {
                    fallback_normal
                } else {
                    let normal = [
                        mesh.normals[index * 3],
                        mesh.normals[index * 3 + 1],
                        mesh.normals[index * 3 + 2],
                    ];
                    let normal = glam::Vec3::from_array(normal);
                    if normal.length_squared() > f32::EPSILON {
                        normal.normalize().to_array()
                    } else {
                        fallback_normal
                    }
                };
                let tex_coords = if mesh.texcoords.is_empty() {
                    [0.0, 0.0]
                } else {
                    [mesh.texcoords[index * 2], mesh.texcoords[index * 2 + 1]]
                };

                vertices.push(Vertex {
                    position,
                    normal,
                    color: [1.0, 1.0, 1.0],
                    tex_coords,
                });
            }

            for &index in &mesh.indices {
                indices.push(base_index.checked_add(index).ok_or_else(|| {
                    ObjImportError::Invalid("vertex count exceeds u32 index range".to_owned())
                })?);
            }
        }

        if vertices.is_empty() || indices.is_empty() {
            return Err(ObjImportError::Invalid(
                "file does not contain any triangle meshes".to_owned(),
            ));
        }
        u32::try_from(indices.len()).map_err(|_| {
            ObjImportError::Invalid("index count exceeds u32 draw range".to_owned())
        })?;

        let skin_vertices = vec![SkinVertex::default(); vertices.len()];
        let uv_sets = vertices
            .iter()
            .map(|vertex| {
                UvSetsVertex([
                    [vertex.tex_coords[0], vertex.tex_coords[1], 0.0, 0.0],
                    [0.0; 4],
                ])
            })
            .collect::<Vec<_>>();
        let tangents = calculate_tangents(&vertices, &indices);
        Ok(Self {
            vertices,
            uv_sets,
            indices,
            skin_vertices,
            tangents,
        })
    }
}

pub(crate) fn calculate_normals(positions: &[f32], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![glam::Vec3::ZERO; positions.len() / 3];
    for triangle in indices.as_chunks::<3>().0 {
        let a = triangle[0] as usize;
        let b = triangle[1] as usize;
        let c = triangle[2] as usize;
        let a = glam::Vec3::from_slice(&positions[a * 3..a * 3 + 3]);
        let b = glam::Vec3::from_slice(&positions[b * 3..b * 3 + 3]);
        let c = glam::Vec3::from_slice(&positions[c * 3..c * 3 + 3]);
        let normal = (b - a).cross(c - a);
        if normal.is_finite() && normal.length_squared() > f32::EPSILON {
            normals[triangle[0] as usize] += normal;
            normals[triangle[1] as usize] += normal;
            normals[triangle[2] as usize] += normal;
        }
    }

    normals
        .into_iter()
        .map(|normal| {
            if normal.length_squared() > f32::EPSILON {
                normal.normalize().to_array()
            } else {
                glam::Vec3::Y.to_array()
            }
        })
        .collect()
}

pub(crate) fn calculate_tangents(vertices: &[Vertex], indices: &[u32]) -> Vec<[f32; 4]> {
    let uv_coords: Vec<_> = vertices.iter().map(|vertex| vertex.tex_coords).collect();
    calculate_tangents_with_uv(vertices, &uv_coords, indices)
}

pub(crate) fn calculate_tangents_with_uv(
    vertices: &[Vertex],
    uv_coords: &[[f32; 2]],
    indices: &[u32],
) -> Vec<[f32; 4]> {
    let mut tangent_sums = vec![glam::Vec3::ZERO; vertices.len()];
    let mut bitangent_sums = vec![glam::Vec3::ZERO; vertices.len()];
    for triangle in indices.as_chunks::<3>().0 {
        let [a, b, c] = triangle;
        let (a, b, c) = (*a as usize, *b as usize, *c as usize);
        let position_a = glam::Vec3::from_array(vertices[a].position);
        let position_b = glam::Vec3::from_array(vertices[b].position);
        let position_c = glam::Vec3::from_array(vertices[c].position);
        let uv_a = glam::Vec2::from_array(uv_coords[a]);
        let uv_b = glam::Vec2::from_array(uv_coords[b]);
        let uv_c = glam::Vec2::from_array(uv_coords[c]);
        let edge_1 = position_b - position_a;
        let edge_2 = position_c - position_a;
        let uv_edge_1 = uv_b - uv_a;
        let uv_edge_2 = uv_c - uv_a;
        let determinant = uv_edge_1.x * uv_edge_2.y - uv_edge_1.y * uv_edge_2.x;
        if !determinant.is_finite() || determinant.abs() <= 1e-8 {
            continue;
        }
        let reciprocal = determinant.recip();
        let tangent = (edge_1 * uv_edge_2.y - edge_2 * uv_edge_1.y) * reciprocal;
        let bitangent = (edge_2 * uv_edge_1.x - edge_1 * uv_edge_2.x) * reciprocal;
        if tangent.is_finite() && bitangent.is_finite() {
            for index in [a, b, c] {
                tangent_sums[index] += tangent;
                bitangent_sums[index] += bitangent;
            }
        }
    }

    vertices
        .iter()
        .enumerate()
        .map(|(index, vertex)| {
            let normal = glam::Vec3::from_array(vertex.normal).normalize_or_zero();
            let accumulated = tangent_sums[index] - normal * normal.dot(tangent_sums[index]);
            let tangent = if accumulated.is_finite() && accumulated.length_squared() > 1e-10 {
                accumulated.normalize()
            } else {
                let helper = if normal.y.abs() < 0.999 {
                    glam::Vec3::Y
                } else {
                    glam::Vec3::X
                };
                normal.cross(helper).normalize_or_zero()
            };
            let handedness = if normal.cross(tangent).dot(bitangent_sums[index]) < 0.0 {
                -1.0
            } else {
                1.0
            };
            [tangent.x, tangent.y, tangent.z, handedness]
        })
        .collect()
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TangentVertex(pub [f32; 4]);

impl TangentVertex {
    const ATTRIBS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![14 => Float32x4];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
    pub tex_coords: [f32; 2],
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Mesh {
    pub vertex_buffer: wgpu::Buffer,
    pub skin_buffer: wgpu::Buffer,
    pub tangent_buffer: wgpu::Buffer,
    pub uv_sets_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    pub num_indices: u32,
    pub bounds_center: Vec3,
    pub bounds_radius: f32,
}

impl Mesh {
    pub fn from_slices(
        device: &wgpu::Device,
        vertices: &[Vertex],
        indices: &[u32],
        label: Option<&str>,
    ) -> Self {
        let skin_vertices = vec![SkinVertex::default(); vertices.len()];
        let uv_sets = vec![UvSetsVertex::default(); vertices.len()];
        let tangents = calculate_tangents(vertices, indices);
        Self::from_slices_with_skin(
            device,
            vertices,
            &uv_sets,
            &skin_vertices,
            &tangents,
            indices,
            label,
        )
    }

    pub fn from_mesh_data(device: &wgpu::Device, data: &MeshData, label: Option<&str>) -> Self {
        Self::from_slices_with_skin(
            device,
            &data.vertices,
            &data.uv_sets,
            &data.skin_vertices,
            &data.tangents,
            &data.indices,
            label,
        )
    }

    fn from_slices_with_skin(
        device: &wgpu::Device,
        vertices: &[Vertex],
        uv_sets: &[UvSetsVertex],
        skin_vertices: &[SkinVertex],
        tangents: &[[f32; 4]],
        indices: &[u32],
        label: Option<&str>,
    ) -> Self {
        let bounds_center = if vertices.is_empty() {
            Vec3::ZERO
        } else {
            let mut min = Vec3::splat(f32::MAX);
            let mut max = Vec3::splat(f32::MIN);
            for vertex in vertices {
                let position = Vec3::from(vertex.position);
                min = min.min(position);
                max = max.max(position);
            }
            (min + max) * 0.5
        };
        let bounds_radius = vertices
            .iter()
            .map(|vertex| Vec3::from(vertex.position).distance(bounds_center))
            .fold(0.0_f32, f32::max);
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: label.map(|l| format!("{l} vertex buffer")).as_deref(),
            contents: bytemuck::cast_slice(vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let skin_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: label.map(|l| format!("{l} skin buffer")).as_deref(),
            contents: bytemuck::cast_slice(skin_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let uv_sets_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: label.map(|l| format!("{l} UV sets buffer")).as_deref(),
            contents: bytemuck::cast_slice(uv_sets),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let generated_tangents;
        let tangents = if tangents.len() == vertices.len() {
            tangents
        } else {
            generated_tangents = calculate_tangents(vertices, indices);
            &generated_tangents
        };
        let tangent_vertices: Vec<TangentVertex> =
            tangents.iter().copied().map(TangentVertex).collect();
        let tangent_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: label.map(|l| format!("{l} tangent buffer")).as_deref(),
            contents: bytemuck::cast_slice(&tangent_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: label.map(|l| format!("{l} index buffer")).as_deref(),
            contents: bytemuck::cast_slice(indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        Self {
            vertex_buffer,
            skin_buffer,
            tangent_buffer,
            uv_sets_buffer,
            index_buffer,
            num_indices: indices.len() as u32,
            bounds_center,
            bounds_radius,
        }
    }

    pub fn cube(device: &wgpu::Device) -> Self {
        Self::from_slices(device, CUBE_VERTICES, CUBE_INDICES, Some("cube"))
    }

    pub fn plane(device: &wgpu::Device, size: f32) -> Self {
        let half = size / 2.0;
        let uv_extent = size;
        let vertices = &[
            Vertex {
                position: [-half, 0.0, half],
                normal: [0.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0],
                tex_coords: [0.0, uv_extent],
            },
            Vertex {
                position: [half, 0.0, half],
                normal: [0.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0],
                tex_coords: [uv_extent, uv_extent],
            },
            Vertex {
                position: [half, 0.0, -half],
                normal: [0.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0],
                tex_coords: [uv_extent, 0.0],
            },
            Vertex {
                position: [-half, 0.0, -half],
                normal: [0.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0],
                tex_coords: [0.0, 0.0],
            },
        ];
        let indices = &[0, 1, 2, 0, 2, 3];
        Self::from_slices(device, vertices, indices, Some("plane"))
    }

    pub fn sphere(device: &wgpu::Device, longitude_segments: u32, latitude_segments: u32) -> Self {
        assert!(longitude_segments >= 3);
        assert!(latitude_segments >= 2);

        let mut vertices =
            Vec::with_capacity(((longitude_segments + 1) * (latitude_segments + 1)) as usize);
        for latitude in 0..=latitude_segments {
            let v = latitude as f32 / latitude_segments as f32;
            let phi = std::f32::consts::FRAC_PI_2 - v * std::f32::consts::PI;
            let ring_radius = phi.cos();
            for longitude in 0..=longitude_segments {
                let u = longitude as f32 / longitude_segments as f32;
                let theta = u * std::f32::consts::TAU;
                let normal = [
                    ring_radius * theta.cos(),
                    phi.sin(),
                    ring_radius * theta.sin(),
                ];
                vertices.push(Vertex {
                    position: normal,
                    normal,
                    color: [1.0; 3],
                    tex_coords: [u, v],
                });
            }
        }

        let mut indices = Vec::with_capacity((longitude_segments * latitude_segments * 6) as usize);
        let row_width = longitude_segments + 1;
        for latitude in 0..latitude_segments {
            for longitude in 0..longitude_segments {
                let upper_left = latitude * row_width + longitude;
                let upper_right = upper_left + 1;
                let lower_left = upper_left + row_width;
                let lower_right = lower_left + 1;
                indices.extend_from_slice(&[
                    upper_left,
                    upper_right,
                    lower_left,
                    upper_right,
                    lower_right,
                    lower_left,
                ]);
            }
        }
        Self::from_slices(device, &vertices, &indices, Some("smooth sphere"))
    }
}

pub const CUBE_VERTICES: &[Vertex] = &[
    // Передняя грань (+Z)
    Vertex {
        position: [-0.5, -0.5, 0.5],
        normal: [0.0, 0.0, 1.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 1.0],
    },
    Vertex {
        position: [0.5, -0.5, 0.5],
        normal: [0.0, 0.0, 1.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 1.0],
    },
    Vertex {
        position: [0.5, 0.5, 0.5],
        normal: [0.0, 0.0, 1.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 0.0],
    },
    Vertex {
        position: [-0.5, 0.5, 0.5],
        normal: [0.0, 0.0, 1.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 0.0],
    },
    // Задняя грань (-Z)
    Vertex {
        position: [0.5, -0.5, -0.5],
        normal: [0.0, 0.0, -1.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 1.0],
    },
    Vertex {
        position: [-0.5, -0.5, -0.5],
        normal: [0.0, 0.0, -1.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 1.0],
    },
    Vertex {
        position: [-0.5, 0.5, -0.5],
        normal: [0.0, 0.0, -1.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 0.0],
    },
    Vertex {
        position: [0.5, 0.5, -0.5],
        normal: [0.0, 0.0, -1.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 0.0],
    },
    // Правая грань (+X)
    Vertex {
        position: [0.5, -0.5, 0.5],
        normal: [1.0, 0.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 1.0],
    },
    Vertex {
        position: [0.5, -0.5, -0.5],
        normal: [1.0, 0.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 1.0],
    },
    Vertex {
        position: [0.5, 0.5, -0.5],
        normal: [1.0, 0.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 0.0],
    },
    Vertex {
        position: [0.5, 0.5, 0.5],
        normal: [1.0, 0.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 0.0],
    },
    // Левая грань (-X)
    Vertex {
        position: [-0.5, -0.5, -0.5],
        normal: [-1.0, 0.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 1.0],
    },
    Vertex {
        position: [-0.5, -0.5, 0.5],
        normal: [-1.0, 0.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 1.0],
    },
    Vertex {
        position: [-0.5, 0.5, 0.5],
        normal: [-1.0, 0.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 0.0],
    },
    Vertex {
        position: [-0.5, 0.5, -0.5],
        normal: [-1.0, 0.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 0.0],
    },
    // Верхняя грань (+Y)
    Vertex {
        position: [-0.5, 0.5, 0.5],
        normal: [0.0, 1.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 1.0],
    },
    Vertex {
        position: [0.5, 0.5, 0.5],
        normal: [0.0, 1.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 1.0],
    },
    Vertex {
        position: [0.5, 0.5, -0.5],
        normal: [0.0, 1.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 0.0],
    },
    Vertex {
        position: [-0.5, 0.5, -0.5],
        normal: [0.0, 1.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 0.0],
    },
    // Нижняя грань (-Y)
    Vertex {
        position: [-0.5, -0.5, -0.5],
        normal: [0.0, -1.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 1.0],
    },
    Vertex {
        position: [0.5, -0.5, -0.5],
        normal: [0.0, -1.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 1.0],
    },
    Vertex {
        position: [0.5, -0.5, 0.5],
        normal: [0.0, -1.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [1.0, 0.0],
    },
    Vertex {
        position: [-0.5, -0.5, 0.5],
        normal: [0.0, -1.0, 0.0],
        color: [1.0, 1.0, 1.0],
        tex_coords: [0.0, 0.0],
    },
];

pub const CUBE_INDICES: &[u32] = &[
    0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16, 17, 18,
    16, 18, 19, 20, 21, 22, 20, 22, 23,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle_model(name: &str) -> tobj::Model {
        let mesh = tobj::Mesh {
            positions: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            indices: vec![0, 1, 2],
            ..Default::default()
        };
        tobj::Model::new(mesh, name.to_owned())
    }

    #[test]
    fn generates_missing_normals_and_texture_coordinates() {
        let data = MeshData::from_obj_models(&[triangle_model("triangle")]).unwrap();

        assert_eq!(data.vertices.len(), 3);
        assert_eq!(data.indices, [0, 1, 2]);
        assert_eq!(data.vertices[0].normal, [0.0, 0.0, 1.0]);
        assert_eq!(data.vertices[0].tex_coords, [0.0, 0.0]);
    }

    #[test]
    fn generates_tangent_frame_from_triangle_uvs() {
        let vertices = [
            Vertex {
                position: [0.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                color: [1.0; 3],
                tex_coords: [0.0, 0.0],
            },
            Vertex {
                position: [1.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                color: [1.0; 3],
                tex_coords: [1.0, 0.0],
            },
            Vertex {
                position: [0.0, 1.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                color: [1.0; 3],
                tex_coords: [0.0, 1.0],
            },
        ];
        let tangents = calculate_tangents(&vertices, &[0, 1, 2]);

        assert_eq!(tangents.len(), vertices.len());
        for tangent in tangents {
            assert!((tangent[0] - 1.0).abs() < 1e-5);
            assert!(tangent[1].abs() < 1e-5);
            assert!(tangent[2].abs() < 1e-5);
            assert_eq!(tangent[3], 1.0);
        }
    }

    #[test]
    fn generates_tangent_frame_from_selected_uv_set() {
        let vertices = [
            Vertex {
                position: [0.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                color: [1.0; 3],
                tex_coords: [0.0, 0.0],
            },
            Vertex {
                position: [1.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                color: [1.0; 3],
                tex_coords: [1.0, 0.0],
            },
            Vertex {
                position: [0.0, 1.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                color: [1.0; 3],
                tex_coords: [0.0, 1.0],
            },
        ];
        let uv_set = [[0.0, 0.0], [0.0, 1.0], [1.0, 0.0]];
        let tangents = calculate_tangents_with_uv(&vertices, &uv_set, &[0, 1, 2]);

        assert!(tangents.iter().all(|tangent| tangent[0].abs() < 1e-5));
        assert!(
            tangents
                .iter()
                .all(|tangent| (tangent[1] - 1.0).abs() < 1e-5)
        );
        assert!(tangents.iter().all(|tangent| tangent[2].abs() < 1e-5));
    }

    #[test]
    fn combines_models_with_u32_indices() {
        let data = MeshData::from_obj_models(&[triangle_model("first"), triangle_model("second")])
            .unwrap();

        assert_eq!(data.vertices.len(), 6);
        assert_eq!(data.indices, [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn rejects_out_of_range_indices() {
        let mut model = triangle_model("invalid");
        model.mesh.indices[2] = 3;

        assert!(MeshData::from_obj_models(&[model]).is_err());
    }

    #[test]
    fn rejects_empty_meshes() {
        let model = tobj::Model::new(tobj::Mesh::default(), "empty".to_owned());

        assert!(MeshData::from_obj_models(&[model]).is_err());
    }

    #[test]
    fn loads_sample_obj_file() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("models")
            .join("sample.obj");
        let data = MeshData::load_obj(path).unwrap();

        assert_eq!(data.indices.len(), 18);
        assert_eq!(data.vertices.len(), 5);
    }

    #[test]
    fn loads_obj_materials_textures_and_material_groups() {
        let directory = std::env::temp_dir().join(format!(
            "bone-smashing-obj-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let obj_path = directory.join("model.obj");
        std::fs::write(
            &obj_path,
            "mtllib model.mtl\n\
             o wall\n\
             v 0 0 0\n\
             v 1 0 0\n\
             v 0 1 0\n\
             vt 0 0\n\
             vt 1 0\n\
             vt 0 1\n\
             vn 0 0 1\n\
             usemtl brick\n\
             f 1/1/1 2/2/1 3/3/1\n\
             o roof\n\
             usemtl roof_material\n\
             f 1/1/1 3/3/1 2/2/1\n",
        )
        .unwrap();
        std::fs::write(
            directory.join("model.mtl"),
            "newmtl brick\n\
             Kd 0.8 0.7 0.6\n\
             Ks 0.4 0.4 0.4\n\
             Ns 64\n\
             map_Kd -s 1 1 1 albedo.png\n\
             map_Bump normal.png\n\
             newmtl roof_material\n\
             Kd 0.2 0.3 0.4\n\
             map_Ka roof_ao.png\n",
        )
        .unwrap();
        let image = image::RgbaImage::from_raw(1, 2, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        image.save(directory.join("albedo.png")).unwrap();
        image.save(directory.join("albedo_occlusion.png")).unwrap();
        image.save(directory.join("roof_ao.png")).unwrap();
        image.save(directory.join("normal.png")).unwrap();

        let model = ObjModelData::load(&obj_path).unwrap();

        assert_eq!(model.primitive_count(), 2);
        assert_eq!(model.materials.len(), 2);
        let brick = model
            .materials
            .iter()
            .find(|material| material.name == "brick")
            .unwrap();
        assert_eq!(brick.diffuse_texture, Some(0));
        assert_eq!(brick.ambient_texture, Some(1));
        assert_eq!(brick.normal_texture, Some(2));
        assert_eq!(
            model
                .materials
                .iter()
                .find(|material| material.name == "roof_material")
                .unwrap()
                .ambient_texture,
            Some(3)
        );
        assert_eq!(model.textures.len(), 4);
        assert_eq!(model.textures[0].rgba, [0, 0, 255, 255, 255, 0, 0, 255]);
        assert!(
            model
                .primitives
                .iter()
                .all(|primitive| primitive.material_index.is_some())
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn parses_obj_texture_options_and_filenames_with_spaces() {
        assert_eq!(
            obj_texture_filename("-s 1 1 1 \"albedo map.png\""),
            Some("albedo map.png".to_owned())
        );
        assert_eq!(
            obj_texture_filename("textures/albedo map.png"),
            Some("textures/albedo map.png".to_owned())
        );
        assert_eq!(obj_texture_filename("-halo"), None);
    }

    #[test]
    fn preserves_obj_texture_quality_up_to_4096_pixels_without_upscaling() {
        let small = image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 1));
        let small = downscale_obj_texture(small);
        assert_eq!((small.width(), small.height()), (2, 1));

        let large = image::DynamicImage::ImageRgba8(image::RgbaImage::new(
            MAX_IMPORTED_TEXTURE_DIMENSION + 1,
            1,
        ));
        let large = downscale_obj_texture(large);
        assert_eq!(
            (large.width(), large.height()),
            (MAX_IMPORTED_TEXTURE_DIMENSION, 1)
        );
    }
}
