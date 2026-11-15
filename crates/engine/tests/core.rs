//! The engine core without a GPU: batches, layout, hit-testing, states,
//! damage and counters.

use norn_ui_engine::{Data, Engine, Op, Props, Rect, RoughMeasure};

fn k(s: &str) -> Data {
    Data::key(s)
}
fn n(x: f64) -> Data {
    Data::Num(x)
}
fn props(kvs: &[(&str, Data)]) -> Props {
    kvs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

/// A window 400×300 with a column: a title text and a row of two buttons.
fn sample() -> Engine {
    let mut e = Engine::new();
    let ok = e.apply(vec![
        Op::Hello(1),
        Op::Create { id: 1, kind: "box".into(), props: vec![] },
        Op::Style { id: 1, style: props(&[("flex-direction", k("column")), ("padding", n(10.0)), ("gap", n(8.0))]) },
        Op::Create { id: 2, kind: "text".into(), props: props(&[("text", Data::str("Hello"))]) },
        Op::Create { id: 3, kind: "box".into(), props: vec![] },
        Op::Style { id: 3, style: props(&[("flex-direction", k("row")), ("gap", n(4.0))]) },
        Op::Create { id: 4, kind: "button".into(), props: props(&[("on", Data::Set(vec![k("click")]))]) },
        Op::Style { id: 4, style: props(&[("width", n(100.0)), ("height", n(30.0))]) },
        Op::Create { id: 5, kind: "button".into(), props: vec![] },
        Op::Style { id: 5, style: props(&[("width", n(50.0)), ("height", n(30.0))]) },
        Op::Insert { parent: 1, index: 0, id: 2 },
        Op::Insert { parent: 1, index: 1, id: 3 },
        Op::Insert { parent: 3, index: 0, id: 4 },
        Op::Insert { parent: 3, index: 1, id: 5 },
        Op::Window { id: 1, props: props(&[("root", n(1.0)), ("size", Data::List(vec![n(400.0), n(300.0)]))]) },
    ]);
    assert!(ok, "{:?}", e.take_events());
    e
}

#[test]
fn hello_and_layout() {
    let mut e = sample();
    let ev = e.take_events();
    assert_eq!(ev[0].kind, "hello");
    assert_eq!(ev[0].get("version"), Some(&n(1.0)));
    let damage = e.frame(1, &mut RoughMeasure);
    assert_eq!(damage, vec![Rect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 }]);
    assert_eq!(e.rect(1), Some(Rect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 }));
    // "Hello": one line of 1.25 × 14 = 17.5, rounded to whole pixels.
    assert_eq!(e.rect(2), Some(Rect { x: 10.0, y: 10.0, w: 380.0, h: 18.0 }));
    assert_eq!(e.rect(4), Some(Rect { x: 10.0, y: 36.0, w: 100.0, h: 30.0 }));
    assert_eq!(e.rect(5), Some(Rect { x: 114.0, y: 36.0, w: 50.0, h: 30.0 }));
}

#[test]
fn a_bad_op_rejects_the_whole_batch() {
    let mut e = sample();
    e.take_events();
    let ok = e.apply(vec![
        Op::Text { id: 2, text: "changed".into() },
        Op::Insert { parent: 1, index: 9, id: 5 },
    ]);
    assert!(!ok);
    let ev = e.take_events();
    assert_eq!(ev[0].kind, "error");
    assert!(ev[0].get("message").unwrap().as_str().unwrap().contains("past node 1"));
    assert_eq!(e.nodes[&2].text(), "Hello");
    for bad in [
        vec![Op::Create { id: 1, kind: "box".into(), props: vec![] }],
        vec![Op::Create { id: 9, kind: "widget".into(), props: vec![] }],
        vec![Op::Style { id: 4, style: props(&[("width", Data::str("wide"))]) }],
        vec![Op::Remove(4), Op::Text { id: 4, text: "x".into() }],
        vec![Op::Theme(Data::Map(vec![(k("rules"), n(3.0))]))],
    ] {
        assert!(!e.apply(bad.clone()), "{bad:?}");
    }
}

