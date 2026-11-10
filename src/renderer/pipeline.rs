use crate::config::{DEPTH_FORMAT, MSAA_SAMPLES};
use crate::renderer::mesh::{UvSetsVertex, Vertex};

pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

pub const SHADER: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    inverse_view_proj: mat4x4<f32>,
    view_pos: vec4<f32>,
};

struct Light {
    direction_intensity: vec4<f32>,
    color: vec4<f32>,
    fill_direction_intensity: vec4<f32>,
    fill_color: vec4<f32>,
    ambient_top: vec4<f32>,
    ambient_bottom: vec4<f32>,
};

struct MaterialParams {
    base_color: vec4<f32>,
    roughness: f32,
    metallic: f32,
    unlit: f32,
    alpha_mode: u32,
    alpha_cutoff: f32,
    normal_scale: f32,
    transmission_factor: f32,
    ior: f32,
    specular_factor: f32,
    double_sided: u32,
    occlusion_strength: f32,
    emissive_strength: f32,
    specular_color_factor: vec3<f32>,
    base_color_tex_coord: u32,
    metallic_roughness_tex_coord: u32,
    normal_tex_coord: u32,
    occlusion_tex_coord: u32,
    emissive_tex_coord: u32,
    emissive_factor: vec3<f32>,
    specular_tex_coord: u32,
    specular_color_tex_coord: u32,
    transmission_tex_coord: u32,
    pbr_workflow: u32,
    _pad0: u32,
    thickness_factor: f32,
    attenuation_distance: f32,
    thickness_tex_coord: u32,
    _volume_pad0: u32,
    attenuation_color: vec3<f32>,
    _volume_pad1: u32,
    thickness_uv_transform: array<vec4<f32>, 2>,
    base_color_uv_transform: array<vec4<f32>, 2>,
    metallic_roughness_uv_transform: array<vec4<f32>, 2>,
    normal_uv_transform: array<vec4<f32>, 2>,
    occlusion_uv_transform: array<vec4<f32>, 2>,
    emissive_uv_transform: array<vec4<f32>, 2>,
    specular_uv_transform: array<vec4<f32>, 2>,
    specular_color_uv_transform: array<vec4<f32>, 2>,
    transmission_uv_transform: array<vec4<f32>, 2>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<uniform> light: Light;
struct SkinMatrices {
    matrices: array<mat4x4<f32>>,
};
@group(0) @binding(2) var<storage, read> skin_matrices: SkinMatrices;
@group(0) @binding(3) var<uniform> shadow_matrix: mat4x4<f32>;
@group(0) @binding(4) var t_shadow: texture_depth_2d;
@group(0) @binding(5) var s_shadow: sampler_comparison;

@group(1) @binding(0) var t_diffuse: texture_2d<f32>;
@group(1) @binding(1) var s_diffuse: sampler;
@group(1) @binding(2) var<uniform> material: MaterialParams;
@group(1) @binding(3) var t_metallic_roughness: texture_2d<f32>;
@group(1) @binding(4) var s_metallic_roughness: sampler;
@group(1) @binding(5) var t_normal: texture_2d<f32>;
@group(1) @binding(6) var t_occlusion: texture_2d<f32>;
@group(1) @binding(7) var t_emissive: texture_2d<f32>;
@group(1) @binding(8) var t_specular: texture_2d<f32>;
@group(1) @binding(9) var t_specular_color: texture_2d<f32>;
@group(1) @binding(10) var t_transmission: texture_2d<f32>;
@group(1) @binding(11) var t_thickness: texture_2d<f32>;
@group(2) @binding(0) var t_environment: texture_2d<f32>;
@group(2) @binding(1) var s_environment: sampler;
@group(2) @binding(2) var t_irradiance: texture_2d<f32>;
@group(2) @binding(3) var t_prefiltered_environment: texture_2d<f32>;
@group(2) @binding(4) var t_brdf_lut: texture_2d<f32>;
@group(3) @binding(0) var t_scene_color: texture_2d<f32>;
@group(3) @binding(1) var s_scene_color: sampler;

const PI: f32 = 3.14159265359;

fn safe_normalize(value: vec3<f32>, fallback: vec3<f32>) -> vec3<f32> {
    let length_squared = dot(value, value);
    if length_squared > 1e-12 {
        return value * inverseSqrt(length_squared);
    }
    return fallback;
}

fn transform_skin_normal(skin: mat4x4<f32>, normal: vec3<f32>) -> vec3<f32> {
    let c0 = skin[0].xyz;
    let c1 = skin[1].xyz;
    let c2 = skin[2].xyz;
    let determinant = dot(c0, cross(c1, c2));
    if abs(determinant) > 1e-8 {
        let inverse_transpose = mat3x3<f32>(
            cross(c1, c2),
            cross(c2, c0),
            cross(c0, c1),
        ) * (1.0 / determinant);
        return inverse_transpose * normal;
    }
    return mat3x3<f32>(c0, c1, c2) * normal;
}

fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - clamp(cos_theta, 0.0, 1.0), 5.0);
}

