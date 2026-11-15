//! The norn-ui painter (plan 2.4): draws a window's node tree with wgpu.
//!
//! - **Paint pipelines:** boxes, glyphs, triangles and RGBA pixel buffers.
//! - **Damage-only:** each window has a persistent canvas texture; a frame
//!   repaints only inside the engine's damage rectangles (scissored), and
//!   backends copy the canvas to the screen. An idle window costs nothing.
//! - **Headless:** the same path renders without a surface; `pixels` and
//!   `save_png` read a canvas back for golden-image tests.
//!
//! Colors are 8-bit sRGB values blended as given (as browsers do).

mod atlas;
mod canvas;
mod display;

use atlas::{Atlas, GlyphKey};
pub use display::{Item, PixelSampling, display_list};
use norn_ui_engine::{Engine, Rect, Text, WindowId};
use std::collections::HashMap;
use std::sync::{Arc, Weak};

const SHADER: &str = r#"
struct Globals { size: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;

fn to_clip(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p.x / globals.size.x * 2.0 - 1.0, 1.0 - p.y / globals.size.y * 2.0, 0.0, 1.0);
}

fn corner(i: u32) -> vec2<f32> {
    var c = array<vec2<f32>, 6>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
    return c[i];
}

// ---- boxes ----
struct BoxIn {
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) border_color: vec4<f32>,
    @location(3) radius_border: vec2<f32>,
};
struct BoxOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) half: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) border_color: vec4<f32>,
    @location(4) radius_border: vec2<f32>,
};

@vertex fn box_vs(@builtin(vertex_index) vi: u32, b: BoxIn) -> BoxOut {
    let c = corner(vi);
    var o: BoxOut;
    let p = b.rect.xy + c * b.rect.zw;
    o.pos = to_clip(p);
    o.half = b.rect.zw * 0.5;
    o.local = p - (b.rect.xy + o.half);
    o.color = b.color;
    o.border_color = b.border_color;
    o.radius_border = b.radius_border;
    return o;
}

