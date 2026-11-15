//! Decode file paths and encoded byte strings into straight RGBA8.

use crate::Data;
use crate::data::NodeId;
use image::{DynamicImage, ImageBuffer, ImageDecoder, ImageReader, Limits, Rgba, imageops};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Cursor, Seek};
use std::sync::Arc;

const MAX_DIMENSION: u32 = 8192;
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub struct RasterImage {
    pub size: [u32; 2],
    pub rgba: Arc<Vec<u8>>,
}

/// An image to decode, and where its result goes (`Engine::decoded`).
#[derive(Clone, Debug)]
pub enum DecodeRequest {
    /// An image node's `:src`.
    Node { id: NodeId, src: Data },
    /// A `ui/decode-image` request, answered with an `image-decoded` event.
    Event { req: u64, src: Data, size: Option<Data> },
}

impl DecodeRequest {
    /// Decode it. Safe on any thread: it touches no engine state.
    pub fn decode(&self) -> Result<RasterImage, String> {
        match self {
            DecodeRequest::Node { src, .. } | DecodeRequest::Event { src, size: None | Some(Data::Nil), .. } => {
                RasterImage::load(src)
            }
            DecodeRequest::Event { src, size: Some(size), .. } => RasterImage::load_scaled(src, size),
        }
    }
}

impl RasterImage {
    pub fn load(src: &Data) -> Result<Self, String> {
        match src {
            Data::Bytes(bytes) => Self::decode(Cursor::new(bytes.as_slice()), bytes.len() as u64),
            Data::Str(path) => {
                let meta = fs::metadata(path).map_err(|e| e.to_string())?;
                if !meta.is_file() {
                    return Err("expected a regular image file".into());
                }
                let file = File::open(path).map_err(|e| e.to_string())?;
                Self::decode(BufReader::new(file), meta.len()).map_err(|e| format!("{path}: {e}"))
            }
            _ => Err("expected a path or encoded image bytes".into()),
        }
    }

    pub(crate) fn load_scaled(src: &Data, size: &Data) -> Result<Self, String> {
        let dim = |value: &Data| {
            let n = value.as_num()?;
            (n > 0.0 && n <= f64::from(MAX_DIMENSION) && n.fract() == 0.0).then_some(n as u32)
        };
        let invalid_size = || "expected an output size [width height] with integer dimensions from 1 to 8192".to_owned();
        let Some([w, h]) = size.as_list() else { return Err(invalid_size()) };
        let (w, h) = dim(w).zip(dim(h)).ok_or_else(invalid_size)?;
        if u64::from(w) * u64::from(h) > MAX_IMAGE_BYTES / 4 {
            return Err("resized image exceeds 64 MiB".into());
        }

        let image = Self::load(src)?;
        if image.size == [w, h] {
            return Ok(image);
        }
        Ok(image.resize([w, h]))
    }

    fn resize(self, [w, h]: [u32; 2]) -> Self {
        let Self { size: [width, height], rgba } = self;
        let premultiplied = ImageBuffer::from_fn(width, height, |x, y| {
            let i = 4 * (y * width + x) as usize;
            let a = u16::from(rgba[i + 3]);
            // Use 16 bits to preserve the full product of two 8-bit channels.
            Rgba([u16::from(rgba[i]) * a, u16::from(rgba[i + 1]) * a, u16::from(rgba[i + 2]) * a, 255 * a])
        });
        drop(rgba);
        let filter = imageops::FilterType::Triangle;
        // The resizer filters vertically first. Shrink the other axis first
        // when growing vertically to keep its intermediate buffer bounded.
        let resized = if w < width && h > height {
            let rotated = imageops::rotate90(&premultiplied);
            drop(premultiplied);
            imageops::rotate270(&imageops::resize(&rotated, h, w, filter))
        } else {
            imageops::resize(&premultiplied, w, h, filter)
        };
        let rgba = resized
            .pixels()
            .flat_map(|&Rgba([r, g, b, a])| {
                let a = u32::from(a);
                let alpha = ((a + 127) / 255) as u8;
                if alpha == 0 {
                    return [0; 4];
                }
                let straight = |c: u16| ((u32::from(c) * 255 + a / 2) / a).min(255) as u8;
                [straight(r), straight(g), straight(b), alpha]
            })
            .collect::<Vec<_>>();
        Self { size: [w, h], rgba: rgba.into() }
    }

    fn decode(input: impl BufRead + Seek, len: u64) -> Result<Self, String> {
        if len > MAX_IMAGE_BYTES {
            return Err("encoded image exceeds 64 MiB".into());
        }
        let mut reader = ImageReader::new(input).with_guessed_format().map_err(|e| e.to_string())?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_DIMENSION);
        limits.max_image_height = Some(MAX_DIMENSION);
        limits.max_alloc = Some(256 * 1024 * 1024);
        reader.limits(limits.clone());
        let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
        let (width, height) = decoder.dimensions();
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_IMAGE_BYTES / 4 {
            return Err("invalid dimensions or decoded image exceeds 64 MiB".into());
        }
        // The decoder's output may have more than four bytes per pixel.
        limits.reserve(decoder.total_bytes()).map_err(|e| e.to_string())?;
        decoder.set_limits(limits).map_err(|e| e.to_string())?;
        let orientation = decoder.orientation().map_err(|e| e.to_string())?;
        let mut image = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
        image.apply_orientation(orientation);
        let rgba = image.into_rgba8();
        Ok(Self { size: [rgba.width(), rgba.height()], rgba: rgba.into_raw().into() })
    }
}
