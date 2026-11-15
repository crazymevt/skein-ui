use image::{DynamicImage, ImageFormat, RgbImage, RgbaImage};
use norn_ui_engine::{Data, Engine, Op, raster::RasterImage};
use std::io::Cursor;

fn encode(image: &DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, format).unwrap();
    encoded.into_inner()
}

#[test]
fn detects_each_format_and_decodes_to_rgba8() {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(3, 2, image::Rgb([40, 100, 170])));
    for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Bmp, ImageFormat::Tiff, ImageFormat::Ico, ImageFormat::Pnm] {
        let encoded = if format == ImageFormat::Ico {
            encode(&DynamicImage::ImageRgba8(image.to_rgba8()), format)
        } else {
            encode(&image, format)
        };
        let src = Data::Bytes(encoded.into());
        let raster = RasterImage::load(&src).unwrap_or_else(|e| panic!("{format:?}: {e}"));
        assert_eq!(raster.size, [3, 2]);
        assert_eq!(raster.rgba.len(), 24);
        for pixel in raster.rgba.chunks_exact(4) {
            assert_eq!(pixel[3], 255);
            assert!(pixel[..3].iter().zip([40, 100, 170]).all(|(a, b)| a.abs_diff(b) <= 3), "{format:?}: {pixel:?}");
        }
    }
}

#[test]
fn jpeg_applies_exif_orientation() {
    let image = DynamicImage::ImageRgb8(RgbImage::new(3, 2));
    let mut jpeg = encode(&image, ImageFormat::Jpeg);
    // EXIF APP1 marker with one little-endian orientation tag (rotate 90°).
    let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
    let mut marker = vec![0xff, 0xe1];
    marker.extend(((exif.len() + 2) as u16).to_be_bytes());
    marker.extend(exif);
    jpeg.splice(2..2, marker);
    let raster = RasterImage::load(&Data::Bytes(jpeg.into())).unwrap();
    assert_eq!(raster.size, [2, 3]);
}

#[test]
fn pixels_preserve_row_order_and_straight_alpha() {
    let pixels = vec![255, 8, 12, 128, 24, 38, 51, 255, 2, 100, 9, 0, 1, 16, 3, 70];
    let image = DynamicImage::ImageRgba8(RgbaImage::from_raw(2, 2, pixels.clone()).unwrap());
    let raster = RasterImage::load(&Data::Bytes(encode(&image, ImageFormat::Png).into())).unwrap();
    assert_eq!(raster.rgba.as_slice(), pixels);
}

#[test]
fn resizes_decoded_pixels_with_alpha_weighted_filtering() {
    let pixels = vec![255, 0, 0, 255, 0, 0, 255, 0];
    let image = DynamicImage::ImageRgba8(RgbaImage::from_raw(2, 1, pixels.clone()).unwrap());
    let src = Data::Bytes(encode(&image, ImageFormat::Png).into());
    let mut engine = Engine::new();
    for (dimensions, expected) in [
        ([2.0, 1.0], pixels),
        ([3.0, 1.0], vec![255, 0, 0, 255, 255, 0, 0, 128, 0, 0, 0, 0]),
        ([1.0, 3.0], [255, 0, 0, 128].repeat(3)),
    ] {
        let size = Data::List(dimensions.into_iter().map(Data::Num).collect());
        assert!(engine.apply(vec![Op::DecodeImage { req: 1, src: src.clone(), size: Some(size.clone()) }]));
        let event = engine.take_events().pop().unwrap();
        assert_eq!(event.get("size"), Some(&size));
        assert_eq!(event.get("rgba"), Some(&Data::Bytes(expected.into())));
    }
}

#[test]
fn rejects_truncated_and_oversized_images() {
    let broken = Data::Bytes(b"P6\n2 2\n255\n".to_vec().into());
    assert!(RasterImage::load(&broken).is_err());
    let oversized = Data::Bytes(b"P6\n8192 8192\n255\n".to_vec().into());
    assert!(RasterImage::load(&oversized).unwrap_err().contains("exceeds 64 MiB"));
}