#[test]
fn idle_draws_nothing_and_a_change_repaints_only_its_node() {
    let mut e = sample();
    e.frame(1, &mut RoughMeasure);
    for _ in 0..100 {
        assert!(e.frame(1, &mut RoughMeasure).is_empty());
    }
    // A paint-only change repaints just that node.
    e.apply(vec![Op::Style { id: 5, style: props(&[("background", Data::str("#ff0000"))]) }]);
    assert_eq!(e.frame(1, &mut RoughMeasure), vec![Rect { x: 114.0, y: 36.0, w: 50.0, h: 30.0 }]);
    assert!(e.frame(1, &mut RoughMeasure).is_empty());
    e.apply(vec![Op::Stats(7)]);
    let s = e.take_events().pop().unwrap();
    assert_eq!(s.get("frames"), Some(&n(2.0)));
    assert_eq!(s.get("layouts"), Some(&n(1.0)));
    assert_eq!(s.get("area"), Some(&n(400.0 * 300.0 + 50.0 * 30.0)));
}

#[test]
fn a_layout_change_repaints_what_moved() {
    let mut e = sample();
    e.frame(1, &mut RoughMeasure);
    e.apply(vec![Op::Style { id: 4, style: props(&[("width", n(120.0))]) }]);
    let damage = e.frame(1, &mut RoughMeasure);
    // Button 4 grew, so button 5 moved: both, old and new places, merged.
    let covered: f32 = damage.iter().map(Rect::area).sum();
    assert!(covered <= 120.0 * 30.0 + 50.0 * 30.0 * 2.0 + 1.0, "{damage:?}");
    assert_eq!(e.rect(5), Some(Rect { x: 134.0, y: 36.0, w: 50.0, h: 30.0 }));
}

#[test]
fn clicks_go_to_the_nearest_node_that_wants_them() {
    let mut e = sample();
    e.frame(1, &mut RoughMeasure);
    e.take_events();
    assert_eq!(e.hit(1, 20.0, 40.0), Some(4));
    assert_eq!(e.hit(1, 300.0, 200.0), Some(1));
    e.pointer_moved(1, 20.0, 40.0, &mut RoughMeasure); // hover: no events
    assert!(e.take_events().is_empty());
    e.pointer_down(1, 20.0, 40.0, &[], &mut RoughMeasure);
    e.pointer_up(1, 21.0, 41.0, "left", 1, &["shift"]);
    let ev = e.take_events();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].kind, "click");
    assert_eq!(ev[0].get("node"), Some(&n(4.0)));
    assert_eq!(ev[0].get("x"), Some(&n(11.0)));
    // Button 5 doesn't ask for clicks, and neither do its ancestors.
    e.pointer_down(1, 120.0, 40.0, &[], &mut RoughMeasure);
    e.pointer_up(1, 120.0, 40.0, "left", 1, &[]);
    assert!(e.take_events().is_empty());
    // Pressing on one node and releasing on another isn't a click.
    e.pointer_down(1, 20.0, 40.0, &[], &mut RoughMeasure);
    e.pointer_up(1, 120.0, 40.0, "left", 1, &[]);
    assert!(e.take_events().is_empty());
}

#[test]
fn themes_style_by_kind_class_and_state() {
    let mut e = sample();
    let theme = Data::Map(vec![
        (k("tokens"), Data::Map(vec![(k("accent"), Data::str("#4f8cff"))])),
        (
            k("rules"),
            Data::List(vec![
                Data::List(vec![Data::List(vec![k("button"), k("*"), k("*")]), Data::Map(vec![(k("background"), k("accent"))])]),
                Data::List(vec![Data::List(vec![k("button"), k("*"), k("hover")]), Data::Map(vec![(k("background"), Data::str("#ffffff"))])]),
                Data::List(vec![Data::List(vec![k("*"), k("big"), k("*")]), Data::Map(vec![(k("height"), n(60.0))])]),
            ]),
        ),
    ]);
    assert!(e.apply(vec![Op::Theme(theme), Op::Set { id: 5, props: props(&[("class", k("big"))]) }]));
    let bg = |e: &Engine, id| e.resolved_style(id).into_iter().find(|(k, _)| k == "background").map(|(_, v)| v);
    assert_eq!(bg(&e, 4), Some(k("accent")));
    assert_eq!(e.theme.token(&k("accent")), &Data::str("#4f8cff"));
    e.frame(1, &mut RoughMeasure);
    e.pointer_moved(1, 20.0, 40.0, &mut RoughMeasure);
    assert_eq!(bg(&e, 4), Some(Data::str("#ffffff")));
    // Hover is a repaint of that node only, no layout.
    let before = e.stats().layouts;
    let damage = e.frame(1, &mut RoughMeasure);
    assert_eq!(damage, vec![Rect { x: 10.0, y: 36.0, w: 100.0, h: 30.0 }]);
    assert_eq!(e.stats().layouts, before);
    // A class rule changed the layout: button 5's own style says 30 high,
    // which wins over the theme's 60.
    assert_eq!(e.rect(5).unwrap().h, 30.0);
}

