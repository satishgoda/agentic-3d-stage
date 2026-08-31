//! wgpu + winit authoring viewport (scene + mailbox live-feed HUD).

use thinner_floor::camera;
use thinner_floor::document::{Document, DocumentStore};
use thinner_floor::feed::MailboxFeed;
use thinner_floor::cycles_stream;
use thinner_floor::light::{self, FrameUniforms, SHADOW_MAP_SIZE};
use thinner_floor::hud::{self, HudHit, HudPanel};
use glam::Mat4;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState};
use winit::window::{Window, WindowId, WindowLevel};

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct EntityUniforms {
    model: [[f32; 4]; 4],
    color: [f32; 4],
    roughness: f32,
    metallic: f32,
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct HudUniforms {
    screen: [f32; 2],
    _pad: [f32; 2],
    /// x, y, w, h in pixels (top-left origin).
    rect: [f32; 4],
}

struct GpuMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    entity_bind_group: wgpu::BindGroup,
    cast_shadow: bool,
}

struct CyclesGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    texture: wgpu::Texture,
    tex_w: u32,
    tex_h: u32,
    last_seq: u32,
    last_sample: u32,
    wait_uniform: wgpu::Buffer,
    wait_texture: wgpu::Texture,
    wait_bind: wgpu::BindGroup,
    wait_w: u32,
    wait_h: u32,
    wait_label: String,
}

struct HudGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    texture: wgpu::Texture,
    tex_w: u32,
    tex_h: u32,
    last_seq: u64,
    last_active: bool,
    last_count: usize,
}

struct SurfaceState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    scene_bind_group: wgpu::BindGroup,
    simple_bind_group: wgpu::BindGroup,
    frame_uniform: wgpu::Buffer,
    entity_layout: wgpu::BindGroupLayout,
    meshes: Vec<GpuMesh>,
    drawn_revision: u64,
    size: winit::dpi::PhysicalSize<u32>,
    depth_view: wgpu::TextureView,
    #[allow(dead_code)]
    shadow_tex: wgpu::Texture,
    shadow_view: wgpu::TextureView,
    hud: HudGpu,
    cycles: CyclesGpu,
}

const HUD_DRAG_THRESHOLD: f32 = 4.0;
const HUD_DOUBLE_CLICK: Duration = Duration::from_millis(350);
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

pub struct ViewportApp {
    store: Arc<Mutex<DocumentStore>>,
    feed: Arc<Mutex<MailboxFeed>>,
    stop_flag: Arc<AtomicBool>,
    state: Option<SurfaceState>,
    last_poll: Instant,
    title_base: String,
    hud_visible: bool,
    modifiers: ModifiersState,
    hud_panel: HudPanel,
    hud_cursor: (f32, f32),
    hud_pressed: bool,
    hud_press_pos: (f32, f32),
    hud_press_hit: HudHit,
    hud_dragging: bool,
    hud_grab: (f32, f32),
    hud_last_title_click: Option<Instant>,
    hud_press_consumed: bool,
}

impl ViewportApp {
    fn cycles_hotkey_toggle(&mut self) {
        let doc = self.store.lock().ok().map(|g| g.document.clone());
        let Some(doc) = doc else {
            return;
        };
        if let Ok(mut h) = cycles_stream::host().lock() {
            let st = h.snapshot();
            match st.state.as_str() {
                "running" => {
                    h.pause();
                }
                "paused" => {
                    h.resume();
                }
                _ => {
                    let st = h.start(&doc, 64, 960, 640);
                    if let Ok(mut feed) = self.feed.lock() {
                        feed.push(thinner_floor::feed::FeedEvent {
                            op: "cycles".into(),
                            ok: st.ok,
                            elapsed_ms: 0.0,
                            revision: None,
                            summary: if st.ok {
                                "starting — first sample can take a few seconds".into()
                            } else {
                                st.error.clone().unwrap_or_else(|| "start failed".into())
                            },
                            hits: Vec::new(),
                            at: Instant::now(),
                        });
                    }
                }
            }
        }
    }

