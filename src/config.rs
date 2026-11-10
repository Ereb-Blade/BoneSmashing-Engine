use wgpu::TextureFormat;

pub const DEPTH_FORMAT: TextureFormat = TextureFormat::Depth32Float;
pub const MSAA_SAMPLES: u32 = 4;

pub const CAMERA_SPEED: f32 = 5.0;
pub const CAMERA_SENSITIVITY: f32 = 0.002;

pub const Z_NEAR: f32 = 0.1;
pub const Z_FAR: f32 = 100.0;