fn distribution_ggx(n_dot_h: f32, roughness: f32) -> f32 {
    let alpha = roughness * roughness;
    let alpha_squared = alpha * alpha;
    let denominator = n_dot_h * n_dot_h * (alpha_squared - 1.0) + 1.0;
    return alpha_squared / max(PI * denominator * denominator, 1e-6);
}

fn geometry_schlick_ggx(n_dot_direction: f32, roughness: f32) -> f32 {
    let alpha = roughness * roughness;
    let alpha_squared = alpha * alpha;
    let denominator = n_dot_direction
        + sqrt(alpha_squared + (1.0 - alpha_squared) * n_dot_direction * n_dot_direction);
    return 2.0 * n_dot_direction / max(denominator, 1e-6);
}

fn evaluate_direct_light(
    normal: vec3<f32>,
    view_dir: vec3<f32>,
    light_dir: vec3<f32>,
    radiance: vec3<f32>,
    base_color: vec3<f32>,
    metallic: f32,
    roughness: f32,
    f0: vec3<f32>,
) -> vec3<f32> {
    let n_dot_l = max(dot(normal, light_dir), 0.0);
    let n_dot_v = max(dot(normal, view_dir), 0.0);
    if n_dot_l <= 0.0 || n_dot_v <= 0.0 {
        return vec3<f32>(0.0);
    }

    let half_dir = safe_normalize(view_dir + light_dir, normal);
    let n_dot_h = max(dot(normal, half_dir), 0.0);
    let h_dot_v = max(dot(half_dir, view_dir), 0.0);
    let fresnel = fresnel_schlick(h_dot_v, f0);
    let distribution = distribution_ggx(n_dot_h, roughness);
    let geometry = geometry_schlick_ggx(n_dot_v, roughness)
        * geometry_schlick_ggx(n_dot_l, roughness);
    let specular = distribution * geometry * fresnel
        / max(4.0 * n_dot_v * n_dot_l, 1e-5);
    let diffuse_weight = (vec3<f32>(1.0) - fresnel) * (1.0 - metallic);
    let diffuse = diffuse_weight * base_color / PI;
    return (diffuse + specular) * radiance * n_dot_l;
}

fn environment_uv(direction: vec3<f32>) -> vec2<f32> {
    let d = safe_normalize(direction, vec3<f32>(0.0, 1.0, 0.0));
    return vec2<f32>(
        atan2(d.z, d.x) / (2.0 * PI) + 0.5,
        acos(clamp(d.y, -1.0, 1.0)) / PI,
    );
}

fn sample_environment(direction: vec3<f32>, mip: f32) -> vec3<f32> {
    return textureSampleLevel(t_environment, s_environment, environment_uv(direction), mip).rgb;
}

fn sample_prefiltered_environment(direction: vec3<f32>, roughness: f32) -> vec3<f32> {
    let max_mip = f32(textureNumLevels(t_prefiltered_environment) - 1u);
    return textureSampleLevel(
        t_prefiltered_environment,
        s_environment,
        environment_uv(direction),
        roughness * max_mip,
    ).rgb;
}

fn diffuse_irradiance(normal: vec3<f32>) -> vec3<f32> {
    return textureSampleLevel(t_irradiance, s_environment, environment_uv(normal), 0.0).rgb;
}

fn transform_uv(uv: vec2<f32>, transform: array<vec4<f32>, 2>) -> vec2<f32> {
    return vec2<f32>(
        dot(transform[0].xy, uv) + transform[0].z,
        dot(transform[1].xy, uv) + transform[1].z,
    );
}