fn rounded(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2(r, r);
    return length(max(q, vec2(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment fn box_fs(o: BoxOut) -> @location(0) vec4<f32> {
    let r = min(o.radius_border.x, min(o.half.x, o.half.y));
    let d = rounded(o.local, o.half, r);
    let outside = clamp(0.5 - d, 0.0, 1.0);
    let bw = o.radius_border.y;
    var c = o.color;
    if (bw > 0.0) {
        let inner = clamp(0.5 - (d + bw), 0.0, 1.0);
        c = mix(o.border_color, o.color, inner);
    }
    let a = c.a * outside;
    return vec4<f32>(c.rgb * a, a);
}

// ---- glyphs ----
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct GlyphIn {
    @location(0) rect: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) color: vec4<f32>,
};
struct GlyphOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex fn glyph_vs(@builtin(vertex_index) vi: u32, g: GlyphIn) -> GlyphOut {
    let c = corner(vi);
    var o: GlyphOut;
    o.pos = to_clip(g.rect.xy + c * g.rect.zw);
    o.uv = g.uv.xy + c * g.uv.zw;
    o.color = g.color;
    return o;
}

@fragment fn glyph_fs(o: GlyphOut) -> @location(0) vec4<f32> {
    let cov = textureSample(atlas, atlas_sampler, o.uv).r;
    let a = o.color.a * cov;
    return vec4<f32>(o.color.rgb * a, a);
}

// ---- triangles (canvas lines and paths) ----
struct TriIn { @location(0) pos: vec2<f32>, @location(1) color: vec4<f32> };
struct TriOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };

@vertex fn tri_vs(t: TriIn) -> TriOut {
    var o: TriOut;
    o.pos = to_clip(t.pos);
    o.color = t.color;
    return o;
}

@fragment fn tri_fs(o: TriOut) -> @location(0) vec4<f32> {
    return vec4<f32>(o.color.rgb * o.color.a, o.color.a);
}

// The pixel pipeline binds an RGBA texture at the same slot as the glyph atlas.
struct PixelOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex fn pixels_vs(@builtin(vertex_index) vi: u32, @location(0) rect: vec4<f32>) -> PixelOut {
    let uv = corner(vi);
    return PixelOut(to_clip(rect.xy + uv * rect.zw), uv);
}

fn premultiply(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}

@fragment fn pixels_fs(o: PixelOut) -> @location(0) vec4<f32> {
    return premultiply(textureSample(atlas, atlas_sampler, o.uv));
}

// Weight each texel by alpha before interpolation so transparent pixels
// cannot tint their neighbors. The shared texture holds straight RGBA8.
fn load_pixel(at: vec2<i32>, last: vec2<i32>) -> vec4<f32> {
    return premultiply(textureLoad(atlas, clamp(at, vec2<i32>(0), last), 0));
}

@fragment fn linear_pixels_fs(o: PixelOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(atlas));
    let p = o.uv * vec2<f32>(size) - vec2<f32>(0.5);
    let at = vec2<i32>(floor(p));
    let f = fract(p);
    let last = size - vec2<i32>(1);
    let top = mix(load_pixel(at, last), load_pixel(at + vec2<i32>(1, 0), last), f.x);
    let bottom = mix(load_pixel(at + vec2<i32>(0, 1), last), load_pixel(at + vec2<i32>(1, 1), last), f.x);
    return mix(top, bottom, f.y);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TriVertex {
    pos: [f32; 2],
    color: [f32; 4],
}

/// What a run of draw calls draws.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Boxes,
    Glyphs,
    Tris,
    Pixels(PixelSampling),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BoxInstance {
    rect: [f32; 4],
    color: [f32; 4],
    border_color: [f32; 4],
    radius_border: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlyphInstance {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    size: [f32; 2],
    _pad: [f32; 2],
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Copies a canvas to a surface texture (any format): a full-screen
/// triangle sampling the canvas.
const BLIT: &str = r#"
@group(0) @binding(0) var canvas: texture_2d<f32>;
@group(0) @binding(1) var canvas_sampler: sampler;
struct Out { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: Out;
    o.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@fragment fn fs(o: Out) -> @location(0) vec4<f32> { return textureSample(canvas, canvas_sampler, o.uv); }
"#;

struct Canvas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

type PixelKey = (usize, [u32; 2]);

struct PixelTexture {
    // Keeps the cache key unique without retaining a discarded pixel buffer.
    source: Weak<Vec<u8>>,
    bind_group: wgpu::BindGroup,
}

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    box_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    tri_pipeline: wgpu::RenderPipeline,
    pixel_pipeline: wgpu::RenderPipeline,
    linear_pixel_pipeline: wgpu::RenderPipeline,
    pixel_sampler: wgpu::Sampler,
    pixel_textures: HashMap<PixelKey, PixelTexture>,
    paint_layout: wgpu::BindGroupLayout,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    atlas: Atlas,
    canvases: HashMap<WindowId, Canvas>,
    /// Frames actually drawn (for tests; the engine counts its own).
    pub frames: u64,
    blits: HashMap<wgpu::TextureFormat, (wgpu::RenderPipeline, wgpu::BindGroupLayout)>,
    blit_sampler: Option<wgpu::Sampler>,
}

fn rgba(c: [u8; 4]) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0]
}

impl Renderer {
    /// A renderer without a surface (tests, CI, the headless backend). It
    /// asks for the low-power GPU; with `NORN_UI_SOFTWARE=1`, for the
    /// software fallback adapter.
    pub fn headless() -> Result<Renderer, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: std::env::var("NORN_UI_SOFTWARE").is_ok_and(|v| v == "1"),
            compatible_surface: None,
            ..Default::default()
        }))
        .map_err(|e| format!("No GPU adapter: {e}"))?;
        Renderer::with_adapter(&adapter)
    }

    /// A renderer on `adapter` (a backend passes the one its surface uses).
    pub fn with_adapter(adapter: &wgpu::Adapter) -> Result<Renderer, String> {
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("norn-ui"),
            ..Default::default()
        }))
        .map_err(|e| format!("No GPU device: {e}"))?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("norn-ui"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let atlas = Atlas::new(&device);
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("norn-ui"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
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
            label: Some("norn-ui"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&atlas.sampler) },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("norn-ui"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = |vs: &str, fs: &str, attrs: &[wgpu::VertexAttribute], stride: usize, step_mode: wgpu::VertexStepMode| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(vs),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: stride as u64,
                        step_mode,
                        attributes: attrs,
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FORMAT,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let box_pipeline = pipeline(
            "box_vs",
            "box_fs",
            &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x2],
            std::mem::size_of::<BoxInstance>(),
            wgpu::VertexStepMode::Instance,
        );
        let glyph_pipeline = pipeline(
            "glyph_vs",
            "glyph_fs",
            &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4],
            std::mem::size_of::<GlyphInstance>(),
            wgpu::VertexStepMode::Instance,
        );
        let tri_pipeline = pipeline(
            "tri_vs",
            "tri_fs",
            &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
            std::mem::size_of::<TriVertex>(),
            wgpu::VertexStepMode::Vertex,
        );
        let pixel_pipeline = pipeline("pixels_vs", "pixels_fs", &wgpu::vertex_attr_array![0 => Float32x4], 16, wgpu::VertexStepMode::Instance);
        let linear_pixel_pipeline = pipeline("pixels_vs", "linear_pixels_fs", &wgpu::vertex_attr_array![0 => Float32x4], 16, wgpu::VertexStepMode::Instance);
        let pixel_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("canvas pixel sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        Ok(Renderer {
            device,
            queue,
            box_pipeline,
            glyph_pipeline,
            tri_pipeline,
            pixel_pipeline,
            linear_pixel_pipeline,
            pixel_sampler,
            pixel_textures: HashMap::new(),
            paint_layout: layout,
            globals,
            bind_group,
            atlas,
            canvases: HashMap::new(),
            frames: 0,
            blits: HashMap::new(),
            blit_sampler: None,
        })
    }

    fn upload_pixels(&mut self, size: [u32; 2], rgba: &Arc<Vec<u8>>) -> Option<PixelKey> {
        let [width, height] = size;
        let max = self.device.limits().max_texture_dimension_2d;
        if width > max || height > max {
            return None;
        }
        let key = (Arc::as_ptr(rgba) as usize, size);
        self.pixel_textures.entry(key).or_insert_with(|| {
            let extent = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("canvas pixels"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                texture.as_image_copy(),
                rgba,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * width), rows_per_image: Some(height) },
                extent,
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("canvas pixels"),
                layout: &self.paint_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.globals.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.pixel_sampler) },
                ],
            });
            PixelTexture { source: Arc::downgrade(rgba), bind_group }
        });
        Some(key)
    }

    fn ensure_canvas(&mut self, w: WindowId, width: u32, height: u32) -> bool {
        let stale = self.canvases.get(&w).is_none_or(|c| c.width != width || c.height != height);
        if stale {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("canvas"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.canvases.insert(w, Canvas { texture, view, width, height });
        }
        stale
    }

    /// The canvas texture of window `w` (backends copy it to the screen).
    pub fn canvas_texture(&self, w: WindowId) -> Option<&wgpu::Texture> {
        self.canvases.get(&w).map(|c| &c.texture)
    }

    /// Repaint window `w` inside `damage` (logical pixels) at `scale`.
    /// With no damage, nothing is drawn.
    pub fn render(&mut self, engine: &Engine, w: WindowId, text: &mut Text, damage: &[Rect], scale: f32) {
        if damage.is_empty() {
            return;
        }
        let Some(win) = engine.windows.get(&w) else { return };
        let (pw, ph) = (((win.size.0 * scale).round() as u32).max(1), ((win.size.1 * scale).round() as u32).max(1));
        // A new canvas has nothing on it: repaint everything once.
        let fresh = self.ensure_canvas(w, pw, ph);
        let full = [Rect { x: 0.0, y: 0.0, w: win.size.0, h: win.size.1 }];
        let damage = if fresh { &full[..] } else { damage };

        let items = display_list(engine, w, text, damage);
        let mut boxes: Vec<BoxInstance> = Vec::new();
        let mut glyphs: Vec<GlyphInstance> = Vec::new();
        let mut tris: Vec<TriVertex> = Vec::new();
        let mut pixel_rects: Vec<[f32; 4]> = Vec::new();
        let mut pixel_keys = Vec::new();
        self.pixel_textures.retain(|_, tex| tex.source.strong_count() > 0);
        // Runs of one kind and clip, in paint order: (kind, start, end, clip).
        let mut segments: Vec<(Kind, u32, u32, Rect)> = Vec::new();
        fn push_seg(segments: &mut Vec<(Kind, u32, u32, Rect)>, kind: Kind, start: usize, end: usize, clip: Rect) {
            if start == end {
                return;
            }
            match segments.last_mut() {
                Some(s) if s.0 == kind && s.2 as usize == start && s.3 == clip => s.2 = end as u32,
                _ => segments.push((kind, start as u32, end as u32, clip)),
            }
        }
        for item in &items {
            match item {
                Item::Box { rect, color, radius, border, clip } => {
                    let (bw, bc) = border.unwrap_or((0.0, [0, 0, 0, 0]));
                    let start = boxes.len();
                    boxes.push(BoxInstance {
                        rect: [rect.x * scale, rect.y * scale, rect.w * scale, rect.h * scale],
                        color: rgba(*color),
                        border_color: rgba(bc),
                        radius_border: [radius * scale, bw * scale],
                    });
                    push_seg(&mut segments, Kind::Boxes, start, boxes.len(), *clip);
                }
                Item::Text { x, y, runs, clip } => {
                    let start = glyphs.len();
                    for run in runs {
                        for &(id, gx, gy) in &run.glyphs {
                            let px = (x + gx) * scale;
                            let py = (y + gy) * scale;
                            let key = GlyphKey::new(&run.font, id, run.size * scale, &run.coords, px.fract());
                            let Some(slot) = self.atlas.glyph(&self.queue, key, &run.font, &run.coords) else { continue };
                            if slot.w == 0 {
                                continue;
                            }
                            glyphs.push(GlyphInstance {
                                rect: [px.floor() + slot.left as f32, py.round() - slot.top as f32, slot.w as f32, slot.h as f32],
                                uv: self.atlas.uv(&slot),
                                color: rgba(run.color),
                            });
                        }
                    }
                    push_seg(&mut segments, Kind::Glyphs, start, glyphs.len(), *clip);
                }
                Item::Icon { rect, path, view, stroke, color, clip } => {
                    let px = (rect.w * scale).round().max(1.0) as u32;
                    let start = glyphs.len();
                    if let Some(slot) = self.atlas.mask(&self.queue, path, *view, px, *stroke) {
                        glyphs.push(GlyphInstance {
                            rect: [(rect.x * scale).round() + slot.left as f32, (rect.y * scale).round() - slot.top as f32, slot.w as f32, slot.h as f32],
                            uv: self.atlas.uv(&slot),
                            color: rgba(*color),
                        });
                    }
                    push_seg(&mut segments, Kind::Glyphs, start, glyphs.len(), *clip);
                }
                Item::Tris { verts, color, clip } => {
                    let start = tris.len();
                    let c = rgba(*color);
                    tris.extend(verts.iter().map(|v| TriVertex { pos: [v[0] * scale, v[1] * scale], color: c }));
                    push_seg(&mut segments, Kind::Tris, start, tris.len(), *clip);
                }
                Item::Pixels { rect, size, rgba, sampling, clip } => {
                    let Some(key) = self.upload_pixels(*size, rgba) else { continue };
                    let start = pixel_rects.len();
                    pixel_rects.push([rect.x * scale, rect.y * scale, rect.w * scale, rect.h * scale]);
                    pixel_keys.push(key);
                    push_seg(&mut segments, Kind::Pixels(*sampling), start, pixel_rects.len(), *clip);
                }
            }
        }

        self.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&Globals { size: [pw as f32, ph as f32], _pad: [0.0; 2] }));
        let make = |data: &[u8], label: &str| {
            use wgpu::util::DeviceExt;
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: if data.is_empty() { &[0u8; 64] } else { data },
                usage: wgpu::BufferUsages::VERTEX,
            })
        };
        let box_buf = make(bytemuck::cast_slice(&boxes), "boxes");
        let glyph_buf = make(bytemuck::cast_slice(&glyphs), "glyphs");
        let tri_buf = make(bytemuck::cast_slice(&tris), "triangles");
        let pixel_buf = (!pixel_rects.is_empty()).then(|| make(bytemuck::cast_slice(&pixel_rects), "pixel rectangles"));
        let canvas = &self.canvases[&w];
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &canvas.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: if fresh { wgpu::LoadOp::Clear(wgpu::Color::WHITE) } else { wgpu::LoadOp::Load },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_bind_group(0, &self.bind_group, &[]);
            let scissor = |r: &Rect| -> Option<(u32, u32, u32, u32)> {
                let x0 = ((r.x * scale).floor().max(0.0) as u32).min(pw);
                let y0 = ((r.y * scale).floor().max(0.0) as u32).min(ph);
                let x1 = (((r.x + r.w) * scale).ceil().max(0.0) as u32).min(pw);
                let y1 = (((r.y + r.h) * scale).ceil().max(0.0) as u32).min(ph);
                (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
            };
            for r in damage {
                for &(kind, start, end, clip) in &segments {
                    let Some((x, y, sw, sh)) = scissor(&r.intersect(&clip)) else { continue };
                    pass.set_scissor_rect(x, y, sw, sh);
                    match kind {
                        Kind::Glyphs => {
                            pass.set_pipeline(&self.glyph_pipeline);
                            pass.set_vertex_buffer(0, glyph_buf.slice(..));
                            pass.draw(0..6, start..end);
                        }
                        Kind::Boxes => {
                            pass.set_pipeline(&self.box_pipeline);
                            pass.set_vertex_buffer(0, box_buf.slice(..));
                            pass.draw(0..6, start..end);
                        }
                        Kind::Tris => {
                            pass.set_pipeline(&self.tri_pipeline);
                            pass.set_vertex_buffer(0, tri_buf.slice(..));
                            pass.draw(start..end, 0..1);
                        }
                        Kind::Pixels(sampling) => {
                            pass.set_pipeline(match sampling {
                                PixelSampling::Nearest => &self.pixel_pipeline,
                                PixelSampling::Linear => &self.linear_pixel_pipeline,
                            });
                            pass.set_vertex_buffer(0, pixel_buf.as_ref().unwrap().slice(..));
                            for i in start..end {
                                pass.set_bind_group(0, &self.pixel_textures[&pixel_keys[i as usize]].bind_group, &[]);
                                pass.draw(0..6, i..i + 1);
                            }
                            pass.set_bind_group(0, &self.bind_group, &[]);
                        }
                    }
                }
            }
        }
        self.queue.submit([enc.finish()]);
        self.frames += 1;
    }

    /// Copy window `w`'s canvas to `target` (a surface texture of `format`,
    /// the canvas's size). Backends call this to show a frame.
    pub fn present(&mut self, w: WindowId, target: &wgpu::TextureView, format: wgpu::TextureFormat) {
        let Some(canvas) = self.canvases.get(&w) else { return };
        if !self.blits.contains_key(&format) {
            let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("blit"), source: wgpu::ShaderSource::Wgsl(BLIT.into()) });
            let layout = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("blit"),
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
            let pl = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("blit"), bind_group_layouts: &[Some(&layout)], ..Default::default() });
            let pipeline = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("blit"),
                layout: Some(&pl),
                vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
            self.blits.insert(format, (pipeline, layout));
        }
        let sampler = self.blit_sampler.get_or_insert_with(|| {
            self.device.create_sampler(&wgpu::SamplerDescriptor { label: Some("blit"), ..Default::default() })
        });
        let (pipeline, layout) = &self.blits[&format];
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&canvas.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("present") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::WHITE), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
    }

    /// Window `w`'s canvas as RGBA8 rows: (width, height, pixels).
    pub fn pixels(&self, w: WindowId) -> Option<(u32, u32, Vec<u8>)> {
        let c = self.canvases.get(&w)?;
        let row = (c.width * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (row * c.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        enc.copy_texture_to_buffer(
            c.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(c.height) },
            },
            wgpu::Extent3d { width: c.width, height: c.height, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().ok()?.ok()?;
        let data = slice.get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((c.width * c.height * 4) as usize);
        for y in 0..c.height {
            let s = (y * row) as usize;
            out.extend_from_slice(&data[s..s + (c.width * 4) as usize]);
        }
        Some((c.width, c.height, out))
    }

    /// Write window `w`'s canvas to a PNG file.
    pub fn save_png(&self, w: WindowId, path: &std::path::Path) -> Result<(), String> {
        let (width, height, px) = self.pixels(w).ok_or("no canvas for that window")?;
        write_png(path, width, height, &px)
    }
}

pub fn write_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(rgba).map_err(|e| e.to_string())
}

pub fn read_png(path: &std::path::Path) -> Result<(u32, u32, Vec<u8>), String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(f)).read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; dec.output_buffer_size().ok_or("png too large")?];
    let info = dec.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    Ok((info.width, info.height, buf))
}