    pub fn run(
        store: Arc<Mutex<DocumentStore>>,
        feed: Arc<Mutex<MailboxFeed>>,
        stop_flag: Arc<AtomicBool>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let event_loop = EventLoop::new()?;
        event_loop.set_control_flow(ControlFlow::Poll);

        let mut app = Self {
            store,
            feed,
            stop_flag,
            state: None,
            last_poll: Instant::now(),
            title_base: "Thinner Floor".into(),
            hud_visible: true,
            modifiers: ModifiersState::default(),
            hud_panel: HudPanel::new(960.0, 640.0),
            hud_cursor: (0.0, 0.0),
            hud_pressed: false,
            hud_press_pos: (0.0, 0.0),
            hud_press_hit: HudHit::Outside,
            hud_dragging: false,
            hud_grab: (0.0, 0.0),
            hud_last_title_click: None,
            hud_press_consumed: false,
        };
        event_loop.run_app(&mut app)?;
        Ok(())
    }

    fn rebuild_meshes(&mut self) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let doc = {
            let guard = self.store.lock().expect("document store");
            guard.document.clone()
        };
        state.meshes = build_meshes(&state.device, &state.entity_layout, &doc);
        state.drawn_revision = doc.revision;
        state.window.set_title(&format!(
            "{} — rev {} — {} entities",
            self.title_base,
            doc.revision,
            doc.entity_count()
        ));
    }

    fn request_redraw(&self) {
        if let Some(state) = self.state.as_ref() {
            state.window.request_redraw();
        }
    }

    fn on_hud_cursor(&mut self, x: f32, y: f32) {
        self.hud_cursor = (x, y);
        if !self.hud_visible || !self.hud_pressed {
            return;
        }
        if self.hud_press_hit == HudHit::Resize {
            self.hud_panel.resize_to(x, y);
            self.refresh_hud_texture(true);
            self.request_redraw();
            return;
        }
        if self.hud_press_consumed || self.hud_press_hit != HudHit::Title {
            if self.hud_dragging {
                let (gx, gy) = self.hud_grab;
                self.hud_panel.drag_to(x - gx, y - gy);
                self.request_redraw();
            }
            return;
        }
        let dx = x - self.hud_press_pos.0;
        let dy = y - self.hud_press_pos.1;
        if !self.hud_dragging && dx * dx + dy * dy >= HUD_DRAG_THRESHOLD * HUD_DRAG_THRESHOLD {
            self.hud_dragging = true;
            if self.hud_panel.mode == hud::HudMode::Fullscreen {
                self.hud_panel.undock_and_drag(x, y);
                let (px, py, _, _) = self.hud_panel.rect();
                self.hud_grab = (x - px, y - py);
                self.refresh_hud_texture(true);
            }
        }
        if self.hud_dragging {
            let (gx, gy) = self.hud_grab;
            self.hud_panel.drag_to(x - gx, y - gy);
            self.request_redraw();
        }
    }

    fn on_hud_mouse(&mut self, state: ElementState) {
        if !self.hud_visible {
            return;
        }
        let (x, y) = self.hud_cursor;
        match state {
            ElementState::Pressed => {
                let hit = self.hud_panel.hit(x, y);
                self.hud_pressed = true;
                self.hud_press_pos = (x, y);
                self.hud_press_hit = hit;
                self.hud_dragging = false;
                self.hud_press_consumed = false;
                match hit {
                    HudHit::Collapse => {
                        self.hud_panel.toggle_collapse();
                        self.hud_press_consumed = true;
                        self.refresh_hud_texture(true);
                        self.request_redraw();
                    }
                    HudHit::Fullscreen => {
                        self.hud_panel.restore_fullscreen();
                        self.hud_press_consumed = true;
                        self.refresh_hud_texture(true);
                        self.request_redraw();
                    }
                    HudHit::Title => {
                        if let Some(prev) = self.hud_last_title_click {
                            if prev.elapsed() <= HUD_DOUBLE_CLICK {
                                self.hud_panel.restore_fullscreen();
                                self.hud_last_title_click = None;
                                self.hud_press_consumed = true;
                                self.refresh_hud_texture(true);
                                self.request_redraw();
                                return;
                            }
                        }
                        let (px, py, _, _) = self.hud_panel.rect();
                        self.hud_grab = (x - px, y - py);
                        self.hud_last_title_click = Some(Instant::now());
                    }
                    HudHit::Resize => {
                        self.hud_press_consumed = true;
                    }
                    HudHit::Body | HudHit::Outside => {}
                }
            }
            ElementState::Released => {
                if self.hud_pressed
                    && !self.hud_press_consumed
                    && !self.hud_dragging
                    && self.hud_press_hit == HudHit::Title
                {
                    self.hud_panel.toggle_collapse();
                    self.refresh_hud_texture(true);
                    self.request_redraw();
                }
                self.hud_pressed = false;
                self.hud_dragging = false;
                self.hud_press_consumed = false;
            }
        }
    }

    fn refresh_hud_texture(&mut self, force: bool) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let (tex_w, tex_h) = self.hud_panel.pixel_size();
        let size_changed = state.hud.tex_w != tex_w || state.hud.tex_h != tex_h;
        recreate_hud_texture(&mut state.hud, &state.device, tex_w, tex_h);
        let (seq, frame) = {
            let feed = self.feed.lock().expect("mailbox feed");
            let solid = self.hud_panel.mode == hud::HudMode::Floating;
            let frame = hud::rasterize_feed(&feed, Instant::now(), tex_w, tex_h, solid);
            (feed.seq, frame)
        };
        if !force
            && !size_changed
            && seq == state.hud.last_seq
            && frame.active == state.hud.last_active
            && frame.event_count == state.hud.last_count
        {
            return;
        }
        let (upload, bytes_per_row) = pad_rgba_rows(&frame.pixels, frame.width, frame.height);
        state.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.hud.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &upload,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(frame.height),
            },
            wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
        );
        state.hud.last_seq = seq;
        state.hud.last_active = frame.active;
        state.hud.last_count = frame.event_count;
    }

    fn render_frame(&mut self) -> Result<(), wgpu::SurfaceError> {
        if self.hud_visible {
            self.refresh_hud_texture(false);
        }

        let Some(state) = self.state.as_mut() else {
            return Ok(());
        };

        let output = state.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let aspect = state.size.width.max(1) as f32 / state.size.height.max(1) as f32;
        let view_proj = camera_matrix(aspect);
        let (center, radius, light) = self
            .store
            .lock()
            .ok()
            .map(|g| {
                let (c, r) = g.document.light_fit();
                (c, r, g.document.light.clone())
            })
            .unwrap_or_else(|| {
                (
                    glam::Vec3::new(0.0, 0.5, 0.0),
                    8.0,
                    thinner_floor::light::SceneLight::default(),
                )
            });
        let (overlay_on, have_cycles) = {
            let mut host = cycles_stream::host().lock().unwrap();
            if host.overlay_on() {
                if let Some(fr) = host.read_frame() {
                    upload_cycles_frame(state, &fr);
                }
                let have = state.cycles.last_seq > 0 && state.cycles.tex_w > 8;
                (true, have)
            } else {
                (false, false)
            }
        };
        if overlay_on {
            let label = if have_cycles {
                format!("CYCLES SAMPLE {}", state.cycles.last_sample)
            } else {
                "CYCLES STARTING".into()
            };
            set_cycles_wait_banner(state, &label, have_cycles);
        }
        let show_cycles = overlay_on && have_cycles;
        let uniforms = light::pack_frame(view_proj, &light, center, radius);
        state
            .queue
            .write_buffer(&state.frame_uniform, 0, bytemuck::bytes_of(&uniforms));

        let (px, py, pw, ph) = self.hud_panel.rect();
        let hud_uniforms = HudUniforms {
            screen: [state.size.width as f32, state.size.height as f32],
            _pad: [0.0, 0.0],
            rect: [px, py, pw, ph],
        };
        state.queue.write_buffer(
            &state.hud.uniform_buffer,
            0,
            bytemuck::bytes_of(&hud_uniforms),
        );

        let mut encoder = state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("thinner-floor encoder"),
            });

        if !show_cycles {
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("thinner-floor shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &state.shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&state.shadow_pipeline);
            pass.set_bind_group(0, &state.simple_bind_group, &[]);
            for mesh in &state.meshes {
                if !mesh.cast_shadow {
                    continue;
                }
                pass.set_bind_group(1, &mesh.entity_bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("thinner-floor scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.08,
                            g: 0.09,
                            b: 0.11,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &state.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&state.pipeline);
            pass.set_bind_group(0, &state.scene_bind_group, &[]);
            for mesh in &state.meshes {
                pass.set_bind_group(1, &mesh.entity_bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }
        }
        }

        if show_cycles && state.cycles.tex_w > 0 {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("thinner-floor cycles overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&state.cycles.pipeline);
            pass.set_bind_group(0, &state.cycles.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        if overlay_on && state.cycles.wait_w > 1 {
            let bar_w = state.cycles.wait_w as f32;
            let bar_h = state.cycles.wait_h as f32;
            let x = ((state.size.width as f32 - bar_w) * 0.5).max(8.0);
            let y = 12.0;
            let wait_u = HudUniforms {
                screen: [state.size.width as f32, state.size.height as f32],
                _pad: [0.0, 0.0],
                rect: [x, y, bar_w, bar_h],
            };
            state.queue.write_buffer(
                &state.cycles.wait_uniform,
                0,
                bytemuck::bytes_of(&wait_u),
            );
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("thinner-floor cycles wait"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&state.hud.pipeline);
            pass.set_bind_group(0, &state.cycles.wait_bind, &[]);
            pass.draw(0..6, 0..1);
        }

        if self.hud_visible {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("thinner-floor hud"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&state.hud.pipeline);
            pass.set_bind_group(0, &state.hud.bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        state.queue.submit(std::iter::once(encoder.finish()));
        output.present();
        Ok(())
    }
}

impl ApplicationHandler for ViewportApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }

        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Thinner Floor")
                        .with_inner_size(LogicalSize::new(960.0, 640.0))
                        .with_window_level(WindowLevel::AlwaysOnTop),
                )
                .expect("create window"),
        );
        window.set_window_level(WindowLevel::AlwaysOnTop);
        let _ = window.focus_window();

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("no suitable GPU adapter (need Vulkan/Metal/DX12/GL)");

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("thinner-floor device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                memory_hints: Default::default(),
            },
            None,
        ))
        .expect("request device");

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("thinner-floor shader"),
            source: wgpu::ShaderSource::Wgsl(light::SCENE_WGSL.into()),
        });
        let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("thinner-floor shadow shader"),
            source: wgpu::ShaderSource::Wgsl(light::SHADOW_WGSL.into()),
        });

        let simple_frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
        });
        let scene_frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
        });

        let entity_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline layout"),
            bind_group_layouts: &[&scene_frame_layout, &entity_layout],
            push_constant_ranges: &[],
        });
        let shadow_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow pipeline layout"),
            bind_group_layouts: &[&simple_frame_layout, &entity_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("thinner-floor pipeline"),
            layout: Some(&pipeline_layout),
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
                    format: config.format,
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
            label: Some("thinner-floor shadow"),
            layout: Some(&shadow_pipeline_layout),
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

        let frame_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame uniform"),
            size: std::mem::size_of::<FrameUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shadow_tex = device.create_texture(&wgpu::TextureDescriptor {
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
        });
        let shadow_view = shadow_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow compare"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let simple_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow frame bind"),
            layout: &simple_frame_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_uniform.as_entire_binding(),
            }],
        });
        let scene_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene frame bind"),
            layout: &scene_frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: frame_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&shadow_sampler),
                },
            ],
        });

        let hud = create_hud_gpu(
            &device,
            config.format,
            size.width.max(1),
            size.height.max(1),
        );
        let cycles = create_cycles_gpu(&device, config.format, &hud);
        self.hud_panel.set_window_size(size.width as f32, size.height as f32);

        let depth_view = make_depth_view(&device, size.width.max(1), size.height.max(1));
        self.state = Some(SurfaceState {
            window,
            surface,
            device,
            queue,
            config,
            pipeline,
            shadow_pipeline,
            scene_bind_group,
            simple_bind_group,
            frame_uniform,
            entity_layout,
            meshes: Vec::new(),
            drawn_revision: u64::MAX,
            size,
            depth_view,
            shadow_tex,
            shadow_view,
            hud,
            cycles,
        });
        self.rebuild_meshes();
        self.refresh_hud_texture(true);
        std::thread::spawn(|| {
            if let Ok(mut h) = cycles_stream::host().lock() {
                let _ = h.ensure_spawned();
            }
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let window_ok = self
            .state
            .as_ref()
            .map(|s| s.window.id() == id)
            .unwrap_or(false);
        if !window_ok {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                if let Ok(mut h) = cycles_stream::host().lock() {
                    h.kill_child();
                }
                self.stop_flag.store(true, Ordering::SeqCst);
                event_loop.exit();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed && !event.repeat {
                    let ctrl = self.modifiers.control_key();
                    let shift = self.modifiers.shift_key();
                    let alt = self.modifiers.alt_key();
                    let meta = self.modifiers.super_key();
                    if ctrl && shift && !alt && !meta {
                        if let Key::Character(c) = &event.logical_key {
                            if c.eq_ignore_ascii_case("m") {
                                self.hud_visible = !self.hud_visible;
                                if self.hud_visible {
                                    self.refresh_hud_texture(true);
                                }
                                self.request_redraw();
                            }
                            if c.eq_ignore_ascii_case("c") {
                                self.cycles_hotkey_toggle();
                                self.request_redraw();
                            }
                            if c.eq_ignore_ascii_case("x") {
                                if let Ok(mut h) = cycles_stream::host().lock() {
                                    h.stop();
                                }
                                self.request_redraw();
                            }
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.on_hud_cursor(position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput {
                state: btn_state,
                button,
                ..
            } => {
                if button == MouseButton::Left {
                    self.on_hud_mouse(btn_state);
                }
            }
            WindowEvent::Resized(new_size) => {
                if new_size.width > 0 && new_size.height > 0 {
                    if let Some(state) = self.state.as_mut() {
                        state.size = new_size;
                        state.config.width = new_size.width;
                        state.config.height = new_size.height;
                        state.surface.configure(&state.device, &state.config);
                        state.depth_view = make_depth_view(&state.device, new_size.width, new_size.height);
                    }
                    self.hud_panel
                        .set_window_size(new_size.width as f32, new_size.height as f32);
                    self.refresh_hud_texture(true);
                    self.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => match self.render_frame() {
                Ok(()) => {}
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                    if let Some(state) = self.state.as_mut() {
                        state.surface.configure(&state.device, &state.config);
                        state.depth_view = make_depth_view(
                            &state.device,
                            state.config.width,
                            state.config.height,
                        );
                    }
                }
                Err(wgpu::SurfaceError::OutOfMemory) => {
                    self.stop_flag.store(true, Ordering::SeqCst);
                    event_loop.exit();
                }
                Err(e) => eprintln!("surface error: {e:?}"),
            },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.stop_flag.load(Ordering::SeqCst) {
            event_loop.exit();
            return;
        }

        if self.last_poll.elapsed() >= Duration::from_millis(50) {
            self.last_poll = Instant::now();
            let revision = self
                .store
                .lock()
                .map(|g| g.document.revision)
                .unwrap_or(0);
            let drawn = self
                .state
                .as_ref()
                .map(|s| s.drawn_revision)
                .unwrap_or(u64::MAX);
            if revision != drawn {
                self.rebuild_meshes();
            }
            // Refresh IDLE/ACTIVE header even when no new events.
            if self.hud_visible {
                self.refresh_hud_texture(false);
            }
        }

        if let Some(state) = self.state.as_ref() {
            state.window.request_redraw();
        }

        std::thread::sleep(Duration::from_millis(8));
    }
}

fn create_cycles_gpu(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    hud: &HudGpu,
) -> CyclesGpu {
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cycles overlay"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("cycles overlay layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
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
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cycles overlay bind"),
        layout: &bind_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cycles overlay shader"),
        source: wgpu::ShaderSource::Wgsl(CYCLES_OVERLAY_SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("cycles overlay pipeline layout"),
        bind_group_layouts: &[&bind_layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cycles overlay pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
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
    let wait_uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cycles wait uniform"),
        size: std::mem::size_of::<HudUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let wait_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cycles wait banner"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let wait_view = wait_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let wait_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cycles wait bind"),
        layout: &hud.bind_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wait_uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&wait_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&hud.sampler),
            },
        ],
    });
    CyclesGpu {
        pipeline,
        bind_group,
        bind_layout,
        sampler,
        texture,
        tex_w: 1,
        tex_h: 1,
        last_seq: 0,
        last_sample: 0,
        wait_uniform,
        wait_texture,
        wait_bind,
        wait_w: 1,
        wait_h: 1,
        wait_label: String::new(),
    }
}

fn set_cycles_wait_banner(state: &mut SurfaceState, title: &str, have_frame: bool) {
    let line2 = if have_frame {
        "CTRL+SHIFT+C PAUSE   CTRL+SHIFT+X STOP"
    } else {
        "FIRST SAMPLE CAN TAKE A FEW SECONDS"
    };
    let label = format!("{title}\n{line2}");
    if state.cycles.wait_label == label {
        return;
    }
    state.cycles.wait_label = label;
    let (w, h, pixels) = hud::rasterize_banner(&[title, line2]);
    if state.cycles.wait_w != w || state.cycles.wait_h != h {
        state.cycles.wait_texture = state.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cycles wait banner"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = state
            .cycles
            .wait_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        state.cycles.wait_bind = state.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cycles wait bind"),
            layout: &state.hud.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: state.cycles.wait_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&state.hud.sampler),
                },
            ],
        });
        state.cycles.wait_w = w;
        state.cycles.wait_h = h;
    }
    let (upload, bpr) = pad_rgba_rows(&pixels, w, h);
    state.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &state.cycles.wait_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &upload,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bpr),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

