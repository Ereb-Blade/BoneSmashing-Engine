use glam::Mat4;

use super::transform::Transform;
use crate::core::{MaterialId, MeshId};

#[derive(Clone, Debug)]
pub struct RenderObject {
    pub transform: Transform,
    pub mesh_id: MeshId,
    pub material_id: MaterialId,
    local_matrix: Mat4,
    skin_matrices: Vec<Mat4>,
}

impl RenderObject {
    pub fn new(transform: Transform, mesh_id: MeshId, material_id: MaterialId) -> Self {
        Self {
            transform,
            mesh_id,
            material_id,
            local_matrix: Mat4::IDENTITY,
            skin_matrices: Vec::new(),
        }
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
