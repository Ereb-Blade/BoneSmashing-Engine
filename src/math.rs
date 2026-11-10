use glam::{Mat3, Mat4, Vec3};

pub fn perspective_rh(fovy: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fovy / 2.0).tan();
    Mat4::from_cols_array(&[
        f / aspect,
        0.0,
        0.0,
        0.0,
        0.0,
        f,
        0.0,
        0.0,
        0.0,
        0.0,
        far / (near - far),
        -1.0,
        0.0,
        0.0,
        near * far / (near - far),
        0.0,
    ])
}

pub fn orthographic_rh(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) -> Mat4 {
    Mat4::from_cols_array(&[
        2.0 / (right - left),
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 / (top - bottom),
        0.0,
        0.0,
        0.0,
        0.0,
        1.0 / (near - far),
        0.0,
        -(right + left) / (right - left),
        -(top + bottom) / (top - bottom),
        near / (near - far),
        1.0,
    ])
}

pub fn look_at_rh(eye: Vec3, center: Vec3, up: Vec3) -> Mat4 {
    let f = (center - eye).normalize();
    let s = f.cross(up).normalize();
    let u = s.cross(f);
    Mat4::from_cols_array(&[
        s.x,
        u.x,
        -f.x,
        0.0,
        s.y,
        u.y,
        -f.y,
        0.0,
        s.z,
        u.z,
        -f.z,
        0.0,
        -s.dot(eye),
        -u.dot(eye),
        f.dot(eye),
        1.0,
    ])
}

/// Безопасное вычисление матрицы нормалей (inverse transpose)
pub fn compute_normal_matrix(model: Mat4) -> Mat3 {
    let mat3 = Mat3::from_mat4(model);
    let det = mat3.determinant();
    // Если масштаб нулевой или очень близок к нулю, обратной матрицы не существует.
    if det.abs() > 1e-6 {
        mat3.inverse().transpose()
    } else {
        // Возвращаем единичную матрицу, чтобы избежать генерации NaN в шейдере
        // при попытке нормализовать нулевой вектор.
        Mat3::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Quat, Vec3, Vec4};

    #[test]
    fn test_look_at_rh() {
        let eye = Vec3::new(0.0, 5.0, 10.0);
        let center = Vec3::ZERO;
        let up = Vec3::Y;

        let view = look_at_rh(eye, center, up);

        // Матрица вида должна переводить позицию камеры в начало координат
        let transformed_eye = view * eye.extend(1.0);
        assert!((transformed_eye.x).abs() < 1e-5);
        assert!((transformed_eye.y).abs() < 1e-5);
        assert!((transformed_eye.z).abs() < 1e-5);
        assert_eq!(transformed_eye.w, 1.0);
    }

    #[test]
    fn test_perspective_rh_depth() {
        let near = 0.1;
        let far = 100.0;
        let proj = perspective_rh(90.0_f32.to_radians(), 1.0, near, far);

        // В правой системе координат камера смотрит в -Z
        // Точка на near-плоскости (z = -0.1)
        let p_near = proj * Vec4::new(0.0, 0.0, -near, 1.0);
        let ndc_near = p_near.z / p_near.w;
        assert!(
            (ndc_near - 0.0).abs() < 1e-5,
            "Near plane should map to 0.0 (WGPU standard)"
        );

        // Точка на far-плоскости (z = -100.0)
        let p_far = proj * Vec4::new(0.0, 0.0, -far, 1.0);
        let ndc_far = p_far.z / p_far.w;
        assert!(
            (ndc_far - 1.0).abs() < 1e-5,
            "Far plane should map to 1.0 (WGPU standard)"
        );
    }

    #[test]
    fn test_orthographic_rh_depth() {
        let near = 0.1;
        let far = 50.0;
        let proj = orthographic_rh(-10.0, 10.0, -10.0, 10.0, near, far);
        let near_depth = (proj * Vec3::new(0.0, 0.0, -near).extend(1.0)).z;
        let far_depth = (proj * Vec3::new(0.0, 0.0, -far).extend(1.0)).z;

        assert!((near_depth - 0.0).abs() < 1e-5);
        assert!((far_depth - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_normal_matrix() {
        // 1. Неравномерный масштаб и поворот
        let scale = Vec3::new(2.0, 0.5, 1.0); // Сильно сплющиваем по Y и растягиваем по X
        let rot = Quat::from_rotation_z(45.0_f32.to_radians());
        let model = Mat4::from_scale_rotation_translation(scale, rot, Vec3::new(10.0, 0.0, 0.0));

        let norm_mat = compute_normal_matrix(model);

        // Касательный вектор к поверхности (например, смотрит вдоль X)
        let tangent = Vec3::X;
        // Нормаль к поверхности (смотрит вдоль Y)
        let normal = Vec3::Y;

        // Трансформируем касательную модельной матрицей (без переноса)
        let transformed_tangent = Mat3::from_mat4(model) * tangent;
        // Трансформируем нормаль матрицей нормалей
        let transformed_normal = norm_mat * normal;

        // КЛЮЧЕВОЙ МОМЕНТ: Нормаль должна остаться перпендикулярна касательной
        let dot_product = transformed_tangent.dot(transformed_normal);
        assert!(
            dot_product.abs() < 1e-5,
            "Нормаль должна оставаться ортогональной поверхности после скейла!"
        );

        // 2. Нулевой масштаб (должен возвращать IDENTITY и не вызывать панику/NaN)
        let model_zero =
            Mat4::from_scale_rotation_translation(Vec3::ZERO, Quat::IDENTITY, Vec3::ZERO);
        let norm_mat_zero = compute_normal_matrix(model_zero);
        assert_eq!(
            norm_mat_zero,
            Mat3::IDENTITY,
            "При вырожденной матрице ожидается IDENTITY"
        );
    }
}
