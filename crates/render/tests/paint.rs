//! Headless painting: pixels, damage-only repaint, and a golden image.
//! Set UPDATE_GOLDEN=1 to rewrite tests/golden/*.png after a deliberate
//! change; review the new images before committing them.

use skein_ui_engine::{Data, Engine, Op, Props, Rect, Text};
use skein_ui_render::{Item, PixelSampling, Renderer, display_list, read_png, write_png};

/// Held by a test for as long as it has a renderer: one GPU device at a
/// time. (On Windows CI's runners, which have no GPU, the paint tests
/// sometimes ended in an access violation, after one test or after five,
/// with each test making its own device beside the others'.)
static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    // (a test that panicked while holding it leaves it for the next)
    GPU.lock().unwrap_or_else(|e| e.into_inner())
}

fn p(kvs: &[(&str, Data)]) -> Props {
    kvs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}
fn k(s: &str) -> Data {
    Data::key(s)
}
fn n(x: f64) -> Data {
    Data::Num(x)
}
fn s(x: &str) -> Data {
    Data::str(x)
}

/// A 240×120 window: a dark header with a title, and a rounded, bordered
/// blue button with a label.
fn scene() -> Engine {
    let mut e = Engine::new();
    assert!(e.apply(vec![
        Op::Create { id: 1, kind: "box".into(), props: vec![] },
        Op::Style { id: 1, style: p(&[("flex-direction", k("column")), ("background", s("#f0f0f0"))]) },
        Op::Create { id: 2, kind: "box".into(), props: vec![] },
        Op::Style { id: 2, style: p(&[("height", n(40.0)), ("padding", n(10.0)), ("background", s("#202830"))]) },
        Op::Create { id: 3, kind: "text".into(), props: p(&[("text", s("Skein UI"))]) },
        Op::Style { id: 3, style: p(&[("color", s("#ffffff")), ("font-size", n(16.0)), ("font-weight", k("bold"))]) },
        Op::Create { id: 4, kind: "button".into(), props: vec![] },
        Op::Style {
            id: 4,
            style: p(&[
                ("margin", n(20.0)),
                ("padding", Data::List(vec![n(8.0), n(16.0)])),
                ("background", s("#3070e0")),
                ("border-radius", n(8.0)),
                ("border", Data::List(vec![n(2.0), s("#103070")])),
                ("align-self", k("start")),
            ]),
        },
        Op::Create { id: 5, kind: "text".into(), props: p(&[("text", s("Click me"))]) },
        Op::Style { id: 5, style: p(&[("color", s("#ffffff"))]) },
        Op::Insert { parent: 2, index: 0, id: 3 },
        Op::Insert { parent: 4, index: 0, id: 5 },
        Op::Insert { parent: 1, index: 0, id: 2 },
        Op::Insert { parent: 1, index: 1, id: 4 },
        Op::Window { id: 1, props: p(&[("root", n(1.0)), ("size", Data::List(vec![n(240.0), n(120.0)]))]) },
    ]));
    e
}

fn px(img: &(u32, u32, Vec<u8>), x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.0 + x) * 4) as usize;
    [img.2[i], img.2[i + 1], img.2[i + 2], img.2[i + 3]]
}

fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2)
}

