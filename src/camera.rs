use glam::{Mat4, Vec3};
use winit::keyboard::KeyCode;

use crate::config::{Z_FAR, Z_NEAR};
use crate::input::Input;
use crate::math::{look_at_rh, perspective_rh};

pub struct Camera {
    pub eye: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub up: Vec3,
    pub aspect: f32,
}

impl Camera {
    pub fn build_view_projection_matrix(&self) -> Mat4 {
        let forward = Vec3::new(
            self.yaw.cos() * self.pitch.cos(),
            self.pitch.sin(),
            self.yaw.sin() * self.pitch.cos(),
        )
        .normalize();

        let view = look_at_rh(self.eye, self.eye + forward, self.up);
        let proj = perspective_rh(45_f32.to_radians(), self.aspect, Z_NEAR, Z_FAR);
        proj * view
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CameraUniform {
    pub mvp: [[f32; 4]; 4],
    pub inverse_mvp: [[f32; 4]; 4],
    pub view_pos: [f32; 4], // Позиция камеры в мире для расчета бликов
}

impl CameraUniform {
    pub fn new() -> Self {
        Self {
            mvp: Mat4::IDENTITY.to_cols_array_2d(),
            inverse_mvp: Mat4::IDENTITY.to_cols_array_2d(),
            view_pos: [0.0; 4],
        }
    }

    pub fn update_view_proj(&mut self, camera: &Camera) {
        let view_projection = camera.build_view_projection_matrix();
        self.mvp = view_projection.to_cols_array_2d();
        self.inverse_mvp = view_projection.inverse().to_cols_array_2d();
        self.view_pos = [camera.eye.x, camera.eye.y, camera.eye.z, 1.0];
    }
}

pub struct CameraController {
    speed: f32,
    sensitivity: f32,
}

impl CameraController {
    pub fn new(speed: f32, sensitivity: f32) -> Self {
        Self { speed, sensitivity }
    }

    pub fn update_camera(&self, camera: &mut Camera, input: &Input, dt: f32) {
        if input.cursor_locked {
            camera.yaw += input.mouse_delta.0 as f32 * self.sensitivity;
            camera.pitch -= input.mouse_delta.1 as f32 * self.sensitivity;

            let max_pitch = 89.0_f32.to_radians();
            camera.pitch = camera.pitch.clamp(-max_pitch, max_pitch);
        }

        let forward = Vec3::new(
            camera.yaw.cos() * camera.pitch.cos(),
            camera.pitch.sin(),
            camera.yaw.sin() * camera.pitch.cos(),
        )
        .normalize();
        let right = forward.cross(camera.up).normalize();

        let mut movement = Vec3::ZERO;
        if input.is_key_pressed(KeyCode::KeyW) || input.is_key_pressed(KeyCode::ArrowUp) {
            movement += forward;
        }
        if input.is_key_pressed(KeyCode::KeyS) || input.is_key_pressed(KeyCode::ArrowDown) {
            movement -= forward;
        }
        if input.is_key_pressed(KeyCode::KeyD) || input.is_key_pressed(KeyCode::ArrowRight) {
            movement += right;
        }
        if input.is_key_pressed(KeyCode::KeyA) || input.is_key_pressed(KeyCode::ArrowLeft) {
            movement -= right;
        }
        if input.is_key_pressed(KeyCode::Space) {
            movement += camera.up;
        }
        if input.is_key_pressed(KeyCode::ShiftLeft) {
            movement -= camera.up;
        }

        camera.eye += movement.normalize_or_zero() * self.speed * dt;
    }
}
