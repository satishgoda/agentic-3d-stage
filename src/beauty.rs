//! Offscreen authored-camera beauty (Phase C). Does not mutate the document.

use crate::camera;
use crate::document::Document;
use crate::light::{self, SHADOW_MAP_SIZE};
use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use wgpu::util::DeviceExt;

pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;
pub const DEFAULT_WIDTH: u32 = 960;
pub const DEFAULT_HEIGHT: u32 = 640;
pub const DEFAULT_OUT_DIR: &str = "renders";

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const ID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.08,
    g: 0.09,
    b: 0.11,
    a: 1.0,
};
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EntityUniforms {
    model: [[f32; 4]; 4],
    color: [f32; 4],
    roughness: f32,
    metallic: f32,
    _pad: [f32; 2],
}

#[derive(Debug, Clone)]
pub struct BeautyFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub revision: u64,
    pub camera: &'static str,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RenderResult {
    pub ok: bool,
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    pub camera: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    id_pipeline: wgpu::RenderPipeline,
    scene_frame_layout: wgpu::BindGroupLayout,
    simple_frame_layout: wgpu::BindGroupLayout,
    entity_layout: wgpu::BindGroupLayout,
    shadow_sampler: wgpu::Sampler,
}

static GPU: Mutex<Option<Gpu>> = Mutex::new(None);

pub fn clamp_size(width: u32, height: u32) -> (u32, u32) {
    (
        width.clamp(1, MAX_WIDTH),
        height.clamp(1, MAX_HEIGHT),
    )
}

pub fn render_beauty(doc: &Document, width: u32, height: u32) -> Result<BeautyFrame, String> {
    let (width, height) = clamp_size(width, height);
    let gpu_guard = ensure_gpu()?;
    let gpu = gpu_guard.as_ref().ok_or("beauty_gpu_missing")?;

    let color = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("beauty color"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("beauty depth"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

    let aspect = width as f32 / height as f32;
    let (center, radius) = doc.light_fit();
    let uniforms = light::pack_frame(
        camera::view_proj_matrix(aspect),
        &doc.light,
        center,
        radius,
    );
    let frame_buffer = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("beauty frame"),
        contents: bytemuck::bytes_of(&uniforms),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let shadow = make_shadow_map(&gpu.device);
    let shadow_view = shadow.create_view(&wgpu::TextureViewDescriptor::default());
    let simple_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("beauty shadow frame"),
        layout: &gpu.simple_frame_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: frame_buffer.as_entire_binding(),
        }],
    });
    let scene_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("beauty scene frame"),
        layout: &gpu.scene_frame_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&shadow_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&gpu.shadow_sampler),
            },
        ],
    });

    let (meshes, _names) = build_meshes(&gpu.device, &gpu.entity_layout, doc, false);

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("beauty encoder"),
        });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("beauty shadow"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &shadow_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&gpu.shadow_pipeline);
        pass.set_bind_group(0, &simple_bind, &[]);
        for mesh in &meshes {
            if !mesh.cast_shadow {
                continue;
            }
            pass.set_bind_group(1, &mesh.bind, &[]);
            pass.set_vertex_buffer(0, mesh.vb.slice(..));
            pass.set_index_buffer(mesh.ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..mesh.n, 0, 0..1);
        }
    }
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("beauty pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, &scene_bind, &[]);
        for mesh in &meshes {
            pass.set_bind_group(1, &mesh.bind, &[]);
            pass.set_vertex_buffer(0, mesh.vb.slice(..));
            pass.set_index_buffer(mesh.ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..mesh.n, 0, 0..1);
        }
    }

    let unpadded = 4 * width;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let buf_size = padded as u64 * height as u64;
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("beauty readback"),
        size: buf_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(std::iter::once(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|_| "beauty_map_recv".to_string())?
        .map_err(|e| format!("beauty_map:{e}"))?;
    let data = slice.get_mapped_range();
    let mut rgba = vec![0u8; (unpadded * height) as usize];
    for y in 0..height as usize {
        let src = y * padded as usize;
        let dst = y * unpadded as usize;
        rgba[dst..dst + unpadded as usize]
            .copy_from_slice(&data[src..src + unpadded as usize]);
    }
    drop(data);
    readback.unmap();

    Ok(BeautyFrame {
        width,
        height,
        rgba,
        revision: doc.revision,
        camera: "authored",
    })
}

