use glam::Mat4;
use std::path::PathBuf;

use super::transform::Transform;
use super::world::ModelGroupId;
use crate::core::{MaterialId, MeshId};

#[derive(Clone, Debug)]
pub struct AssetSource {
    pub path: PathBuf,
    pub instance_index: usize,
}

#[derive(Clone, Debug)]
pub struct RenderObject {
    pub name: String,
    pub transform: Transform,
    pub mesh_id: MeshId,
    pub material_id: MaterialId,
    local_matrix: Mat4,
    skin_matrices: Vec<Mat4>,
    asset_source: Option<AssetSource>,
    model_group: Option<ModelGroupId>,
}

impl RenderObject {
    pub fn new(transform: Transform, mesh_id: MeshId, material_id: MaterialId) -> Self {
        Self {
            name: String::new(),
            transform,
            mesh_id,
            material_id,
            local_matrix: Mat4::IDENTITY,
            skin_matrices: Vec::new(),
            asset_source: None,
            model_group: None,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_asset_source(mut self, path: PathBuf, instance_index: usize) -> Self {
        self.asset_source = Some(AssetSource {
            path,
            instance_index,
        });
        self
    }

    pub fn asset_source(&self) -> Option<&AssetSource> {
        self.asset_source.as_ref()
    }

    pub fn model_group(&self) -> Option<ModelGroupId> {
        self.model_group
    }

    pub(crate) fn set_model_group(&mut self, model_group: Option<ModelGroupId>) {
        self.model_group = model_group;
    }

    pub fn with_local_matrix(mut self, local_matrix: Mat4) -> Self {
        self.local_matrix = local_matrix;
        self
    }

    pub fn set_local_matrix(&mut self, local_matrix: Mat4) {
        self.local_matrix = local_matrix;
    }

    pub fn with_skin_matrices(mut self, skin_matrices: Vec<Mat4>) -> Self {
        self.skin_matrices = skin_matrices;
        self
    }

    pub fn set_skin_matrices(&mut self, skin_matrices: &[Mat4]) {
        self.skin_matrices.clear();
        self.skin_matrices.extend_from_slice(skin_matrices);
    }

    pub fn skin_matrices(&self) -> &[Mat4] {
        &self.skin_matrices
    }

    pub fn model_matrix(&self) -> Mat4 {
        self.transform.to_matrix() * self.local_matrix
    }
}