fn apply_normal_map(
    normal: vec3<f32>,
    world_tangent: vec4<f32>,
    uv: vec2<f32>,
    uv_transform: array<vec4<f32>, 2>,
) -> vec3<f32> {
    if material.normal_scale == 0.0 {
        return normal;
    }
    let fallback_axis = select(
        vec3<f32>(0.0, 0.0, 1.0),
        vec3<f32>(0.0, 1.0, 0.0),
        abs(normal.z) > 0.999,
    );
    let fallback_tangent = safe_normalize(cross(normal, fallback_axis), vec3<f32>(1.0, 0.0, 0.0));
    let source_tangent = safe_normalize(
        world_tangent.xyz - normal * dot(normal, world_tangent.xyz),
        fallback_tangent,
    );
    let source_bitangent = cross(normal, source_tangent) * world_tangent.w;
    let transform_determinant =
        uv_transform[0].x * uv_transform[1].y - uv_transform[0].y * uv_transform[1].x;
    if abs(transform_determinant) <= 1e-8 {
        return normal;
    }
    let transform_handedness = select(-1.0, 1.0, transform_determinant > 0.0);
    let tangent = safe_normalize(
        (source_tangent * uv_transform[1].y - source_bitangent * uv_transform[1].x)
            * transform_handedness,
        source_tangent,
    );
    let bitangent = safe_normalize(
        (source_tangent * -uv_transform[0].y + source_bitangent * uv_transform[0].x)
            * transform_handedness,
        source_bitangent,
    );
    let sampled = textureSample(t_normal, s_diffuse, uv).xyz * 2.0 - vec3<f32>(1.0);
    let tangent_normal = safe_normalize(
        vec3<f32>(sampled.xy * material.normal_scale, sampled.z),
        vec3<f32>(0.0, 0.0, 1.0),
    );
    return safe_normalize(
        tangent * tangent_normal.x + bitangent * tangent_normal.y + normal * tangent_normal.z,
        normal,
    );
}

struct SkyboxOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) direction: vec3<f32>,
};

@vertex
fn vs_skybox(@builtin(vertex_index) vertex_index: u32) -> SkyboxOut {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let position = positions[vertex_index];
    let far_world = camera.inverse_view_proj * vec4<f32>(position, 1.0, 1.0);
    var output: SkyboxOut;
    output.clip_position = vec4<f32>(position, 1.0, 1.0);
    output.direction = far_world.xyz / far_world.w - camera.view_pos.xyz;
    return output;
}

@fragment
fn fs_skybox(input: SkyboxOut) -> @location(0) vec4<f32> {
    let environment = sample_environment(input.direction, 0.0);
    return vec4<f32>(environment, 1.0);
}

fn tonemap_aces(color: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp(
        (color * (a * color + b)) / (color * (c * color + d) + e),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
}

struct PostProcessOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(3) @binding(0) var t_postprocess: texture_2d<f32>;
@group(3) @binding(1) var s_postprocess: sampler;

@vertex
fn vs_postprocess(@builtin(vertex_index) vertex_index: u32) -> PostProcessOut {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let position = positions[vertex_index];
    var output: PostProcessOut;
    output.clip_position = vec4<f32>(position, 0.0, 1.0);
    output.uv = position * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    return output;
}

@fragment
fn fs_postprocess(input: PostProcessOut) -> @location(0) vec4<f32> {
    let hdr = textureSample(t_postprocess, s_postprocess, input.uv).rgb;
    let debug_view = u32(light.fill_color.a + 0.5);
    if debug_view >= 1u && debug_view <= 3u {
        return vec4<f32>(clamp(hdr, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
    }
    return vec4<f32>(tonemap_aces(hdr * exp2(light.ambient_bottom.a)), 1.0);
}

fn get_local_position(v: VsIn, instance: InstanceInput) -> vec4<f32> {
    var local_position = vec4<f32>(v.pos, 1.0);
    if instance.skin_info.y > 0u {
        var skin = mat4x4<f32>(
            vec4<f32>(0.0),
            vec4<f32>(0.0),
            vec4<f32>(0.0),
            vec4<f32>(0.0),
        );
        for (var index = 0u; index < 4u; index += 1u) {
            let weight = v.weights[index];
            if weight > 0.0 {
                let joint_index = instance.skin_info.x + v.joints[index];
                skin += skin_matrices.matrices[joint_index] * weight;
            }
        }
        local_position = skin * local_position;
    }
    return local_position;
}

fn shadow_visibility(world_pos: vec3<f32>, normal: vec3<f32>, light_dir: vec3<f32>) -> f32 {
    let light_clip = shadow_matrix * vec4<f32>(world_pos, 1.0);
    let projected = light_clip.xyz / light_clip.w;
    let uv = projected.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    if projected.z <= 0.0 || projected.z >= 1.0
        || any(uv <= vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0)) {
        return 1.0;
    }

    let texel = 1.0 / vec2<f32>(textureDimensions(t_shadow));
    let bias = max(0.0008 * (1.0 - dot(normal, light_dir)), 0.00025);
    var visibility = 0.0;
    for (var y = 0; y < 2; y += 1) {
        for (var x = 0; x < 2; x += 1) {
            visibility += textureSampleCompare(
                t_shadow,
                s_shadow,
                uv + (vec2<f32>(f32(x), f32(y)) - vec2<f32>(0.5)) * texel,
                projected.z - bias,
            );
        }
    }
    return visibility * 0.25;
}

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) uv_01: vec4<f32>,
    @location(14) tangent: vec4<f32>,
    @location(4) joints: vec4<u32>,
    @location(5) weights: vec4<f32>,
    @location(15) uv_23: vec4<f32>,
};

