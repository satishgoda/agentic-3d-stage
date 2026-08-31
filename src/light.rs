//! Directional Lambert lighting + shadow map. Not RTX.

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

pub const SHADOW_MAP_SIZE: u32 = 2048;
pub const DEFAULT_DIRECTION: [f32; 3] = [-0.35, -0.85, -0.25];
pub const SHADOW_BIAS: f32 = 0.0004;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SceneLight {
    /// World-space travel direction (sun toward ground).
    pub direction: [f32; 3],
    #[serde(default = "default_true")]
    pub shadows: bool,
}

fn default_true() -> bool {
    true
}

impl Default for SceneLight {
    fn default() -> Self {
        Self {
            direction: DEFAULT_DIRECTION,
            shadows: true,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FrameUniforms {
    pub view_proj: [[f32; 4]; 4],
    pub light_view_proj: [[f32; 4]; 4],
    pub light_dir: [f32; 4],
    /// constant z-bias, shadows_on (1/0), 1/shadow_map_size, world texel metres
    pub shadow: [f32; 4],
}

pub fn direction_vec(light: &SceneLight) -> Vec3 {
    Vec3::from_array(light.direction)
        .try_normalize()
        .unwrap_or(Vec3::NEG_Y)
}

/// Floors and smiley bits do not cast (they caused shard acne). Cubes/spheres do.
pub fn casts_shadow(recipe: &str, size: [f32; 3]) -> bool {
    if recipe == "empty" {
        return false;
    }
    let ax = size[0].abs();
    let ay = size[1].abs();
    let az = size[2].abs();
    let max_e = ax.max(ay).max(az);
    if max_e < 0.35 {
        return false;
    }
    // Thin slabs (ground box / pad plane) receive but do not cast.
    if ay < 0.25 && ax.max(az) > 1.5 {
        return false;
    }
    true
}

/// Ortho camera looking along the light travel direction at `center`.
pub fn light_view_proj(light: &SceneLight, center: Vec3, radius: f32) -> Mat4 {
    let d = direction_vec(light);
    let radius = radius.clamp(4.0, 24.0);
    let texel = (2.0 * radius) / SHADOW_MAP_SIZE as f32;
    let snap = |v: f32| (v / texel).floor() * texel;
    let center = Vec3::new(snap(center.x), snap(center.y), snap(center.z));
    let eye = center - d * (radius * 2.2);
    let up = if d.y.abs() > 0.92 { Vec3::Z } else { Vec3::Y };
    let view = Mat4::look_at_rh(eye, center, up);
    let ortho = Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.5, radius * 5.0);
    ortho * view
}

pub fn pack_frame(view_proj: Mat4, light: &SceneLight, center: Vec3, radius: f32) -> FrameUniforms {
    let radius = radius.clamp(4.0, 24.0);
    let lvp = light_view_proj(light, center, radius);
    let d = direction_vec(light);
    let world_texel = (2.0 * radius) / SHADOW_MAP_SIZE as f32;
    FrameUniforms {
        view_proj: view_proj.to_cols_array_2d(),
        light_view_proj: lvp.to_cols_array_2d(),
        light_dir: [d.x, d.y, d.z, 0.0],
        shadow: [
            SHADOW_BIAS,
            if light.shadows { 1.0 } else { 0.0 },
            1.0 / SHADOW_MAP_SIZE as f32,
            world_texel,
        ],
    }
}

/// Depth-only shadow caster. Group 0 = FrameUniforms (uses light_view_proj).
pub const SHADOW_WGSL: &str = r#"
struct FrameUniforms {
    view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    light_dir: vec4<f32>,
    shadow: vec4<f32>,
};
struct EntityUniforms {
    model: mat4x4<f32>,
    color: vec4<f32>,
    roughness: f32,
    metallic: f32,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> frame: FrameUniforms;
@group(1) @binding(0) var<uniform> entity: EntityUniforms;
@vertex
fn vs_shadow(@location(0) position: vec3<f32>, @location(1) _n: vec3<f32>) -> @builtin(position) vec4<f32> {
    let world = entity.model * vec4<f32>(position, 1.0);
    return frame.light_view_proj * world;
}
"#;

/// Lit scene with directional Lambert/spec and 3×3 PCF shadows.
pub const SCENE_WGSL: &str = r#"
struct FrameUniforms {
    view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    light_dir: vec4<f32>,
    shadow: vec4<f32>,
};
struct EntityUniforms {
    model: mat4x4<f32>,
    color: vec4<f32>,
    roughness: f32,
    metallic: f32,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> frame: FrameUniforms;
@group(0) @binding(1) var shadow_map: texture_depth_2d;
@group(0) @binding(2) var shadow_samp: sampler_comparison;
@group(1) @binding(0) var<uniform> entity: EntityUniforms;
struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};
struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
};
@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    let world = entity.model * vec4<f32>(input.position, 1.0);
    out.clip_position = frame.view_proj * world;
    out.world_pos = world.xyz;
    out.world_normal = normalize((entity.model * vec4<f32>(input.normal, 0.0)).xyz);
    return out;
}
fn shadow_texel(uv: vec2<f32>, ref_depth: f32) -> f32 {
    if (uv.x <= 0.0 || uv.x >= 1.0 || uv.y <= 0.0 || uv.y >= 1.0) {
        return 1.0;
    }
    return textureSampleCompareLevel(shadow_map, shadow_samp, uv, ref_depth);
}
fn visibility(world: vec3<f32>, n: vec3<f32>) -> f32 {
    if (frame.shadow.y < 0.5) {
        return 1.0;
    }
    let l = normalize(-frame.light_dir.xyz);
    let ndotl = max(dot(n, l), 0.0);
    // Slope-scaled receiver bias. Facing the sun (the pad) gets almost no lift,
    // so contact shadows stay glued. Grazing sides get more to kill acne.
    let sin_t = sqrt(max(1.0 - ndotl * ndotl, 0.0));
    let n_off = n * frame.shadow.w * (0.4 + 3.0 * sin_t * sin_t);
    let lp = frame.light_view_proj * vec4<f32>(world + n_off, 1.0);
    let w = max(abs(lp.w), 1e-6);
    let ndc = lp.xyz / w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, ndc.y * 0.5 + 0.5);
    let z_bias = frame.shadow.x * (0.5 + 8.0 * sin_t);
    let ref_d = ndc.z - z_bias;
    let texel = frame.shadow.z;
    var s = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let o = vec2<f32>(f32(x), f32(y)) * texel;
            s = s + shadow_texel(uv + o, ref_d);
        }
    }
    return s / 9.0;
}
@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(input.world_normal);
    let l = normalize(-frame.light_dir.xyz);
    let vis = visibility(input.world_pos, n);
    let ndotl = max(dot(n, l), 0.0);
    let r = clamp(entity.roughness, 0.04, 1.0);
    let m = clamp(entity.metallic, 0.0, 1.0);
    let ambient = 0.18 + 0.14 * r;
    let diff = (1.0 - m) * ndotl * vis;
    let spec = pow(ndotl, mix(64.0, 4.0, r)) * mix(0.06, 0.55, m) * vis;
    let albedo = entity.color.rgb;
    let lit = albedo * (ambient + 0.82 * diff) + vec3<f32>(spec) * mix(vec3<f32>(1.0, 1.0, 1.0), albedo, m);
    return vec4<f32>(lit, entity.color.a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ground_and_bits_do_not_cast() {
        assert!(!casts_shadow("empty", [0.0, 0.0, 0.0]));
        assert!(!casts_shadow("box", [8.0, 0.1, 8.0]));
        assert!(!casts_shadow("plane", [2.0, 0.0, 2.0]));
        assert!(!casts_shadow("box", [0.12, 0.12, 0.08]));
        assert!(casts_shadow("box", [1.0, 1.0, 1.0]));
        assert!(casts_shadow("sphere", [0.9, 0.9, 0.9]));
    }
}