fn upload_cycles_frame(state: &mut SurfaceState, fr: &cycles_stream::Frame) {
    if fr.width == 0 || fr.height == 0 {
        return;
    }
    if state.cycles.tex_w != fr.width || state.cycles.tex_h != fr.height {
        state.cycles.texture = state.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cycles overlay"),
            size: wgpu::Extent3d {
                width: fr.width,
                height: fr.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = state
            .cycles
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        state.cycles.bind_group = state.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cycles overlay bind"),
            layout: &state.cycles.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&state.cycles.sampler),
                },
            ],
        });
        state.cycles.tex_w = fr.width;
        state.cycles.tex_h = fr.height;
    }
    state.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &state.cycles.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &fr.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(fr.width * 4),
            rows_per_image: Some(fr.height),
        },
        wgpu::Extent3d {
            width: fr.width,
            height: fr.height,
            depth_or_array_layers: 1,
        },
    );
    state.cycles.last_seq = fr.seq;
    state.cycles.last_sample = fr.sample;
}

fn create_hud_gpu(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> HudGpu {
    let width = width.max(1);
    let height = height.max(1);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hud texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("hud sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("hud uniform"),
        size: std::mem::size_of::<HudUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("hud layout"),
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
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("hud bind group"),
        layout: &bind_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&texture_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("hud shader"),
        source: wgpu::ShaderSource::Wgsl(HUD_SHADER.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("hud pipeline layout"),
        bind_group_layouts: &[&bind_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("hud pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    HudGpu {
        pipeline,
        bind_group,
        bind_layout,
        sampler,
        uniform_buffer,
        texture,
        tex_w: width,
        tex_h: height,
        last_seq: u64::MAX,
        last_active: false,
        last_count: usize::MAX,
    }
}

fn pad_rgba_rows(pixels: &[u8], width: u32, height: u32) -> (Vec<u8>, u32) {
    let unpadded = 4 * width;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    if padded == unpadded {
        return (pixels.to_vec(), unpadded);
    }
    let mut out = vec![0u8; (padded * height) as usize];
    for y in 0..height as usize {
        let src = y * unpadded as usize;
        let dst = y * padded as usize;
        out[dst..dst + unpadded as usize].copy_from_slice(&pixels[src..src + unpadded as usize]);
    }
    (out, padded)
}

fn recreate_hud_texture(hud: &mut HudGpu, device: &wgpu::Device, width: u32, height: u32) {
    let width = width.max(1);
    let height = height.max(1);
    if hud.tex_w == width && hud.tex_h == height {
        return;
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hud texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("hud bind group"),
        layout: &hud.bind_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: hud.uniform_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&texture_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&hud.sampler),
            },
        ],
    });
    hud.texture = texture;
    hud.bind_group = bind_group;
    hud.tex_w = width;
    hud.tex_h = height;
    hud.last_seq = u64::MAX;
}