struct InstanceInput {
    @location(6) model_matrix_0: vec4<f32>,
    @location(7) model_matrix_1: vec4<f32>,
    @location(8) model_matrix_2: vec4<f32>,
    @location(9) model_matrix_3: vec4<f32>,
    @location(10) normal_matrix_0: vec4<f32>,
    @location(11) normal_matrix_1: vec4<f32>,
    @location(12) normal_matrix_2: vec4<f32>,
    @location(13) skin_info: vec2<u32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) tex_coords: vec2<f32>,
    @location(2) world_normal: vec3<f32>,
    @location(3) world_pos: vec3<f32>,
    @location(4) world_tangent: vec4<f32>,
    @location(5) tex_coords_1: vec2<f32>,
    @location(6) tex_coords_2: vec2<f32>,
    @location(7) tex_coords_3: vec2<f32>,
};

fn get_uv(uv_set: u32, uv0: vec2<f32>, uv1: vec2<f32>, uv2: vec2<f32>, uv3: vec2<f32>) -> vec2<f32> {
    switch uv_set {
        case 1u: { return uv1; }
        case 2u: { return uv2; }
        case 3u: { return uv3; }
        default: { return uv0; }
    }
}

@vertex
fn vs_main(v: VsIn, instance: InstanceInput) -> VsOut {
    let model = mat4x4<f32>(
        instance.model_matrix_0,
        instance.model_matrix_1,
        instance.model_matrix_2,
        instance.model_matrix_3,
    );

    let normal_matrix = mat3x3<f32>(
        instance.normal_matrix_0.xyz,
        instance.normal_matrix_1.xyz,
        instance.normal_matrix_2.xyz,
    );

    let local_position = get_local_position(v, instance);
    var local_normal = v.normal;
    var local_tangent = v.tangent.xyz;
    var skin_handedness = 1.0;
    if instance.skin_info.y > 0u {
        var skin = mat4x4<f32>(
            vec4<f32>(0.0),
            vec4<f32>(0.0),
            vec4<f32>(0.0),
            vec4<f32>(0.0),
        );
        for (var index = 0u; index < 4u; index += 1u) {
            let weight = v.weights[index];
            if weight > 0.0 {
                let joint_index = instance.skin_info.x + v.joints[index];
                skin += skin_matrices.matrices[joint_index] * weight;
            }
        }
        skin_handedness = select(
            1.0,
            -1.0,
            dot(skin[0].xyz, cross(skin[1].xyz, skin[2].xyz)) < 0.0,
        );
        local_normal = transform_skin_normal(skin, local_normal);
        local_tangent = mat3x3<f32>(skin[0].xyz, skin[1].xyz, skin[2].xyz) * local_tangent;
    }

    let world_pos = (model * local_position).xyz;
    let world_tangent =
        mat3x3<f32>(model[0].xyz, model[1].xyz, model[2].xyz) * local_tangent;
    let model_handedness = select(
        1.0,
        -1.0,
        dot(model[0].xyz, cross(model[1].xyz, model[2].xyz)) < 0.0,
    );

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = v.color;
    out.tex_coords = v.uv_01.xy;
    out.tex_coords_1 = v.uv_01.zw;
    out.tex_coords_2 = v.uv_23.xy;
    out.tex_coords_3 = v.uv_23.zw;
    out.world_normal = safe_normalize(normal_matrix * local_normal, vec3<f32>(0.0, 1.0, 0.0));
    out.world_pos = world_pos;
    out.world_tangent = vec4<f32>(
        world_tangent,
        v.tangent.w * model_handedness * skin_handedness,
    );
    return out;
}