#[test]
fn removing_destroys_the_subtree_and_hidden_windows_draw_nothing() {
    let mut e = sample();
    e.frame(1, &mut RoughMeasure);
    assert!(e.apply(vec![Op::Remove(3)]));
    assert!(!e.nodes.contains_key(&4) && !e.nodes.contains_key(&5));
    assert!(!e.frame(1, &mut RoughMeasure).is_empty());
    e.set_visible(1, false);
    e.apply(vec![Op::Text { id: 2, text: "Hidden change".into() }]);
    assert!(e.frame(1, &mut RoughMeasure).is_empty());
    e.set_visible(1, true);
    assert!(!e.frame(1, &mut RoughMeasure).is_empty());
}

#[test]
fn keys_go_to_the_focused_node_or_the_window() {
    let mut e = sample();
    e.apply(vec![Op::Set { id: 4, props: props(&[("on", Data::Set(vec![k("click"), k("key")]))]) }, Op::Focus(4)]);
    e.take_events();
    e.key(1, Data::str("a"), &["cmd"], false, &mut RoughMeasure);
    let ev = e.take_events();
    assert_eq!(ev[0].get("node"), Some(&n(4.0)));
    e.apply(vec![Op::Focus(5)]);
    e.key(1, k("enter"), &[], false, &mut RoughMeasure);
    assert_eq!(e.take_events()[0].get("node"), None);
}

#[test]
fn text_properties_inherit_and_hover_reaches_ancestors() {
    let mut e = sample();
    let theme = Data::Map(vec![(
        k("rules"),
        Data::List(vec![
            Data::List(vec![Data::List(vec![k("button"), k("*"), k("*")]), Data::Map(vec![(k("color"), Data::str("#ffffff")), (k("background"), Data::str("#3070e0"))])]),
            Data::List(vec![Data::List(vec![k("button"), k("*"), k("hover")]), Data::Map(vec![(k("background"), Data::str("#2058c0"))])]),
        ]),
    )]);
    assert!(e.apply(vec![
        Op::Theme(theme),
        Op::Create { id: 6, kind: "text".into(), props: props(&[("text", Data::str("OK"))]) },
        Op::Insert { parent: 4, index: 0, id: 6 },
    ]));
    let color = |e: &Engine, id| e.nodes[&id].resolved.iter().find(|(k, _)| k == "color").map(|(_, v)| v.clone());
    let bg = |e: &Engine, id| e.nodes[&id].resolved.iter().find(|(k, _)| k == "background").map(|(_, v)| v.clone());
    assert_eq!(color(&e, 6), Some(Data::str("#ffffff")));
    assert_eq!(bg(&e, 6), None); // background isn't inherited
    e.frame(1, &mut RoughMeasure);
    // The pointer over the label hovers the button too; only the button's
    // style changes, so only it is repainted.
    let label = e.rect(6).unwrap();
    e.pointer_moved(1, label.x + 1.0, label.y + 1.0, &mut RoughMeasure);
    assert_eq!(e.hovered.map(|(_, n)| n), Some(6));
    assert_eq!(bg(&e, 4), Some(Data::str("#2058c0")));
    assert_eq!(e.frame(1, &mut RoughMeasure), vec![e.rect(4).unwrap()]);
    // Changing an inherited property on the parent restyles the child.
    e.apply(vec![Op::Style { id: 4, style: props(&[("color", Data::str("#000000"))]) }]);
    assert_eq!(color(&e, 6), Some(Data::str("#000000")));
}