fn make_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scene depth"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn camera_matrix(aspect: f32) -> Mat4 {
    camera::window_view_proj_matrix(aspect)
}

fn build_meshes(
    device: &wgpu::Device,
    entity_layout: &wgpu::BindGroupLayout,
    doc: &Document,
) -> Vec<GpuMesh> {
    let mut meshes = Vec::new();
    for scene in &doc.scenes {
        for entity in &scene.entities {
            if entity.mesh.recipe == "empty" {
                continue;
            }
            let Ok((pos, nrm, indices)) =
                thinner_floor::geom::mesh(&entity.mesh.recipe, entity.mesh.size)
            else {
                continue;
            };
            let vertices: Vec<Vertex> = pos
                .into_iter()
                .zip(nrm)
                .map(|(position, normal)| Vertex { position, normal })
                .collect();
            let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("entity vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("entity indices"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });

            let model = doc
                .world_matrix(&entity.id)
                .unwrap_or_else(|| thinner_floor::document::Document::local_matrix(&entity.transform));

            let surf = doc.resolved_surface(entity);
            let entity_uniform = EntityUniforms {
                model: model.to_cols_array_2d(),
                color: surf.color,
                roughness: surf.roughness,
                metallic: surf.metallic,
                _pad: [0.0, 0.0],
            };
            let entity_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("entity uniform"),
                contents: bytemuck::bytes_of(&entity_uniform),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let entity_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("entity bind group"),
                layout: entity_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: entity_buffer.as_entire_binding(),
                }],
            });

            meshes.push(GpuMesh {
                vertex_buffer,
                index_buffer,
                index_count: indices.len() as u32,
                entity_bind_group,
                cast_shadow: thinner_floor::light::casts_shadow(
                    &entity.mesh.recipe,
                    entity.mesh.size,
                ),
            });
        }
    }
    meshes
}

const CYCLES_OVERLAY_SHADER: &str = r#"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let xy = p[i];
    var o: VsOut;
    o.pos = vec4<f32>(xy, 0.0, 1.0);
    o.uv = vec2<f32>(xy.x * 0.5 + 0.5, 0.5 - xy.y * 0.5);
    return o;
}
@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, input.uv);
}
"#;

const HUD_SHADER: &str = r#"
struct HudUniforms {
    screen: vec2<f32>,
    _pad: vec2<f32>,
    rect: vec4<f32>,
};

@group(0) @binding(0) var<uniform> hud: HudUniforms;
@group(0) @binding(1) var hud_tex: texture_2d<f32>;
@group(0) @binding(2) var hud_samp: sampler;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[idx];
    let px = hud.rect.x + c.x * hud.rect.z;
    let py = hud.rect.y + c.y * hud.rect.w;
    let ndc = vec2<f32>(
        (px / hud.screen.x) * 2.0 - 1.0,
        1.0 - (py / hud.screen.y) * 2.0,
    );
    var out: VsOut;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = c;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    return textureSample(hud_tex, hud_samp, input.uv);
}
"#;