struct ShadowOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv_01: vec4<f32>,
    @location(1) uv_23: vec4<f32>,
};

@vertex
fn vs_shadow(v: VsIn, instance: InstanceInput) -> ShadowOut {
    let model = mat4x4<f32>(
        instance.model_matrix_0,
        instance.model_matrix_1,
        instance.model_matrix_2,
        instance.model_matrix_3,
    );
    let world_pos = model * get_local_position(v, instance);
    var out: ShadowOut;
    out.clip = shadow_matrix * world_pos;
    out.uv_01 = v.uv_01;
    out.uv_23 = v.uv_23;
    return out;
}

@fragment
fn fs_shadow(v: ShadowOut, @builtin(front_facing) front_facing: bool) {
    if !front_facing && material.double_sided == 0u {
        discard;
    }
    let uv = transform_uv(get_uv(
        material.base_color_tex_coord,
        v.uv_01.xy,
        v.uv_01.zw,
        v.uv_23.xy,
        v.uv_23.zw,
    ), material.base_color_uv_transform);
    let alpha = textureSample(t_diffuse, s_diffuse, uv).a * material.base_color.a;
    if material.alpha_mode != 0u && alpha < material.alpha_cutoff {
        discard;
    }
}

@fragment
fn fs_glass(
    v: VsOut,
    @builtin(front_facing) front_facing: bool,
) -> @location(0) vec4<f32> {
    let base_uv = transform_uv(get_uv(
        material.base_color_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.base_color_uv_transform);
    let metallic_roughness_uv = transform_uv(get_uv(
        material.metallic_roughness_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.metallic_roughness_uv_transform);
    let normal_uv = transform_uv(get_uv(
        material.normal_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.normal_uv_transform);
    let transmission_uv = transform_uv(get_uv(
        material.transmission_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.transmission_uv_transform);
    let specular_uv = transform_uv(get_uv(
        material.specular_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.specular_uv_transform);
    let specular_color_uv = transform_uv(get_uv(
        material.specular_color_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.specular_color_uv_transform);
    let emissive_uv = transform_uv(get_uv(
        material.emissive_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.emissive_uv_transform);
    let thickness_uv = transform_uv(get_uv(
        material.thickness_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.thickness_uv_transform);
    let tex_color = textureSample(t_diffuse, s_diffuse, base_uv);
    let surface_color = tex_color * vec4<f32>(v.color, 1.0) * material.base_color;
    let metallic_roughness = textureSample(
        t_metallic_roughness,
        s_metallic_roughness,
        metallic_roughness_uv,
    );
    if material.alpha_mode == 1u && surface_color.a < material.alpha_cutoff {
        discard;
    }
    if !front_facing && material.double_sided == 0u {
        discard;
    }

    var normal = safe_normalize(v.world_normal, vec3<f32>(0.0, 1.0, 0.0));
    if !front_facing {
        normal = -normal;
    }
    normal = apply_normal_map(
        normal,
        v.world_tangent,
        normal_uv,
        material.normal_uv_transform,
    );
    let debug_view = u32(light.fill_color.a + 0.5);
    if debug_view == 1u {
        return vec4<f32>(normal * 0.5 + 0.5, surface_color.a);
    }
    if debug_view == 2u {
        return surface_color;
    }
    let view_dir = safe_normalize(camera.view_pos.xyz - v.world_pos, normal);
    let roughness = clamp(material.roughness * metallic_roughness.g, 0.045, 1.0);
    if debug_view == 3u {
        return vec4<f32>(roughness, material.metallic, 0.0, surface_color.a);
    }
    let dielectric_f0 = pow((material.ior - 1.0) / (material.ior + 1.0), 2.0)
        * material.specular_factor * textureSample(t_specular, s_diffuse, specular_uv).a;
    let f0 = vec3<f32>(dielectric_f0)
        * material.specular_color_factor
        * textureSample(t_specular_color, s_diffuse, specular_color_uv).rgb;
    let glass_fresnel = fresnel_schlick(max(dot(normal, view_dir), 0.0), f0);
    let key_dir = safe_normalize(-light.direction_intensity.xyz, normal);
    let fill_dir = safe_normalize(-light.fill_direction_intensity.xyz, normal);
    let key_radiance = light.color.rgb * light.direction_intensity.w;
    let fill_radiance = light.fill_color.rgb * light.fill_direction_intensity.w;
    var key_visibility = 1.0;
    if light.color.a > 0.5 {
        key_visibility = shadow_visibility(v.world_pos, normal, key_dir);
    }
    let direct_reflection = evaluate_direct_light(
        normal,
        view_dir,
        key_dir,
        key_radiance,
        vec3<f32>(0.0),
        0.0,
        roughness,
        f0,
    ) * key_visibility
        + evaluate_direct_light(
            normal,
            view_dir,
            fill_dir,
            fill_radiance,
            vec3<f32>(0.0),
            0.0,
            roughness,
            f0,
        );
    let reflected_environment =
        sample_prefiltered_environment(reflect(-view_dir, normal), roughness) * light.ambient_top.a;
    let refraction_ratio = select(material.ior, 1.0 / material.ior, front_facing);
    let refracted_direction = refract(-view_dir, normal, refraction_ratio);
    let projected_origin = camera.view_proj * vec4<f32>(v.world_pos, 1.0);
    let projected_refracted = camera.view_proj
        * vec4<f32>(v.world_pos + refracted_direction * 0.65, 1.0);
    let screen_size = vec2<f32>(textureDimensions(t_scene_color));
    let screen_uv = v.clip.xy / screen_size;
    let refraction_offset = (
        projected_refracted.xy / projected_refracted.w
            - projected_origin.xy / projected_origin.w
    ) * vec2<f32>(0.5, -0.5);
    let refracted_uv = clamp(
        screen_uv + clamp(refraction_offset, vec2<f32>(-0.045), vec2<f32>(0.045)),
        vec2<f32>(0.001),
        vec2<f32>(0.999),
    );
    let background = textureSample(t_scene_color, s_scene_color, refracted_uv).rgb;
    let thickness = material.thickness_factor * textureSample(
        t_thickness,
        s_diffuse,
        thickness_uv,
    ).g;
    var volume_attenuation = vec3<f32>(1.0);
    if material.attenuation_distance > 0.0 && thickness > 0.0 {
        volume_attenuation = pow(
            max(material.attenuation_color, vec3<f32>(1e-4)),
            vec3<f32>(thickness / material.attenuation_distance),
        );
    }
    let transmitted = background * surface_color.rgb * volume_attenuation;
    let opaque_surface = surface_color.rgb * material.base_color.a;
    let transmission = clamp(
        material.transmission_factor * textureSample(t_transmission, s_diffuse, transmission_uv).r,
        0.0,
        1.0,
    );
    let body = mix(opaque_surface, transmitted, transmission);
    let emissive = material.emissive_factor * material.emissive_strength
        * textureSample(t_emissive, s_diffuse, emissive_uv).rgb;
    let color = body * (vec3<f32>(1.0) - glass_fresnel)
        + reflected_environment * glass_fresnel
        + direct_reflection
        + emissive;
    return vec4<f32>(color, 1.0);
}

@fragment
fn fs_main(
    v: VsOut,
    @builtin(front_facing) front_facing: bool,
) -> @location(0) vec4<f32> {
    let base_uv = transform_uv(get_uv(
        material.base_color_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.base_color_uv_transform);
    let metallic_roughness_uv = transform_uv(get_uv(
        material.metallic_roughness_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.metallic_roughness_uv_transform);
    let normal_uv = transform_uv(get_uv(
        material.normal_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.normal_uv_transform);
    let occlusion_uv = transform_uv(get_uv(
        material.occlusion_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.occlusion_uv_transform);
    let emissive_uv = transform_uv(get_uv(
        material.emissive_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.emissive_uv_transform);
    let specular_uv = transform_uv(get_uv(
        material.specular_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.specular_uv_transform);
    let specular_color_uv = transform_uv(get_uv(
        material.specular_color_tex_coord,
        v.tex_coords,
        v.tex_coords_1,
        v.tex_coords_2,
        v.tex_coords_3,
    ), material.specular_color_uv_transform);
    let tex_color = textureSample(t_diffuse, s_diffuse, base_uv);
    let surface_color = tex_color * vec4<f32>(v.color, 1.0) * material.base_color;
    let metallic_roughness = textureSample(
        t_metallic_roughness,
        s_metallic_roughness,
        metallic_roughness_uv,
    );
    if material.alpha_mode == 1u && surface_color.a < material.alpha_cutoff {
        discard;
    }
    if !front_facing && material.double_sided == 0u {
        discard;
    }

    var normal = safe_normalize(v.world_normal, vec3<f32>(0.0, 1.0, 0.0));
    if !front_facing {
        normal = -normal;
    }
    normal = apply_normal_map(
        normal,
        v.world_tangent,
        normal_uv,
        material.normal_uv_transform,
    );
    let debug_view = u32(light.fill_color.a + 0.5);
    if debug_view == 1u {
        return vec4<f32>(normal * 0.5 + 0.5, surface_color.a);
    }
    if debug_view == 2u {
        return surface_color;
    }
    let view_dir = safe_normalize(camera.view_pos.xyz - v.world_pos, normal);
    let metallic = clamp(material.metallic * metallic_roughness.b, 0.0, 1.0);
    var roughness = clamp(material.roughness * metallic_roughness.g, 0.045, 1.0);
    let specular_glossiness = textureSample(t_specular_color, s_diffuse, specular_color_uv);
    if material.pbr_workflow == 1u {
        roughness = clamp(
            1.0 - (1.0 - material.roughness) * specular_glossiness.a,
            0.045,
            1.0,
        );
    }
    if debug_view == 3u {
        return vec4<f32>(roughness, metallic, 0.0, surface_color.a);
    }
    if material.unlit > 0.5 {
        return surface_color;
    }
    let ior_f0 = pow((material.ior - 1.0) / (material.ior + 1.0), 2.0);
    var dielectric_f0 = vec3<f32>(ior_f0)
        * material.specular_color_factor
        * material.specular_factor
        * textureSample(t_specular, s_diffuse, specular_uv).a
        * textureSample(t_specular_color, s_diffuse, specular_color_uv).rgb;
    if material.pbr_workflow == 1u {
        dielectric_f0 = material.specular_color_factor * specular_glossiness.rgb;
    }
    let f0 = mix(dielectric_f0, surface_color.rgb, metallic);

    let key_dir = safe_normalize(-light.direction_intensity.xyz, normal);
    let fill_dir = safe_normalize(-light.fill_direction_intensity.xyz, normal);
    let key_radiance = light.color.rgb * light.direction_intensity.w;
    let fill_radiance = light.fill_color.rgb * light.fill_direction_intensity.w;

    var key_visibility = 1.0;
    if light.color.a > 0.5 {
        key_visibility = shadow_visibility(v.world_pos, normal, key_dir);
    }
    var radiance = evaluate_direct_light(
        normal, view_dir, key_dir, key_radiance, surface_color.rgb, metallic, roughness, f0,
    ) * key_visibility;
    radiance += evaluate_direct_light(
        normal, view_dir, fill_dir, fill_radiance, surface_color.rgb, metallic, roughness, f0,
    );

    let n_dot_v = max(dot(normal, view_dir), 0.0);
    let fresnel_roughness = f0
        + (max(vec3<f32>(1.0 - roughness), f0) - f0)
            * pow(1.0 - n_dot_v, 5.0);
    let diffuse_weight = (vec3<f32>(1.0) - fresnel_roughness) * (1.0 - metallic);
    let env_intensity = light.ambient_top.a;
    let ambient_occlusion = mix(
        1.0,
        textureSample(t_occlusion, s_diffuse, occlusion_uv).r,
        material.occlusion_strength,
    );
    let diffuse_ambient = diffuse_weight * surface_color.rgb
        * diffuse_irradiance(normal) * env_intensity
        * ambient_occlusion;
    let reflection_dir = reflect(-view_dir, normal);
    let environment = sample_prefiltered_environment(reflection_dir, roughness);
    let brdf = textureSampleLevel(t_brdf_lut, s_environment, vec2<f32>(n_dot_v, roughness), 0.0).rg;
    let single_scatter = f0 * brdf.x + vec3<f32>(brdf.y);
    let single_scatter_energy = brdf.x + brdf.y;
    let average_fresnel = f0 + (vec3<f32>(1.0) - f0) * 0.047619;
    let multiple_scatter = single_scatter
        * average_fresnel
        * (1.0 - single_scatter_energy)
        / max(vec3<f32>(1.0) - average_fresnel * (1.0 - single_scatter_energy), vec3<f32>(1e-4));
    let specular_ambient =
        environment * (single_scatter + multiple_scatter) * env_intensity
            * clamp(
                pow(n_dot_v + ambient_occlusion, exp2(-16.0 * roughness - 1.0))
                    - 1.0 + ambient_occlusion,
                0.0,
                1.0,
            );
    if debug_view == 4u {
        return vec4<f32>(diffuse_ambient, surface_color.a);
    }
    if debug_view == 5u {
        return vec4<f32>(specular_ambient, surface_color.a);
    }

    let emissive = material.emissive_factor * material.emissive_strength
        * textureSample(t_emissive, s_diffuse, emissive_uv).rgb;
    let color = radiance + diffuse_ambient + specular_ambient + emissive;
    return vec4<f32>(color, surface_color.a);
}
"#;

pub struct RenderPipelineBundle {
    pub pipeline: wgpu::RenderPipeline,
    pub transparent_pipeline: wgpu::RenderPipeline,
    pub glass_pipeline: wgpu::RenderPipeline,
    pub skybox_pipeline: wgpu::RenderPipeline,
    pub shadow_pipeline: wgpu::RenderPipeline,
    pub shared_bind_group_layout: wgpu::BindGroupLayout,
    pub shadow_bind_group_layout: wgpu::BindGroupLayout,
    pub texture_bind_group_layout: wgpu::BindGroupLayout,
    pub environment_bind_group_layout: wgpu::BindGroupLayout,
    pub scene_color_bind_group_layout: wgpu::BindGroupLayout,
    pub postprocess_pipeline: wgpu::RenderPipeline,
}

impl RenderPipelineBundle {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let shared_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("shared bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                        count: None,
                    },
                ],
            });

        let shadow_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("shadow shared bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 7,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 8,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 9,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 10,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 11,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                ],
                label: Some("texture_bind_group_layout"),
            });

        let environment_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("environment bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                ],
            });

        let scene_color_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("scene color bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline layout"),
            bind_group_layouts: &[
                &shared_bind_group_layout,
                &texture_bind_group_layout,
                &environment_bind_group_layout,
            ],
            push_constant_ranges: &[],
        });
        let glass_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glass pipeline layout"),
            bind_group_layouts: &[
                &shared_bind_group_layout,
                &texture_bind_group_layout,
                &environment_bind_group_layout,
                &scene_color_bind_group_layout,
            ],
            push_constant_ranges: &[],
        });
        let postprocess_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("postprocess pipeline layout"),
            bind_group_layouts: &[
                &shared_bind_group_layout,
                &texture_bind_group_layout,
                &environment_bind_group_layout,
                &scene_color_bind_group_layout,
            ],
            push_constant_ranges: &[],
        });
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow pipeline layout"),
            bind_group_layouts: &[&shadow_bind_group_layout, &texture_bind_group_layout],
            push_constant_ranges: &[],
        });

        // Импортируем InstanceRaw layout через супер-модуль state или определяем структуру
        // Но проще передать макет или завязаться на state. Здесь используем супермодуль:
        use crate::renderer::mesh::TangentVertex;
        use crate::renderer::state::InstanceRaw;

        let create_pipeline = |label, pipeline_layout, entry_point, blend, depth_write_enabled| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[
                        Vertex::layout(),
                        InstanceRaw::layout(),
                        crate::renderer::mesh::SkinVertex::layout(),
                        TangentVertex::layout(),
                        UvSetsVertex::layout(),
                    ],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry_point),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled,
                    depth_compare: wgpu::CompareFunction::Less,
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: MSAA_SAMPLES,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview: None,
                cache: None,
            })
        };
        let pipeline = create_pipeline("opaque pipeline", &layout, "fs_main", None, true);
        let transparent_pipeline = create_pipeline(
            "transparent pipeline",
            &layout,
            "fs_main",
            Some(wgpu::BlendState::ALPHA_BLENDING),
            false,
        );
        let glass_pipeline =
            create_pipeline("glass pipeline", &glass_layout, "fs_glass", None, false);
        let postprocess_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("HDR tone mapping pipeline"),
            layout: Some(&postprocess_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_postprocess"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_postprocess"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let skybox_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("environment background pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_skybox"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_skybox"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLES,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
            cache: None,
        });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow pipeline"),
            layout: Some(&shadow_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_shadow"),
                buffers: &[
                    Vertex::layout(),
                    InstanceRaw::layout(),
                    crate::renderer::mesh::SkinVertex::layout(),
                    TangentVertex::layout(),
                    UvSetsVertex::layout(),
                ],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_shadow"),
                targets: &[],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            pipeline,
            transparent_pipeline,
            glass_pipeline,
            skybox_pipeline,
            shadow_pipeline,
            shared_bind_group_layout,
            shadow_bind_group_layout,
            texture_bind_group_layout,
            environment_bind_group_layout,
            scene_color_bind_group_layout,
            postprocess_pipeline,
        }
    }
}
