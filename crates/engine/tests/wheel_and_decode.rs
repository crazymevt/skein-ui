//! Wheel handlers next to built-in scrolling, and image decoding off the
//! engine's thread.

use skein_ui_engine::raster::DecodeRequest;
use skein_ui_engine::{Data, Engine, Op, Props, RoughMeasure};

fn k(s: &str) -> Data {
    Data::key(s)
}
fn n(x: f64) -> Data {
    Data::Num(x)
}
fn props(kvs: &[(&str, Data)]) -> Props {
    kvs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

/// A 200×300 window whose root (1) wants :wheel and holds a 100-high scroll
/// container (2) of ten 30-high rows.
fn handler_around_a_scroller() -> Engine {
    let mut e = Engine::new();
    let mut ops = vec![
        Op::Create { id: 1, kind: "box".into(), props: props(&[("on", Data::Set(vec![k("wheel")]))]) },
        Op::Style { id: 1, style: props(&[("flex-direction", k("column"))]) },
        Op::Create { id: 2, kind: "scroll".into(), props: vec![] },
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
    e.take_events();
    e
}

fn wheel_events(e: &mut Engine) -> Vec<(Option<Data>, Option<Data>)> {
    e.take_events()
        .into_iter()
        .filter(|ev| ev.kind == "wheel")
        .map(|ev| (ev.get("node").cloned(), ev.get("dy").cloned()))
        .collect()
}

#[test]
fn an_inner_scroller_scrolls_before_an_outer_handler_hears_the_wheel() {
    let mut e = handler_around_a_scroller();
    e.wheel(1, 50.0, 50.0, 0.0, 45.0, &mut RoughMeasure);
    assert_eq!(e.scroll[&2], (0.0, 45.0));
    assert!(wheel_events(&mut e).is_empty(), "the scroller took it");
    e.wheel(1, 50.0, 50.0, 0.0, 10_000.0, &mut RoughMeasure);
    assert_eq!(e.scroll[&2], (0.0, 200.0));
    assert!(wheel_events(&mut e).is_empty());
    // At its end, the scroller can't move: the handler around it hears it.
    e.wheel(1, 50.0, 50.0, 0.0, 30.0, &mut RoughMeasure);
    assert_eq!(e.scroll[&2], (0.0, 200.0));
    assert_eq!(wheel_events(&mut e), vec![(Some(n(1.0)), Some(n(30.0)))]);
    // Below the scroller, the handler is the first taker.
    e.wheel(1, 50.0, 200.0, 0.0, -5.0, &mut RoughMeasure);
    assert_eq!(wheel_events(&mut e), vec![(Some(n(1.0)), Some(n(-5.0)))]);
}

#[test]
fn queued_wheel_events_for_one_node_combine() {
    let mut e = handler_around_a_scroller();
    for dy in [10.0, 20.0, 5.0] {
        assert!(e.dispatch_wheel(1, 50.0, 200.0, 0.0, dy, &[]));
    }
    assert_eq!(wheel_events(&mut e), vec![(Some(n(1.0)), Some(n(35.0)))]);
    // Different modifiers keep events apart.
    assert!(e.dispatch_wheel(1, 50.0, 200.0, 0.0, 1.0, &[]));
    assert!(e.dispatch_wheel(1, 50.0, 200.0, 0.0, 2.0, &["shift"]));
    assert_eq!(wheel_events(&mut e).len(), 2);
}

#[test]
fn a_wheel_on_a_window_without_layout_is_ignored() {
    let mut e = handler_around_a_scroller();
    assert!(!e.dispatch_wheel(99, 50.0, 50.0, 0.0, 10.0, &[]), "no such window");
}

fn ppm(width: u32, height: u32) -> Data {
    let mut bytes = format!("P6\n{width} {height}\n255\n").into_bytes();
    bytes.extend(std::iter::repeat_n([40u8, 100, 170], (width * height) as usize).flatten());
    Data::Bytes(bytes.into())
}

fn run(e: &mut Engine, request: DecodeRequest) {
    let result = request.decode();
    e.decoded(request, result);
}

#[test]
fn images_decode_off_the_engine_thread_and_stale_results_are_dropped() {
    let mut e = Engine::new();
    e.set_async_decode(true);
    assert!(e.apply(vec![Op::Create { id: 1, kind: "image".into(), props: props(&[("src", ppm(3, 2))]) }]));
    assert!(e.nodes[&1].raster.is_none(), "not decoded while applying the batch");
    let first = e.take_decode_requests();
    assert_eq!(first.len(), 1);
    // The source changes before the first decode finishes.
    assert!(e.apply(vec![Op::Set { id: 1, props: props(&[("src", ppm(2, 4))]) }]));
    let second = e.take_decode_requests();
    assert_eq!(second.len(), 1);
    for request in second {
        run(&mut e, request);
    }
    assert_eq!(e.nodes[&1].raster.as_ref().map(|image| image.size), Some([2, 4]));
    for request in first {
        run(&mut e, request);
    }
    assert_eq!(e.nodes[&1].raster.as_ref().map(|image| image.size), Some([2, 4]), "the stale result is ignored");
    // A nil source clears the image once its (failed) decode comes back.
    assert!(e.apply(vec![Op::Set { id: 1, props: props(&[("src", Data::Nil)]) }]));
    for request in e.take_decode_requests() {
        run(&mut e, request);
    }
    assert!(e.nodes[&1].raster.is_none());
    // A decode-image request is answered once its result comes back.
    assert!(e.apply(vec![Op::DecodeImage { req: 7, src: ppm(1, 1), size: None }]));
    assert!(e.take_events().iter().all(|ev| ev.kind != "image-decoded"));
    for request in e.take_decode_requests() {
        run(&mut e, request);
    }
    let answer = e.take_events().into_iter().find(|ev| ev.kind == "image-decoded").unwrap();
    assert_eq!(answer.get("req"), Some(&n(7.0)));
    assert_eq!(answer.get("size"), Some(&Data::List(vec![n(1.0), n(1.0)])));
}

#[test]
fn without_async_decode_images_are_ready_after_the_batch() {
    let mut e = Engine::new();
    assert!(e.apply(vec![Op::Create { id: 1, kind: "image".into(), props: props(&[("src", ppm(3, 2))]) }]));
    assert_eq!(e.nodes[&1].raster.as_ref().map(|image| image.size), Some([3, 2]));
    assert!(e.take_decode_requests().is_empty());
}