pub fn write_png(path: impl AsRef<Path>, frame: &BeautyFrame) -> Result<PathBuf, String> {
    let path = path.as_ref();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("beauty_mkdir:{e}"))?;
    }
    let file = fs::File::create(path).map_err(|e| format!("beauty_create:{e}"))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), frame.width, frame.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| format!("beauty_png:{e}"))?;
    w.write_image_data(&frame.rgba)
        .map_err(|e| format!("beauty_png:{e}"))?;
    Ok(path.to_path_buf())
}

pub fn default_png_path(revision: u64, width: u32, height: u32) -> PathBuf {
    PathBuf::from(DEFAULT_OUT_DIR).join(format!("beauty-r{revision}-{width}x{height}.png"))
}

pub fn render_to_default_png(
    doc: &Document,
    width: u32,
    height: u32,
) -> Result<RenderResult, String> {
    let frame = render_beauty(doc, width, height)?;
    let path = default_png_path(frame.revision, frame.width, frame.height);
    write_png(&path, &frame)?;
    let mut digest = digest_frame(&frame);
    if let Ok(prev) = LAST.lock() {
        if let Some(prev) = prev.as_ref() {
            digest.diff_from_previous = Some(diff_frames(prev, &frame));
        }
    }
    if let Ok(mut slot) = LAST.lock() {
        *slot = Some(frame.clone());
    }
    if let Ok(mut slot) = LAST_DIGEST.lock() {
        *slot = Some(digest);
    }
    Ok(RenderResult {
        ok: true,
        path: path.display().to_string(),
        width: frame.width,
        height: frame.height,
        revision: frame.revision,
        camera: frame.camera.into(),
        error: None,
    })
}

static LAST: Mutex<Option<BeautyFrame>> = Mutex::new(None);
static LAST_DIGEST: Mutex<Option<BeautyDigest>> = Mutex::new(None);

pub fn last_frame() -> Option<BeautyFrame> {
    LAST.lock().ok().and_then(|g| g.clone())
}

