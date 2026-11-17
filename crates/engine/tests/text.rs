//! Text with the bundled fonts: families, measuring, wrapping, glyph runs,
//! and layout using real text sizes.

use skein_ui_engine::{Data, Engine, Op, Props, Text, TextStyle};

fn style(family: &str, size: f32) -> TextStyle {
    TextStyle { family: family.into(), size, ..TextStyle::default() }
}

#[test]
fn the_bundled_families_are_registered() {
    let t = Text::new();
    let (ui, mono) = t.families();
    assert!(ui.starts_with("Inter"), "{ui}");
    assert!(mono.starts_with("JetBrains Mono"), "{mono}");
}

#[test]
fn code_is_monospaced_and_ui_text_is_not() {
    let mut t = Text::new();
    let code = style("code", 14.0);
    let (iw, _) = t.measure_text("iiiiiiiiii", &code, None);
    let (ww, _) = t.measure_text("WWWWWWWWWW", &code, None);
    assert_eq!(iw, ww);
    // JetBrains Mono advances 600 units per 1000: 0.6 em.
    assert!((iw - 84.0).abs() <= 1.0, "{iw}");
    let ui = style("ui", 14.0);
    let (iw, _) = t.measure_text("iiiiiiiiii", &ui, None);
    let (ww, _) = t.measure_text("WWWWWWWWWW", &ui, None);
    assert!(iw * 2.0 < ww, "{iw} {ww}");
}

#[test]
fn text_wraps_and_lines_stack() {
    let mut t = Text::new();
    let s = style("ui", 14.0);
    let (w1, h1) = t.measure_text("The quick brown fox", &s, None);
    assert!((h1 - (14.0f32 * 1.3).ceil()).abs() <= 1.0, "{h1}");
    let (w2, h2) = t.measure_text("The quick brown fox jumps over the lazy dog", &s, Some(w1 + 1.0));
    assert!(w2 <= w1 + 1.0, "{w2} {w1}");
    assert!(h2 >= 2.0 * h1 - 1.0, "{h2} {h1}");
    let (_, h3) = t.measure_text("one\ntwo\nthree", &s, None);
    assert!(h3 >= 3.0 * 14.0 * 1.3 - 1.0, "{h3}");
    // Larger sizes measure larger; bold measures wider.
    let (wb, _) = t.measure_text("Hello", &TextStyle { weight: 700.0, ..s.clone() }, None);
    let (wr, _) = t.measure_text("Hello", &s, None);
    assert!(wb > wr);
}

#[test]
fn glyph_runs_carry_fonts_positions_and_colors() {
    let mut t = Text::new();
    let s = TextStyle { color: [10, 20, 30, 255], ..style("code", 20.0) };
    let l = t.layout("ab é", &s, None);
    let runs = Text::glyph_runs(&l);
    let glyphs: usize = runs.iter().map(|r| r.glyphs.len()).sum();
    assert_eq!(glyphs, 4);
    assert_eq!(runs[0].color, [10, 20, 30, 255]);
    assert_eq!(runs[0].size, 20.0);
    let xs: Vec<f32> = runs.iter().flat_map(|r| r.glyphs.iter().map(|g| g.1)).collect();
    assert!(xs.windows(2).all(|w| w[1] > w[0]), "{xs:?}");
}

#[test]
fn the_engine_lays_out_with_real_text() {
    let mut e = Engine::new();
    let p = |kvs: &[(&str, Data)]| -> Props { kvs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect() };
    assert!(e.apply(vec![
        Op::Create { id: 1, kind: "box".into(), props: vec![] },
        Op::Style { id: 1, style: p(&[("flex-direction", Data::key("column")), ("align-items", Data::key("start"))]) },
        Op::Create { id: 2, kind: "text".into(), props: p(&[("text", Data::str("Hello, world"))]) },
        Op::Style { id: 2, style: p(&[("font-family", Data::key("code")), ("font-size", Data::Num(10.0))]) },
        Op::Insert { parent: 1, index: 0, id: 2 },
        Op::Window { id: 1, props: p(&[("root", Data::Num(1.0)), ("size", Data::List(vec![Data::Num(300.0), Data::Num(100.0)]))]) },
    ]));
    let mut text = Text::new();
    e.frame(1, &mut text);
    let r = e.rect(2).unwrap();
    // 12 characters × 0.6 × 10 = 72, one line of 13.
    assert!((r.w - 72.0).abs() <= 1.0 && (r.h - 13.0).abs() <= 1.0, "{r:?}");
}