#[test]
fn a_batch_is_checked_against_the_tree_as_it_changes() {
    // Node 3 has two children (4, 5). Removing one leaves one: inserting at
    // 2 is past the end, and the batch is refused (it used to pass the
    // check, then panic).
    let mut e = sample();
    e.take_events();
    let bad = e.apply(vec![
        Op::Remove(5),
        Op::Create { id: 9, kind: "text".into(), props: vec![] },
        Op::Insert { parent: 3, index: 2, id: 9 },
    ]);
    assert!(!bad);
    assert_eq!(e.take_events()[0].kind, "error");
    assert_eq!(e.nodes[&3].children, vec![4, 5], "nothing was applied");
    // Inserting a node already under the parent moves it: 4 to the end is
    // index 1 (it leaves its place first), not 2.
    assert!(e.apply(vec![Op::Insert { parent: 3, index: 1, id: 4 }]));
    assert_eq!(e.nodes[&3].children, vec![5, 4]);
    assert!(!e.apply(vec![Op::Insert { parent: 3, index: 2, id: 4 }]));
    // And a node moved to another parent leaves a gap where it was.
    assert!(!e.apply(vec![Op::Insert { parent: 1, index: 0, id: 5 }, Op::Insert { parent: 3, index: 2, id: 2 }]));
    assert!(e.apply(vec![Op::Insert { parent: 1, index: 0, id: 5 }, Op::Insert { parent: 3, index: 1, id: 2 }]));
    assert_eq!(e.nodes[&3].children, vec![4, 2]);
}

#[test]
fn hover_passes_inherited_styles_down_and_back() {
    let mut e = sample();
    let theme = Data::Map(vec![(
        k("rules"),
        Data::List(vec![
            Data::List(vec![Data::List(vec![k("button"), k("*"), k("*")]), Data::Map(vec![(k("color"), Data::str("#ffffff"))])]),
            Data::List(vec![Data::List(vec![k("button"), k("*"), k("hover")]), Data::Map(vec![(k("color"), Data::str("#ff0000"))])]),
        ]),
    )]);
    assert!(e.apply(vec![
        Op::Theme(theme),
        Op::Create { id: 6, kind: "box".into(), props: vec![] },
        Op::Insert { parent: 4, index: 0, id: 6 },
        Op::Create { id: 7, kind: "text".into(), props: props(&[("text", Data::str("OK"))]) },
        Op::Insert { parent: 6, index: 0, id: 7 },
    ]));
    let color = |e: &Engine, id| e.nodes[&id].resolved.iter().find(|(k, _)| k == "color").map(|(_, v)| v.clone());
    e.frame(1, &mut RoughMeasure);
    let label = e.rect(7).unwrap();
    e.pointer_moved(1, label.x + 1.0, label.y + 1.0, &mut RoughMeasure);
    assert_eq!(color(&e, 7), Some(Data::str("#ff0000")), "the hover colour reaches the grandchild");
    e.pointer_moved(1, 1000.0, 1000.0, &mut RoughMeasure);
    assert_eq!(color(&e, 7), Some(Data::str("#ffffff")), "and leaves with the pointer");
}

#[test]
fn a_transitions_next_frame_stays_put_as_events_come() {
    // Asked after every event (the pointer moves every few milliseconds),
    // "a frame from now" would never come, and the transition would stop.
    let mut e = sample();
    let theme = Data::Map(vec![(
        k("rules"),
        Data::List(vec![
            Data::List(vec![
                Data::List(vec![k("button"), k("*"), k("*")]),
                Data::Map(vec![(k("background"), Data::str("#3070e0")), (k("transition"), Data::List(vec![k("background"), n(120.0)]))]),
            ]),
            Data::List(vec![Data::List(vec![k("button"), k("*"), k("hover")]), Data::Map(vec![(k("background"), Data::str("#2058c0"))])]),
        ]),
    )]);
    assert!(e.apply(vec![Op::Theme(theme)]));
    e.frame(1, &mut RoughMeasure);
    let b = e.rect(4).unwrap();
    e.pointer_moved(1, b.x + 1.0, b.y + 1.0, &mut RoughMeasure);
    e.tick(std::time::Instant::now());
    let first = e.next_wakeup().expect("a transition runs");
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert_eq!(e.next_wakeup(), Some(first));
}