#[test]
fn canvas_pixels_scale_clip_blend_and_update() {
    let rect = |x, y, w, h| Data::List(vec![n(x), n(y), n(w), n(h)]);
    let pixels = |rgba: Vec<u8>| {
        Data::List(vec![k("pixels"), rect(0.0, 0.0, 8.0, 8.0), Data::List(vec![n(2.0), n(2.0)]), Data::Bytes(rgba.into())])
    };
    let commands = |rgba| {
        let translated = Data::List(vec![k("translate"), Data::List(vec![n(2.0), n(2.0)]), Data::List(vec![pixels(rgba)])]);
        Data::List(vec![
            Data::List(vec![k("clip"), rect(3.0, 2.0, 6.0, 8.0), Data::List(vec![translated])]),
            // A malformed upload is ignored and later drawing stays on top.
            pixels(vec![0, 0, 0, 255]),
            Data::List(vec![k("rect"), rect(4.0, 3.0, 2.0, 2.0), Data::Map(vec![(k("fill"), s("#000000"))])]),
        ])
    };
    let source = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 128, 0, 0, 0, 0];
    let mut e = Engine::new();
    assert!(e.apply(vec![
        Op::Create { id: 1, kind: "canvas".into(), props: p(&[("commands", commands(source.to_vec()))]) },
        Op::Window { id: 1, props: p(&[("root", n(1.0)), ("size", Data::List(vec![n(12.0), n(12.0)])), ("background", s("#ffffff"))]) },
    ]));
    let mut t = Text::new();
    let damage = e.frame(1, &mut t);
    let items = display_list(&e, 1, &mut t, &damage);
    let [Item::Box { .. }, Item::Pixels { rect, size, rgba, sampling, clip }, Item::Box { .. }] = items.as_slice() else {
        panic!("expected one valid pixel upload between the two boxes");
    };
    assert_eq!(*rect, Rect { x: 2.0, y: 2.0, w: 8.0, h: 8.0 });
    assert_eq!(*clip, Rect { x: 3.0, y: 2.0, w: 6.0, h: 8.0 });
    assert_eq!(*size, [2, 2]);
    assert_eq!(rgba.as_slice(), source);
    assert_eq!(*sampling, PixelSampling::Nearest);

    let _gpu = one_at_a_time();
    let Ok(mut r) = Renderer::headless() else {
        eprintln!("no GPU adapter: skipping pixel readback");
        return;
    };
    r.render(&e, 1, &mut t, &damage, 2.0);
    let img = r.pixels(1).unwrap();
    assert_eq!(px(&img, 2, 2), [255, 255, 255, 255]);
    assert_eq!(px(&img, 5, 5), [255, 255, 255, 255]);
    assert_eq!(px(&img, 6, 5), [255, 0, 0, 255]);
    assert_eq!(px(&img, 9, 8), [0, 0, 0, 255]);
    assert_eq!(px(&img, 12, 5), [0, 255, 0, 255]);
    assert_eq!(px(&img, 18, 5), [255, 255, 255, 255]);
    assert!(near(px(&img, 7, 15), [127, 127, 255, 255]));
    assert_eq!(px(&img, 14, 15), [255, 255, 255, 255]);

    assert!(e.apply(vec![Op::Set { id: 1, props: p(&[("commands", commands([255, 255, 0, 255].repeat(4)))]) }]));
    let damage = e.frame(1, &mut t);
    r.render(&e, 1, &mut t, &damage, 2.0);
    assert_eq!(px(&r.pixels(1).unwrap(), 7, 5), [255, 255, 0, 255]);
}

#[test]
fn image_fit_places_and_clips_the_decoded_pixels() {
    let mut e = Engine::new();
    let src = Data::Bytes(b"P6\n2 1\n255\n\xff\x00\x00\x00\x00\xff".to_vec().into());
    assert!(e.apply(vec![
        Op::Create { id: 1, kind: "image".into(), props: p(&[("src", src)]) },
        Op::Window { id: 1, props: p(&[("root", n(1.0)), ("size", Data::List(vec![n(20.0), n(20.0)]))]) },
    ]));
    let box_rect = Rect { x: 0.0, y: 0.0, w: 20.0, h: 20.0 };
    let mut t = Text::new();
    for (fit, expected) in [
        ("contain", Rect { y: 5.0, h: 10.0, ..box_rect }),
        ("cover", Rect { x: -10.0, w: 40.0, ..box_rect }),
        ("fill", box_rect),
    ] {
        assert!(e.apply(vec![Op::Set { id: 1, props: p(&[("fit", k(fit))]) }]));
        let damage = e.frame(1, &mut t);
        let items = display_list(&e, 1, &mut t, &damage);
        let [Item::Box { .. }, Item::Pixels { rect, size, rgba, sampling, clip }] = items.as_slice() else {
            panic!("expected one image above the background");
        };
        assert_eq!(*rect, expected);
        assert_eq!(*clip, box_rect);
        assert_eq!(*size, [2, 1]);
        assert_eq!(rgba.as_slice(), [255, 0, 0, 255, 0, 0, 255, 255]);
        assert_eq!(*sampling, PixelSampling::Linear);
    }
}

