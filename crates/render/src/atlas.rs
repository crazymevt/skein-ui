//! The glyph atlas: glyphs rasterized by swash into one alpha texture,
//! packed in shelves, cached by font, glyph, size, variation and subpixel
//! offset (a quarter pixel).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Format, Vector};

const SIZE: u32 = 2048;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    font: u64,
    glyph: u32,
    size: u32,
    coords: u64,
    sub: u8,
}

impl GlyphKey {
    pub fn new(font: &parley::FontData, glyph: u32, size: f32, coords: &[i16], fract: f32) -> GlyphKey {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        coords.hash(&mut h);
        GlyphKey {
            font: font.data.id() ^ ((font.index as u64) << 56),
            glyph,
            size: (size * 64.0).round() as u32,
            coords: h.finish(),
            sub: ((fract.rem_euclid(1.0) * 4.0).floor() as u8).min(3),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub left: i32,
    pub top: i32,
}

pub struct Atlas {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    slots: HashMap<GlyphKey, Option<Slot>>,
    /// Icon masks (SVG paths rasterized by zeno), by a hash of path, size
    /// and stroke.
    masks: HashMap<u64, Option<Slot>>,
    /// Shelves: (y, height, next x).
    shelves: Vec<(u32, u32, u32)>,
    next_y: u32,
    ctx: ScaleContext,
}

impl Atlas {
    pub fn new(device: &wgpu::Device) -> Atlas {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph atlas"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        Atlas { texture, view, sampler, slots: HashMap::new(), masks: HashMap::new(), shelves: Vec::new(), next_y: 0, ctx: ScaleContext::new() }
    }

    pub fn uv(&self, s: &Slot) -> [f32; 4] {
        let f = SIZE as f32;
        [s.x as f32 / f, s.y as f32 / f, s.w as f32 / f, s.h as f32 / f]
    }

    fn place(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let (w, h) = (w + 1, h + 1); // a pixel of padding
        for shelf in &mut self.shelves {
            if h <= shelf.1 && h * 4 >= shelf.1 * 3 && shelf.2 + w <= SIZE {
                let x = shelf.2;
                shelf.2 += w;
                return Some((x, shelf.0));
            }
        }
        if self.next_y + h > SIZE || w > SIZE {
            return None;
        }
        let y = self.next_y;
        self.next_y += h;
        self.shelves.push((y, h, w));
        Some((0, y))
    }

    /// The atlas slot of a glyph, rasterizing it on first use. None if the
    /// font can't be read (the glyph is skipped).
    pub fn glyph(&mut self, queue: &wgpu::Queue, key: GlyphKey, font: &parley::FontData, coords: &[i16]) -> Option<Slot> {
        if let Some(s) = self.slots.get(&key) {
            return *s;
        }
        let slot = self.rasterize(queue, key, font, coords);
        if slot.is_none() && self.next_y > 0 && self.slots.len() > 4096 {
            // Full: start over (the next frames re-rasterize what they show).
            self.slots.clear();
            self.masks.clear();
            self.shelves.clear();
            self.next_y = 0;
            return self.glyph(queue, key, font, coords);
        }
        self.slots.insert(key, slot);
        slot
    }

    /// The atlas slot of an icon: SVG path data in a view x view box,
    /// stroked (width in view units) or filled, at px pixels square.
    pub fn mask(&mut self, queue: &wgpu::Queue, path: &str, view: f32, px: u32, stroke: Option<f32>) -> Option<Slot> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (path, view.to_bits(), px, stroke.map(f32::to_bits)).hash(&mut h);
        let key = h.finish();
        if let Some(s) = self.masks.get(&key) {
            return *s;
        }
        use swash::zeno::{Fill, Mask, Stroke, Style, Transform};
        let k = px as f32 / view;
        let style: Style = match stroke {
            Some(w) => {
                let mut st = Stroke::new(w);
                st.cap(swash::zeno::Cap::Round).join(swash::zeno::Join::Round);
                st.into()
            }
            None => Fill::NonZero.into(),
        };
        let (data, p) = Mask::new(path).style(style).transform(Some(Transform::scale(k, k))).size(px, px).format(Format::Alpha).render();
        let slot = if p.width == 0 || p.height == 0 {
            None
        } else {
            self.place(p.width, p.height).map(|(x, y)| {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture: &self.texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
                    &data,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(p.width), rows_per_image: Some(p.height) },
                    wgpu::Extent3d { width: p.width, height: p.height, depth_or_array_layers: 1 },
                );
                Slot { x, y, w: p.width, h: p.height, left: p.left, top: p.top }
            })
        };
        self.masks.insert(key, slot);
        slot
    }

    fn rasterize(&mut self, queue: &wgpu::Queue, key: GlyphKey, font: &parley::FontData, coords: &[i16]) -> Option<Slot> {
        let font_ref = swash::FontRef::from_index(font.data.as_ref(), font.index as usize)?;
        let mut scaler = self.ctx.builder(font_ref).size(key.size as f32 / 64.0).hint(true).normalized_coords(coords.iter()).build();
        let image = Render::new(&[Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline])
            .format(Format::Alpha)
            .offset(Vector::new(key.sub as f32 / 4.0, 0.0))
            .render(&mut scaler, key.glyph as u16)?;
        let p = image.placement;
        if p.width == 0 || p.height == 0 {
            return Some(Slot { x: 0, y: 0, w: 0, h: 0, left: 0, top: 0 });
        }
        let alpha: Vec<u8> = match image.content {
            Content::Mask => image.data,
            // Color glyphs (emoji) become their alpha until the atlas has color.
            _ => image.data.chunks(4).map(|px| px[3]).collect(),
        };
        let (x, y) = self.place(p.width, p.height)?;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &self.texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
            &alpha,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(p.width), rows_per_image: Some(p.height) },
            wgpu::Extent3d { width: p.width, height: p.height, depth_or_array_layers: 1 },
        );
        Some(Slot { x, y, w: p.width, h: p.height, left: p.left, top: p.top })
    }
}