pub fn last_digest() -> Option<BeautyDigest> {
    LAST_DIGEST.lock().ok().and_then(|g| g.clone())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PixelForecast {
    pub pixels_will_move: bool,
    pub changed_pixels: u32,
    pub max_delta: u32,
    pub bbox: Option<[u32; 4]>,
}

pub fn forecast_from_frames(before: &BeautyFrame, after: &BeautyFrame) -> PixelForecast {
    let d = diff_frames(before, after);
    PixelForecast {
        pixels_will_move: d.max_delta > 2 && d.changed_pixels > 0,
        changed_pixels: d.changed_pixels,
        max_delta: d.max_delta,
        bbox: d.bbox,
    }
}

pub fn forecast_documents(before: &Document, after: &Document) -> Option<PixelForecast> {
    let a = render_beauty(before, 160, 100).ok()?;
    let b = render_beauty(after, 160, 100).ok()?;
    Some(forecast_from_frames(&a, &b))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RgbProbe {
    pub x: u32,
    pub y: u32,
    pub rgb: [u8; 3],
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceDiff {
    pub changed_pixels: u32,
    pub max_delta: u32,
    pub bbox: Option<[u32; 4]>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BeautyDigest {
    pub hash: String,
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    pub camera: String,
    pub clip: u32,
    pub black: u32,
    pub mean_luma: f32,
    pub probes: Vec<RgbProbe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_from_previous: Option<EvidenceDiff>,
}

pub fn digest_frame(frame: &BeautyFrame) -> BeautyDigest {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&frame.rgba);
    let hash = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let n = (frame.width * frame.height).max(1);
    let mut clip = 0u32;
    let mut black = 0u32;
    let mut luma_sum = 0f32;
    for px in frame.rgba.chunks_exact(4) {
        if px[0] == 0 || px[0] == 255 || px[1] == 0 || px[1] == 255 || px[2] == 0 || px[2] == 255 {
            clip += 1;
        }
        let luma = 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32;
        luma_sum += luma;
        if luma < 3.0 {
            black += 1;
        }
    }
    let probes = default_probe_coords(frame.width, frame.height)
        .into_iter()
        .map(|(x, y)| RgbProbe {
            x,
            y,
            rgb: sample_rgb(frame, x, y),
        })
        .collect();
    BeautyDigest {
        hash,
        width: frame.width,
        height: frame.height,
        revision: frame.revision,
        camera: frame.camera.into(),
        clip,
        black,
        mean_luma: luma_sum / n as f32,
        probes,
        diff_from_previous: None,
    }
}

pub fn diff_frames(a: &BeautyFrame, b: &BeautyFrame) -> EvidenceDiff {
    let w = a.width.min(b.width);
    let h = a.height.min(b.height);
    let mut changed = 0u32;
    let mut max_delta = 0u32;
    let mut minx = w;
    let mut miny = h;
    let mut maxx = 0u32;
    let mut maxy = 0u32;
    for y in 0..h {
        for x in 0..w {
            let pa = sample_rgb(a, x, y);
            let pb = sample_rgb(b, x, y);
            let d = pa
                .iter()
                .zip(pb.iter())
                .map(|(l, r)| l.abs_diff(*r) as u32)
                .max()
                .unwrap_or(0);
            if d > 0 {
                changed += 1;
                max_delta = max_delta.max(d);
                minx = minx.min(x);
                miny = miny.min(y);
                maxx = maxx.max(x);
                maxy = maxy.max(y);
            }
        }
    }
    EvidenceDiff {
        changed_pixels: changed,
        max_delta,
        bbox: if changed > 0 {
            Some([minx, miny, maxx, maxy])
        } else {
            None
        },
    }
}

fn sample_rgb(frame: &BeautyFrame, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * frame.width + x) * 4) as usize;
    if i + 2 >= frame.rgba.len() {
        return [0, 0, 0];
    }
    [frame.rgba[i], frame.rgba[i + 1], frame.rgba[i + 2]]
}

fn default_probe_coords(w: u32, h: u32) -> Vec<(u32, u32)> {
    let w = w.max(1);
    let h = h.max(1);
    vec![
        (w / 2, h / 2),
        (w / 4, h / 2),
        (3 * w / 4, h / 2),
        (w / 2, h / 3),
    ]
}

struct MeshBuf {
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    n: u32,
    bind: wgpu::BindGroup,
    cast_shadow: bool,
}

fn pack_id_color(index: u32) -> [f32; 4] {
    let i = index + 1;
    [
        (i & 0xff) as f32 / 255.0,
        ((i >> 8) & 0xff) as f32 / 255.0,
        0.0,
        1.0,
    ]
}

fn unpack_id(rgb: [u8; 3]) -> u32 {
    rgb[0] as u32 + ((rgb[1] as u32) << 8)
}

#[derive(Debug, Clone)]
pub struct IdBuffer {
    pub width: u32,
    pub height: u32,
    pub ids: Vec<u32>,
    pub names: Vec<String>,
}

impl IdBuffer {
    pub fn pick(&self, x: u32, y: u32) -> Option<&str> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let id = self.ids[(y * self.width + x) as usize];
        if id == 0 {
            return None;
        }
        self.names.get((id - 1) as usize).map(|s| s.as_str())
    }
}

pub fn render_ids(doc: &Document, width: u32, height: u32) -> Result<IdBuffer, String> {
    let (width, height) = clamp_size(width, height);
    let gpu_guard = ensure_gpu()?;
    let gpu = gpu_guard.as_ref().ok_or("beauty_gpu_missing")?;

    let color = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("id color"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: ID_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("id depth"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
    let aspect = width as f32 / height as f32;
    let (center, radius) = doc.light_fit();
    let uniforms = light::pack_frame(
        camera::view_proj_matrix(aspect),
        &doc.light,
        center,
        radius,
    );
    let frame_buffer = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("id frame"),
        contents: bytemuck::bytes_of(&uniforms),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let frame_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("id frame bind"),
        layout: &gpu.simple_frame_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: frame_buffer.as_entire_binding(),
        }],
    });
    let (meshes, names) = build_meshes(&gpu.device, &gpu.entity_layout, doc, true);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("id encoder"),
        });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("id pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&gpu.id_pipeline);
        pass.set_bind_group(0, &frame_bind, &[]);
        for mesh in &meshes {
            pass.set_bind_group(1, &mesh.bind, &[]);
            pass.set_vertex_buffer(0, mesh.vb.slice(..));
            pass.set_index_buffer(mesh.ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..mesh.n, 0, 0..1);
        }
    }
    let unpadded = 4 * width;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("id readback"),
        size: padded as u64 * height as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(std::iter::once(encoder.finish()));
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|_| "id_map_recv".to_string())?
        .map_err(|e| format!("id_map:{e}"))?;
    let data = slice.get_mapped_range();
    let mut ids = vec![0u32; (width * height) as usize];
    for y in 0..height as usize {
        let src = y * padded as usize;
        for x in 0..width as usize {
            let i = src + x * 4;
            ids[y * width as usize + x] = unpack_id([data[i], data[i + 1], data[i + 2]]);
        }
    }
    drop(data);
    readback.unmap();
    Ok(IdBuffer {
        width,
        height,
        ids,
        names,
    })
}

