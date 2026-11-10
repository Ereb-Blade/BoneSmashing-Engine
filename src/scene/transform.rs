use glam::{Mat4, Quat, Vec3};

#[derive(Clone, Copy, Debug)]
pub struct Transform {
    pub position: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl Transform {
    pub fn new(position: Vec3, rotation: Quat, scale: Vec3) -> Self {
        Self {
            position,
            rotation,
            scale,
        }
    }

    pub fn from_translation(position: Vec3) -> Self {
        Self {
            position,
            ..Default::default()
        }
    }

    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_matrix() {
        let t = Transform::default();
        assert_eq!(t.to_matrix(), Mat4::IDENTITY);
    }

    #[test]
    fn test_translation_and_scale() {
        let t = Transform {
            position: Vec3::new(10.0, 20.0, 30.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::new(2.0, 3.0, 4.0),
        };
        let mat = t.to_matrix();

        // Точка (1, 1, 1) сначала масштабируется до (2, 3, 4), а затем смещается на (10, 20, 30) -> (12, 23, 34)
        let transformed_point = mat.transform_point3(Vec3::ONE);
        assert_eq!(transformed_point, Vec3::new(12.0, 23.0, 34.0));
    }
}