#[test]
fn image_sampling_blends_without_transparent_color_bleed() {
    let mut e = Engine::new();
    let src = b"P7\nWIDTH 2\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n\xff\x00\x00\xff\x00\x00\xff\x00";
    assert!(e.apply(vec![
        Op::Create { id: 1, kind: "image".into(), props: p(&[("src", Data::Bytes(src.to_vec().into())), ("fit", k("fill"))]) },
        Op::Window { id: 1, props: p(&[("root", n(1.0)), ("size", Data::List(vec![n(15.0), n(3.0)])), ("background", s("#ffffff"))]) },
    ]));
    let mut t = Text::new();
    let damage = e.frame(1, &mut t);
    assert_eq!(e.nodes[&1].raster.as_ref().unwrap().rgba.as_slice(), [255, 0, 0, 255, 0, 0, 255, 0]);

    let _gpu = one_at_a_time();
    let Ok(mut r) = Renderer::headless() else {
        eprintln!("no GPU adapter: skipping pixel readback");
        return;
    };
    r.render(&e, 1, &mut t, &damage, 1.0);
    let smooth = r.pixels(1).unwrap();
    assert_eq!(px(&smooth, 0, 1), [255, 0, 0, 255]);
    assert!(near(px(&smooth, 7, 1), [255, 128, 128, 255]));
    assert_eq!(px(&smooth, 14, 1), [255, 255, 255, 255]);

    assert!(e.apply(vec![Op::Set { id: 1, props: p(&[("sampling", k("nearest"))]) }]));
    let damage = e.frame(1, &mut t);
    r.render(&e, 1, &mut t, &damage, 1.0);
    assert_eq!(px(&r.pixels(1).unwrap(), 6, 1), [255, 0, 0, 255]);
}

#[test]
fn boxes_text_and_damage_only_repaint() {
    let _gpu = one_at_a_time();
    let Ok(mut r) = Renderer::headless() else {
        eprintln!("no GPU adapter: skipping");
        return;
    };
    let mut e = scene();
    let mut t = Text::new();
    let damage = e.frame(1, &mut t);
    r.render(&e, 1, &mut t, &damage, 1.0);
    let img = r.pixels(1).unwrap();
    assert_eq!((img.0, img.1), (240, 120));
    assert!(near(px(&img, 230, 110), [0xf0, 0xf0, 0xf0, 255]), "{:?}", px(&img, 230, 110));
    assert!(near(px(&img, 230, 5), [0x20, 0x28, 0x30, 255]));
    let b = e.rect(4).unwrap();
    // Inside the button, off the label: the fill; on its edge: the border;
    // its rounded corner shows the background.
    assert!(near(px(&img, b.x as u32 + 4, (b.y + b.h / 2.0) as u32), [0x30, 0x70, 0xe0, 255]));
    assert!(near(px(&img, b.x as u32, (b.y + b.h / 2.0) as u32), [0x10, 0x30, 0x70, 255]));
    assert!(near(px(&img, b.x as u32, b.y as u32), [0xf0, 0xf0, 0xf0, 255]));
    // The title is drawn: bright pixels in the header where the text is.
    let t3 = e.rect(3).unwrap();
    let bright = (t3.x as u32..(t3.x + t3.w) as u32)
        .flat_map(|x| (t3.y as u32..(t3.y + t3.h) as u32).map(move |y| (x, y)))
        .filter(|&(x, y)| px(&img, x, y)[0] > 200)
        .count();
    assert!(bright > 40, "{bright}");

    // A paint-only change repaints the button and nothing else.
    e.apply(vec![Op::Style { id: 4, style: p(&[("background", s("#e03030"))]) }]);
    let damage = e.frame(1, &mut t);
    assert_eq!(damage.len(), 1);
    r.render(&e, 1, &mut t, &damage, 1.0);
    let after = r.pixels(1).unwrap();
    assert!(near(px(&after, b.x as u32 + 4, (b.y + b.h / 2.0) as u32), [0xe0, 0x30, 0x30, 255]));
    let changed = (0..240u32)
        .flat_map(|x| (0..120u32).map(move |y| (x, y)))
        .filter(|&(x, y)| px(&img, x, y) != px(&after, x, y))
        .filter(|&(x, y)| !damage[0].contains(x as f32 + 0.5, y as f32 + 0.5))
        .count();
    assert_eq!(changed, 0, "pixels changed outside the damage");

    // Idle: no damage, nothing drawn.
    let frames = r.frames;
    let damage = e.frame(1, &mut t);
    r.render(&e, 1, &mut t, &damage, 1.0);
    assert_eq!(r.frames, frames);
}