fn build_meshes(
    device: &wgpu::Device,
    entity_layout: &wgpu::BindGroupLayout,
    doc: &Document,
    pack_ids: bool,
) -> (Vec<MeshBuf>, Vec<String>) {
    let mut meshes = Vec::new();
    let mut names = Vec::new();
    for entity in doc.entities() {
        if entity.mesh.recipe == "empty" {
            continue;
        }
        let Ok((pos, nrm, indices)) = crate::geom::mesh(&entity.mesh.recipe, entity.mesh.size)
        else {
            continue;
        };
        let vertices: Vec<Vertex> = pos
            .into_iter()
            .zip(nrm)
            .map(|(position, normal)| Vertex { position, normal })
            .collect();
        let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("beauty verts"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("beauty idx"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let model = doc
            .world_matrix(&entity.id)
            .unwrap_or_else(|| Document::local_matrix(&entity.transform));
        let surf = doc.resolved_surface(entity);
        let color = if pack_ids {
            pack_id_color(names.len() as u32)
        } else {
            surf.color
        };
        names.push(entity.id.clone());
        let eu = EntityUniforms {
            model: model.to_cols_array_2d(),
            color,
            roughness: if pack_ids { 0.5 } else { surf.roughness },
            metallic: if pack_ids { 0.0 } else { surf.metallic },
            _pad: [0.0, 0.0],
        };
        let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("beauty entity"),
            contents: bytemuck::bytes_of(&eu),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("beauty entity bind"),
            layout: entity_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ub.as_entire_binding(),
            }],
        });
        meshes.push(MeshBuf {
            vb,
            ib,
            n: indices.len() as u32,
            bind,
            cast_shadow: crate::light::casts_shadow(&entity.mesh.recipe, entity.mesh.size),
        });
    }
    (meshes, names)
}

fn ensure_gpu() -> Result<std::sync::MutexGuard<'static, Option<Gpu>>, String> {
    let mut slot = GPU.lock().map_err(|_| "beauty_gpu_poisoned")?;
    if slot.is_some() {
        return Ok(slot);
    }
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .or_else(|| {
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        }))
    })
    .ok_or("beauty: no GPU adapter")?;
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("beauty device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                .using_resolution(adapter.limits()),
            memory_hints: Default::default(),
        },
        None,
    ))
    .map_err(|e| format!("beauty_device:{e}"))?;

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("beauty shader"),
        source: wgpu::ShaderSource::Wgsl(light::SCENE_WGSL.into()),
    });
    let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("beauty shadow shader"),
        source: wgpu::ShaderSource::Wgsl(light::SHADOW_WGSL.into()),
    });
    let simple_frame_layout = simple_frame_layout(&device);
    let scene_frame_layout = scene_frame_layout(&device);
    let entity_layout = entity_layout(&device);
    let shadow_sampler = shadow_comparison_sampler(&device);
    let scene_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("beauty scene layout"),
        bind_group_layouts: &[&scene_frame_layout, &entity_layout],
        push_constant_ranges: &[],
    });
    let simple_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("beauty simple layout"),
        bind_group_layouts: &[&simple_frame_layout, &entity_layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("beauty pipeline"),
        layout: Some(&scene_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: COLOR_FORMAT,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });
    let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("beauty shadow pipeline"),
        layout: Some(&simple_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shadow_shader,
            entry_point: Some("vs_shadow"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
            }],
            compilation_options: Default::default(),
        },
        fragment: None,
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });
    let id_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("id shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER_ID.into()),
    });
    let id_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("id pipeline"),
        layout: Some(&simple_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &id_shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &id_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: ID_FORMAT,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });
    *slot = Some(Gpu {
        device,
        queue,
        pipeline,
        shadow_pipeline,
        id_pipeline,
        scene_frame_layout,
        simple_frame_layout,
        entity_layout,
        shadow_sampler,
    });
    Ok(slot)
}

fn simple_frame_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("simple frame"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

fn scene_frame_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("scene frame"),
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
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
        ],
    })
}

fn entity_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("entity layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

fn shadow_comparison_sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("shadow compare"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        compare: Some(wgpu::CompareFunction::LessEqual),
        ..Default::default()
    })
}