#[test]
fn hovering_in_a_big_tree_restyles_little() {
    // A column of 40 sections × 50 lines, 4 deep: moving the pointer from
    // line to line used to restyle every ancestor's whole subtree.
    let mut e = Engine::new();
    let mut ops = vec![
        Op::Create { id: 1, kind: "box".into(), props: vec![] },
        Op::Style { id: 1, style: props(&[("flex-direction", k("column"))]) },
        Op::Window { id: 1, props: vec![("root".into(), n(1.0)), ("size".into(), Data::List(vec![n(800.0), n(600.0)]))] },
    ];
    let mut id = 2u64;
    for s in 0..40 {
        let sec = id;
        id += 1;
        ops.push(Op::Create { id: sec, kind: "box".into(), props: vec![] });
        ops.push(Op::Style { id: sec, style: props(&[("flex-direction", k("column"))]) });
        ops.push(Op::Insert { parent: 1, index: s, id: sec });
        for l in 0..50 {
            ops.push(Op::Create { id, kind: "text".into(), props: props(&[("text", Data::str("a line of text"))]) });
            ops.push(Op::Insert { parent: sec, index: l, id });
            id += 1;
        }
    }
    assert!(e.apply(ops));
    e.frame(1, &mut RoughMeasure);
    let t = std::time::Instant::now();
    for i in 0..200 {
        e.pointer_moved(1, 10.0, (i % 30) as f32 * 17.0 + 2.0, &mut RoughMeasure);
    }
    assert!(t.elapsed() < std::time::Duration::from_millis(500), "200 moves took {:?}", t.elapsed());
}

#[test]
fn nil_removes_a_style_property() {
    let mut e = sample();
    e.frame(1, &mut RoughMeasure);
    assert!(e.apply(vec![Op::Style { id: 4, style: props(&[("width", Data::Nil)]) }]));
    e.frame(1, &mut RoughMeasure);
    // Without its width the button shrinks to its (empty) content.
    assert_eq!(e.rect(4).unwrap().w, 0.0);
}

#[test]
fn scroll_containers_clip_and_scroll_without_layout() {
    let mut e = Engine::new();
    let mut ops = vec![
        Op::Create { id: 1, kind: "box".into(), props: vec![] },
        Op::Style { id: 1, style: props(&[("flex-direction", k("column"))]) },
        Op::Create { id: 2, kind: "scroll".into(), props: props(&[("on", Data::Set(vec![k("scroll")]))]) },
        Op::Style { id: 2, style: props(&[("height", n(100.0))]) },
        Op::Insert { parent: 1, index: 0, id: 2 },
    ];
    for i in 0..10u64 {
        ops.push(Op::Create { id: 10 + i, kind: "box".into(), props: vec![] });
        ops.push(Op::Style { id: 10 + i, style: props(&[("height", n(30.0))]) });
        ops.push(Op::Insert { parent: 2, index: i as usize, id: 10 + i });
    }
    ops.push(Op::Window { id: 1, props: props(&[("root", n(1.0)), ("size", Data::List(vec![n(200.0), n(300.0)]))]) });
    assert!(e.apply(ops));
    e.frame(1, &mut RoughMeasure);
    // Ten rows of 30 in a 100-high container: the fourth row is clipped.
    assert_eq!(e.rect(13).unwrap().y, 90.0);
    assert_eq!(e.windows[&1].clips[&13], Rect { x: 0.0, y: 0.0, w: 200.0, h: 100.0 });
    assert_eq!(e.hit(1, 50.0, 95.0), Some(13));
    assert_eq!(e.hit(1, 50.0, 105.0), Some(1)); // below the container: not row 14
    let layouts = e.stats().layouts;
    e.take_events();
    e.wheel(1, 50.0, 50.0, 0.0, 45.0, &mut RoughMeasure);
    assert_eq!(e.rect(13).unwrap().y, 45.0);
    let ev = e.take_events();
    assert_eq!(ev[0].kind, "scroll");
    assert_eq!(ev[0].get("y"), Some(&n(45.0)));
    // Only the container repaints, and nothing is laid out again.
    assert_eq!(e.frame(1, &mut RoughMeasure), vec![Rect { x: 0.0, y: 0.0, w: 200.0, h: 100.0 }]);
    assert_eq!(e.stats().layouts, layouts);
    // Scrolling stops at the end: 300 of content in 100.
    e.wheel(1, 50.0, 50.0, 0.0, 10_000.0, &mut RoughMeasure);
    assert_eq!(e.scroll[&2], (0.0, 200.0));
    e.wheel(1, 50.0, 50.0, 0.0, -10_000.0, &mut RoughMeasure);
    assert_eq!(e.scroll[&2], (0.0, 0.0));
    // Outside any scroll container the wheel does nothing.
    e.wheel(1, 50.0, 200.0, 0.0, 10.0, &mut RoughMeasure);
    assert_eq!(e.scroll[&2], (0.0, 0.0));
}

