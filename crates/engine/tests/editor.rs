//! The editor surface: line requests, line-exact repaints, scrolling,
//! clicks to line and column.

use skein_ui_engine::{Data, Engine, Op, Props, RoughMeasure};

fn p(kvs: &[(&str, Data)]) -> Props {
    kvs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}
fn n(x: f64) -> Data {
    Data::Num(x)
}

/// A 300×100 editor, font 10 × line-height 2.0: 20-pixel lines, 6-pixel
/// columns with the rough measure.
fn setup() -> Engine {
    let mut e = Engine::new();
    assert!(e.apply(vec![
        Op::Create { id: 1, kind: "editor".into(), props: p(&[("on", Data::Set(vec![Data::key("click")]))]) },
        Op::Style { id: 1, style: p(&[("font-size", n(10.0)), ("line-height", n(2.0))]) },
        Op::Window { id: 1, props: p(&[("root", n(1.0)), ("size", Data::List(vec![n(300.0), n(100.0)]))]) },
    ]));
    e
}

fn lines(from: usize, texts: &[&str]) -> Op {
    Op::Editor { id: 1, op: "lines".into(), args: vec![n(from as f64), Data::List(texts.iter().map(|t| Data::str(t)).collect())] }
}

#[test]
fn it_asks_for_the_lines_in_view_and_repaints_only_changed_lines() {
    let mut e = setup();
    e.frame(1, &mut RoughMeasure);
    let ev = e.take_events();
    assert_eq!(ev[0].kind, "need-lines");
    assert_eq!((ev[0].get("from"), ev[0].get("to")), (Some(&n(0.0)), Some(&n(1.0))));
    let texts: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    assert!(e.apply(vec![Op::Editor { id: 1, op: "line-count".into(), args: vec![n(100.0)] }, lines(0, &refs[..10])]));
    e.frame(1, &mut RoughMeasure);
    // Everything in view is cached: no more requests, and idle draws nothing.
    assert!(e.take_events().iter().all(|x| x.kind != "need-lines"));
    assert!(e.frame(1, &mut RoughMeasure).is_empty());
    // One changed line repaints that line only; an unchanged one, nothing.
    assert!(e.apply(vec![lines(2, &["line 2 edited"]), lines(3, &["line 3"])]));
    let d = e.frame(1, &mut RoughMeasure);
    assert_eq!(d.len(), 1);
    assert_eq!((d[0].y, d[0].h, d[0].w), (40.0, 20.0, 300.0));
}

#[test]
fn scrolling_asks_for_new_lines_and_clicks_give_positions() {
    let mut e = setup();
    e.frame(1, &mut RoughMeasure);
    e.take_events();
    assert!(e.apply(vec![Op::Editor { id: 1, op: "line-count".into(), args: vec![n(100.0)] }, lines(0, &["a"; 10])]));
    e.frame(1, &mut RoughMeasure);
    e.take_events();
    e.wheel(1, 100.0, 50.0, 0.0, 400.0, &mut RoughMeasure);
    let ev = e.take_events();
    let need = ev.iter().find(|x| x.kind == "need-lines").expect("a request");
    assert_eq!(need.get("from"), Some(&n(20.0)));
    // A click at the third visible line, column 2 (gutter: 4 columns + 4 px).
    let gutter = e.editors[&1].gutter_w;
    e.pointer_down(1, gutter + 4.0 + 12.0, 45.0, &[], &mut RoughMeasure);
    e.pointer_up(1, gutter + 4.0 + 12.0, 45.0, "left", 1, &[]);
    let click = e.take_events().into_iter().find(|x| x.kind == "click").unwrap();
    assert_eq!(click.get("line"), Some(&n(22.0)));
    assert_eq!(e.focused, Some(1));
}

#[test]
fn a_reveal_before_layout_waits_for_it() {
    // Revealing line 0 before the editor has a size would scroll as if the
    // view were a line short, hiding line 0; it waits for the layout.
    let mut e = Engine::new();
    assert!(e.apply(vec![
        Op::Create { id: 1, kind: "editor".into(), props: p(&[]) },
        Op::Style { id: 1, style: p(&[("font-size", n(10.0)), ("line-height", n(2.0))]) },
        Op::Editor { id: 1, op: "line-count".into(), args: vec![n(2.0)] },
        Op::Editor { id: 1, op: "reveal".into(), args: vec![n(0.0), n(0.0)] },
        Op::Window { id: 1, props: p(&[("root", n(1.0)), ("size", Data::List(vec![n(300.0), n(100.0)]))]) },
    ]));
    e.frame(1, &mut RoughMeasure);
    assert_eq!(e.editors[&1].scroll_y, 0.0);
    // and a reveal after layout still scrolls when it has to
    assert!(e.apply(vec![Op::Editor { id: 1, op: "line-count".into(), args: vec![n(100.0)] },
                         Op::Editor { id: 1, op: "reveal".into(), args: vec![n(50.0)] }]));
    assert_eq!(e.editors[&1].scroll_y, 51.0 * 20.0 - 100.0);
}

#[test]
fn an_editors_scroll_bar_drags() {
    let mut e = setup();
    e.frame(1, &mut RoughMeasure);
    e.take_events();
    assert!(e.apply(vec![Op::Editor { id: 1, op: "line-count".into(), args: vec![n(100.0)] }]));
    e.frame(1, &mut RoughMeasure);
    let t = e.editor_thumb(&e.windows[&1], 1).expect("a thumb: 100 lines of 20 pixels in 100");
    let (tx, ty) = (t.x + 3.0, t.y + 5.0);
    e.pointer_down(1, tx, ty, &[], &mut RoughMeasure);
    e.pointer_moved(1, tx, 100.0, &mut RoughMeasure);
    e.pointer_up(1, tx, 100.0, "left", 1, &[]);
    assert_eq!(e.editors[&1].scroll_y, 100.0 * 20.0 - 100.0, "dragged to the end");
    // and that wasn't a click in the text
    assert!(e.take_events().iter().all(|x| x.kind != "click"));
}