fn make_shadow_map(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shadow map"),
        size: wgpu::Extent3d {
            width: SHADOW_MAP_SIZE,
            height: SHADOW_MAP_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

const SHADER_ID: &str = r#"
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
struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};
struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
};
@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    let world = entity.model * vec4<f32>(input.position, 1.0);
    out.clip_position = frame.view_proj * world;
    return out;
}
@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return entity.color;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    #[test]
    fn bootstrap_beauty_is_not_a_black_frame() {
        let doc = Document::bootstrap();
        let frame = render_beauty(&doc, 128, 96).expect("beauty render");
        assert_eq!(frame.camera, "authored");
        assert_eq!(frame.revision, 1);
        assert_eq!(frame.rgba.len(), 128 * 96 * 4);
        let mut warm = 0u32;
        let mut sum = 0u64;
        for px in frame.rgba.chunks_exact(4) {
            sum += px[0] as u64 + px[1] as u64 + px[2] as u64;
            if px[0] > 80 && px[0] > px[2] {
                warm += 1;
            }
        }
        let mean = sum as f32 / (frame.rgba.len() as f32 / 4.0 * 3.0);
        assert!(mean > 8.0, "mean luma too dark: {mean}");
        assert!(warm > 40, "expected terracotta pixels, got {warm}");
        assert_eq!(doc.revision, 1, "render must not mutate the document");
    }

    #[test]
    fn digest_has_hash_probes_and_stable_hash() {
        let doc = Document::bootstrap();
        let frame = render_beauty(&doc, 96, 64).expect("beauty");
        let a = digest_frame(&frame);
        let b = digest_frame(&frame);
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.hash.len(), 64);
        assert_eq!(a.probes.len(), 4);
        assert!(a.mean_luma > 8.0);
        assert_eq!(a.revision, 1);
        assert_eq!(a.camera, "authored");
    }

    #[test]
    fn object_id_pass_finds_box_1() {
        let doc = Document::bootstrap();
        let buf = render_ids(&doc, 128, 96).expect("id pass");
        assert!(buf.names.iter().any(|n| n == "box-1"));
        let mut found = false;
        for y in 0..buf.height {
            for x in 0..buf.width {
                if buf.pick(x, y) == Some("box-1") {
                    found = true;
                    break;
                }
            }
            if found {
                break;
            }
        }
        assert!(found, "no pixel encoded as box-1");
        assert_eq!(doc.revision, 1);
    }

    #[test]
    fn graph_bind_patch_changes_beauty_hash() {
        use crate::document::{ApplyChange, ApplyRequest};
        let mut doc = Document::bootstrap();
        let before = render_beauty(&doc, 96, 64).expect("before");
        let h0 = digest_frame(&before).hash;
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 1,
                idempotency_key: "g".into(),
                label: "bind green".into(),
                changes: vec![
                    ApplyChange::GraphCreate {
                        graph_id: "mat-1".into(),
                    },
                    ApplyChange::GraphBind {
                        graph_id: "mat-1".into(),
                        entity_id: "box-1".into(),
                    },
                    ApplyChange::GraphPatch {
                        graph_id: "mat-1".into(),
                        node_id: "principled".into(),
                        socket: "base_color_r".into(),
                        value: 0.1,
                    },
                    ApplyChange::GraphPatch {
                        graph_id: "mat-1".into(),
                        node_id: "principled".into(),
                        socket: "base_color_g".into(),
                        value: 0.9,
                    },
                    ApplyChange::GraphPatch {
                        graph_id: "mat-1".into(),
                        node_id: "principled".into(),
                        socket: "base_color_b".into(),
                        value: 0.2,
                    },
                ],
                dry_run: false,
            })
            .ok);
        let after = render_beauty(&doc, 96, 64).expect("after");
        let h1 = digest_frame(&after).hash;
        assert_ne!(h0, h1, "graph color must change beauty pixels");
    }

    #[test]
    fn shadow_map_darkens_mean_luma() {
        let mut on = Document::bootstrap();
        on.light.shadows = true;
        let mut off = Document::bootstrap();
        off.light.shadows = false;
        let a = digest_frame(&render_beauty(&on, 160, 100).expect("shadows on"));
        let b = digest_frame(&render_beauty(&off, 160, 100).expect("shadows off"));
        assert_ne!(a.hash, b.hash, "shadows must change beauty pixels");
        assert!(
            a.mean_luma + 0.4 < b.mean_luma,
            "shadows-on luma {} should be darker than off {}",
            a.mean_luma,
            b.mean_luma
        );
    }
}