#[test]
fn the_scene_matches_its_golden_image() {
    let _gpu = one_at_a_time();
    let Ok(mut r) = Renderer::headless() else {
        eprintln!("no GPU adapter: skipping");
        return;
    };
    let mut e = scene();
    let mut t = Text::new();
    let damage = e.frame(1, &mut t);
    r.render(&e, 1, &mut t, &damage, 2.0);
    let (w, h, got) = r.pixels(1).unwrap();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/scene@2x.png");
    if std::env::var("UPDATE_GOLDEN").is_ok_and(|v| v == "1") || !path.exists() {
        write_png(&path, w, h, &got).unwrap();
        eprintln!("wrote {}", path.display());
        return;
    }
    let (gw, gh, want) = read_png(&path).unwrap();
    assert_eq!((w, h), (gw, gh));
    let differ = got.chunks(4).zip(want.chunks(4)).filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 3)).count();
    // A little slack for differences between GPUs' rasterization.
    assert!(differ * 200 <= (w * h) as usize, "{differ} pixels differ from {}", path.display());
}

#[test]
fn scrolled_content_is_clipped_to_its_container() {
    let _gpu = one_at_a_time();
    let Ok(mut r) = Renderer::headless() else {
        eprintln!("no GPU adapter: skipping");
        return;
    };
    let mut e = Engine::new();
    let mut ops = vec![
        Op::Create { id: 1, kind: "box".into(), props: vec![] },
        Op::Style { id: 1, style: p(&[("flex-direction", k("column")), ("background", s("#ffffff"))]) },
        Op::Create { id: 2, kind: "scroll".into(), props: vec![] },
        Op::Style { id: 2, style: p(&[("height", n(50.0))]) },
        Op::Insert { parent: 1, index: 0, id: 2 },
    ];
    for (i, c) in ["#ff0000", "#00ff00", "#0000ff", "#000000"].iter().enumerate() {
        let id = 10 + i as u64;
        ops.push(Op::Create { id, kind: "box".into(), props: vec![] });
        ops.push(Op::Style { id, style: p(&[("height", n(25.0)), ("background", s(c))]) });
        ops.push(Op::Insert { parent: 2, index: i, id });
    }
    ops.push(Op::Window { id: 1, props: p(&[("root", n(1.0)), ("size", Data::List(vec![n(40.0), n(100.0)]))]) });
    assert!(e.apply(ops));
    let mut t = Text::new();
    let d = e.frame(1, &mut t);
    r.render(&e, 1, &mut t, &d, 1.0);
    let img = r.pixels(1).unwrap();
    assert!(near(px(&img, 20, 10), [255, 0, 0, 255]));
    assert!(near(px(&img, 20, 40), [0, 255, 0, 255]));
    // Rows 3 and 4 lie below the container: white, not blue or black.
    assert!(near(px(&img, 20, 60), [255, 255, 255, 255]), "{:?}", px(&img, 20, 60));
    e.wheel(1, 20.0, 20.0, 0.0, 50.0, &mut t);
    let d = e.frame(1, &mut t);
    r.render(&e, 1, &mut t, &d, 1.0);
    let img = r.pixels(1).unwrap();
    assert!(near(px(&img, 20, 10), [0, 0, 255, 255]));
    assert!(near(px(&img, 20, 40), [0, 0, 0, 255]));
    assert!(near(px(&img, 20, 60), [255, 255, 255, 255]));
}