#[test]
fn drags_report_movement_after_a_threshold() {
    let mut e = sample();
    e.apply(vec![Op::Set { id: 5, props: props(&[("on", Data::Set(vec![k("drag")])), ]) },
                 Op::Style { id: 5, style: props(&[("cursor", k("col-resize"))]) }]);
    e.frame(1, &mut RoughMeasure);
    e.take_events();
    let m = &mut RoughMeasure;
    e.pointer_moved(1, 120.0, 40.0, m);
    assert_eq!(e.cursor(), "col-resize");
    e.pointer_down(1, 120.0, 40.0, &[], m);
    e.pointer_moved(1, 121.0, 40.0, m); // under the threshold
    assert!(e.take_events().is_empty());
    e.pointer_moved(1, 130.0, 42.0, m);
    e.pointer_moved(1, 135.0, 42.0, m);
    let ev = e.take_events();
    assert_eq!(ev[0].kind, "drag-start");
    // The two moves are one :drag event with their sum.
    assert_eq!(ev.len(), 2);
    assert_eq!(ev[1].get("dx"), Some(&n(15.0)));
    assert_eq!(ev[1].get("dy"), Some(&n(2.0)));
    e.pointer_up(1, 135.0, 42.0, "left", 1, &[]);
    let ev = e.take_events();
    assert_eq!(ev.iter().map(|x| x.kind).collect::<Vec<_>>(), ["drag-end"]);
}

#[test]
fn setting_selected_restyles_at_once() {
    let mut e = sample();
    let theme = Data::Map(vec![(
        k("rules"),
        Data::List(vec![Data::List(vec![Data::List(vec![k("button"), k("*"), k("selected")]), Data::Map(vec![(k("background"), Data::str("#dbe6fb"))])])]),
    )]);
    assert!(e.apply(vec![Op::Theme(theme)]));
    e.frame(1, &mut RoughMeasure);
    let bg = |e: &Engine| e.nodes[&5].resolved.iter().find(|(k, _)| k == "background").map(|(_, v)| v.clone());
    assert_eq!(bg(&e), None);
    // No pointer involved: the prop alone must restyle and repaint the node.
    assert!(e.apply(vec![Op::Set { id: 5, props: props(&[("selected", Data::Bool(true))]) }]));
    assert_eq!(bg(&e), Some(Data::str("#dbe6fb")));
    assert_eq!(e.frame(1, &mut RoughMeasure), vec![e.rect(5).unwrap()]);
    assert!(e.apply(vec![Op::Set { id: 5, props: props(&[("selected", Data::Bool(false))]) }]));
    assert_eq!(bg(&e), None);
}

