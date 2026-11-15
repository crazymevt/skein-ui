//! Text fields: editing in the engine, events for Norn, the caret blink.

use norn_ui_engine::{Data, Engine, Op, Props, Rect, RoughMeasure};
use std::time::{Duration, Instant};

fn p(kvs: &[(&str, Data)]) -> Props {
    kvs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

/// A window with a 200-wide field (padding 4, font 10: 6 px per character
/// with the rough measure) that wants :input and :submit.
fn setup() -> Engine {
    let mut e = Engine::new();
    assert!(e.apply(vec![
        Op::Create { id: 1, kind: "box".into(), props: vec![] },
        Op::Style { id: 1, style: p(&[("padding", Data::Num(10.0)), ("align-items", Data::key("start"))]) },
        Op::Create {
            id: 2,
            kind: "text-field".into(),
            props: p(&[("value", Data::str("hello")), ("on", Data::Set(vec![Data::key("input"), Data::key("submit")]))]),
        },
        Op::Style { id: 2, style: p(&[("width", Data::Num(200.0)), ("padding", Data::Num(4.0)), ("font-size", Data::Num(10.0))]) },
        Op::Insert { parent: 1, index: 0, id: 2 },
        Op::Window { id: 1, props: p(&[("root", Data::Num(1.0)), ("size", Data::List(vec![Data::Num(300.0), Data::Num(100.0)]))]) },
    ]));
    e.frame(1, &mut RoughMeasure);
    e.take_events();
    e
}

fn click_at(e: &mut Engine, x: f32, mods: &[&str]) {
    e.pointer_down(1, x, 18.0, mods, &mut RoughMeasure);
    e.pointer_up(1, x, 18.0, "left", 1, mods);
}

fn value(e: &Engine) -> &str {
    &e.fields[&2].value
}

#[test]
fn typing_editing_and_events() {
    let mut e = setup();
    let m = &mut RoughMeasure;
    // Click after "he" (field content starts at 14; 2 chars = 12 px).
    click_at(&mut e, 14.0 + 12.0, &[]);
    assert_eq!(e.focused, Some(2));
    assert_eq!(e.fields[&2].caret, 2);
    e.text_input(1, "y", m);
    e.text_input(1, "!", m);
    assert_eq!(value(&e), "hey!llo");
    // Two inputs, one :input event (coalesced), with the latest value.
    let ev = e.take_events();
    assert_eq!(ev.iter().filter(|x| x.kind == "input").count(), 1);
    assert_eq!(ev[0].get("value"), Some(&Data::str("hey!llo")));
    e.key(1, Data::key("backspace"), &[], false, m);
    assert_eq!(value(&e), "heyllo");
    e.key(1, Data::key("arrow-left"), &["shift"], false, m);
    e.key(1, Data::key("arrow-left"), &["shift"], false, m);
    assert_eq!(e.copy_selection().as_deref(), Some("ey"));
    assert_eq!(e.cut_selection(m).as_deref(), Some("ey"));
    assert_eq!(value(&e), "hllo");
    e.paste("ELL\nO", 1, m);
    assert_eq!(value(&e), "hELL Ollo");
    e.key(1, Data::str("a"), &["cmd"], false, m);
    e.key(1, Data::key("delete"), &[], false, m);
    assert_eq!(value(&e), "");
    e.key(1, Data::key("end"), &[], false, m);
    e.text_input(1, "done", m);
    e.take_events();
    e.key(1, Data::key("enter"), &[], false, m);
    let ev = e.take_events();
    assert_eq!(ev[0].kind, "submit");
    assert_eq!(ev[0].get("value"), Some(&Data::str("done")));
    // Keys the field doesn't use still reach Norn (to the window here).
    e.key(1, Data::key("escape"), &[], false, m);
    assert_eq!(e.take_events()[0].kind, "key");
}

#[test]
fn norn_sets_the_value() {
    let mut e = setup();
    click_at(&mut e, 200.0, &[]);
    assert_eq!(e.fields[&2].caret, 5);
    assert!(e.apply(vec![Op::Set { id: 2, props: p(&[("value", Data::str("hi"))]) }]));
    assert_eq!(value(&e), "hi");
    assert_eq!(e.fields[&2].caret, 2);
    assert!(e.apply(vec![Op::Set { id: 2, props: p(&[("value", Data::str("longer"))]) }]));
    assert_eq!(e.fields[&2].caret, 6);
    // Without focus, typing goes to Norn as a :text event.
    let mut e = setup();
    e.text_input(1, "x", &mut RoughMeasure);
    assert_eq!(value(&e), "hello");
    assert_eq!(e.take_events()[0].kind, "text");
}

#[test]
fn shift_click_and_drag_select() {
    let mut e = setup();
    let m = &mut RoughMeasure;
    click_at(&mut e, 14.0 + 6.0, &[]);
    click_at(&mut e, 14.0 + 24.0, &["shift"]);
    assert_eq!(e.fields[&2].selection(), (1, 4));
    e.pointer_down(1, 14.0, 18.0, &[], m);
    e.pointer_moved(1, 14.0 + 18.0, 18.0, m);
    e.pointer_up(1, 14.0 + 18.0, 18.0, "left", 1, &[]);
    assert_eq!(e.fields[&2].selection(), (0, 3));
}

#[test]
fn the_caret_blinks_alone_and_stops() {
    let mut e = setup();
    click_at(&mut e, 200.0, &[]);
    e.frame(1, &mut RoughMeasure);
    let caret = e.fields[&2].caret_rect;
    assert!(caret.w > 0.0 && caret.h > 0.0, "{caret:?}");
    let start = Instant::now();
    assert!(e.next_wakeup().is_some());
    e.tick(start + Duration::from_millis(600));
    assert!(!e.fields[&2].caret_on);
    // A blink repaints the caret's rectangle and nothing else.
    assert_eq!(e.frame(1, &mut RoughMeasure), vec![caret]);
    e.tick(start + Duration::from_millis(1200));
    assert!(e.fields[&2].caret_on);
    // After ten seconds without typing it stays on and the engine sleeps.
    e.tick(start + Duration::from_secs(11));
    assert!(e.fields[&2].caret_on);
    let f = e.fields.get_mut(&2).unwrap();
    f.active_at = start - Duration::from_secs(11);
    assert!(e.next_wakeup().is_none());
    // An unfocused window doesn't blink either.
    let mut e = setup();
    click_at(&mut e, 200.0, &[]);
    e.set_window_focus(1, false);
    assert!(e.next_wakeup().is_none());
    let _ = Rect::default();
}