#[test]
fn the_wheel_passes_over_a_box_that_only_clips() {
    // overflow :hidden clips, as in CSS, but isn't scrolled by the user:
    // the wheel over it went to it, and moved a whole layout.
    let mut e = Engine::new();
    assert!(e.apply(vec![
        Op::Create { id: 1, kind: "box".into(), props: vec![] },
        Op::Style { id: 1, style: props(&[("flex-direction", k("column")), ("height", n(100.0)), ("overflow", k("hidden"))]) },
        Op::Create { id: 2, kind: "box".into(), props: vec![] },
        Op::Style { id: 2, style: props(&[("height", n(300.0)), ("flex-shrink", n(0.0))]) },
        Op::Insert { parent: 1, index: 0, id: 2 },
        Op::Window { id: 1, props: props(&[("root", n(1.0)), ("size", Data::List(vec![n(200.0), n(100.0)]))]) },
    ]));
    e.frame(1, &mut RoughMeasure);
    e.wheel(1, 50.0, 50.0, 0.0, 40.0, &mut RoughMeasure);
    assert!(e.scroll.get(&1).is_none_or(|s| *s == (0.0, 0.0)), "{:?}", e.scroll.get(&1));
}

#[test]
fn pre_text_does_not_wrap() {
    let mut e = Engine::new();
    let long = "a b c d e f g h i j k l m n o p q r s t u v w x y z";
    assert!(e.apply(vec![
        Op::Create { id: 1, kind: "box".into(), props: vec![] },
        Op::Style { id: 1, style: props(&[("flex-direction", k("column")), ("width", n(60.0))]) },
        Op::Create { id: 2, kind: "text".into(), props: props(&[("text", Data::str(long))]) },
        Op::Create { id: 3, kind: "text".into(), props: props(&[("text", Data::str(long))]) },
        Op::Style { id: 3, style: props(&[("white-space", k("pre"))]) },
        Op::Insert { parent: 1, index: 0, id: 2 },
        Op::Insert { parent: 1, index: 1, id: 3 },
        Op::Window { id: 1, props: props(&[("root", n(1.0)), ("size", Data::List(vec![n(300.0), n(300.0)]))]) },
    ]));
    e.frame(1, &mut RoughMeasure);
    let (wrapped, pre) = (e.windows[&1].rects[&2], e.windows[&1].rects[&3]);
    assert!(wrapped.h > pre.h, "the plain text wraps onto more lines ({wrapped:?} {pre:?})");
}

/// A 200×100 window that's a scroll container holding 1000 pixels.
fn scroller() -> Engine {
    let mut e = Engine::new();
    assert!(e.apply(vec![
        Op::Create { id: 1, kind: "scroll".into(), props: vec![] },
        Op::Style { id: 1, style: props(&[("flex-direction", k("column")), ("height", n(100.0)), ("width", n(200.0))]) },
        Op::Create { id: 2, kind: "box".into(), props: vec![] },
        Op::Style { id: 2, style: props(&[("height", n(1000.0)), ("flex-shrink", n(0.0))]) },
        Op::Insert { parent: 1, index: 0, id: 2 },
        Op::Window { id: 1, props: props(&[("root", n(1.0)), ("size", Data::List(vec![n(200.0), n(100.0)]))]) },
    ]));
    e.frame(1, &mut RoughMeasure);
    e
}

#[test]
fn a_scroll_bar_thumb_drags_and_its_track_jumps() {
    let mut e = scroller();
    let t = e.scroll_thumbs(&e.windows[&1], 1);
    assert_eq!(t.len(), 1, "a vertical thumb");
    let (tx, ty) = (t[0].x + 3.0, t[0].y + 5.0);
    // Drag the thumb halfway down its track: the view moves halfway.
    e.pointer_down(1, tx, ty, &[], &mut RoughMeasure);
    let room = 100.0 - t[0].h;
    e.pointer_moved(1, tx, ty + room / 2.0, &mut RoughMeasure);
    e.pointer_up(1, tx, ty + room / 2.0, "left", 1, &[]);
    let (_, sy) = e.scroll[&1];
    assert!((sy - 450.0).abs() < 2.0, "halfway: {sy}");
    // Pressing the track at the bottom jumps to the end.
    e.pointer_down(1, tx, 99.0, &[], &mut RoughMeasure);
    e.pointer_up(1, tx, 99.0, "left", 1, &[]);
    assert!((e.scroll[&1].1 - 900.0).abs() < 1.0, "the end: {:?}", e.scroll[&1]);
}
