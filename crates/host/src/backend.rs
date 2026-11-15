//! Run the existing renderer with real windows or deterministic scripted input.

use crate::{Cmd, SHARED, STUB, deliver, execute};
use norn_ui_engine::{Data, Engine, Text};
use norn_ui_render::Renderer;
use std::sync::LazyLock;

pub(crate) static HEADLESS: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("NORN_UI_HEADLESS").is_some_and(|value| value == "1"));

fn num(xs: &[Data], i: usize) -> f32 {
    xs.get(i).and_then(Data::as_num).unwrap_or(0.0) as f32
}

fn modifiers(d: Option<&Data>) -> Vec<String> {
    match d {
        Some(Data::Set(xs)) | Some(Data::List(xs)) => xs
            .iter()
            .filter_map(|x| x.as_key().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// The headless backend's state: the engine, text, and the painter once a
/// screenshot has been asked for (until then nothing is painted).
struct Headless {
    engine: Engine,
    text: Text,
    painter: Option<(Renderer, f32)>,
}

/// Scripted input (both backends; screenshots only headless).
pub(crate) fn input(
    engine: &mut Engine,
    text: &mut Text,
    painter: &mut Option<(Renderer, f32)>,
    xs: &[Data],
) {
    if xs.first().and_then(Data::as_key) == Some("screenshot") && !*HEADLESS {
        engine.push_event(norn_ui_engine::Event::new(
            "screenshot",
            vec![
                (
                    "error",
                    Data::str("screenshots need the headless backend (NORN_UI_HEADLESS=1)"),
                ),
                ("path", xs.get(2).cloned().unwrap_or(Data::Nil)),
            ],
        ));
        return;
    }
    if xs.first().and_then(Data::as_key) == Some("screenshot") {
        let w = num(xs, 1) as u64;
        let path = xs
            .get(2)
            .and_then(Data::as_str)
            .unwrap_or("screenshot.png")
            .to_owned();
        let scale = xs.get(3).and_then(Data::as_num).unwrap_or(1.0) as f32;
        let result = (|| -> std::result::Result<(), String> {
            if painter.as_ref().is_none_or(|(_, s)| *s != scale) {
                *painter = Some((Renderer::headless()?, scale));
            }
            let damage = engine.frame(w, text);
            let (r, scale) = painter.as_mut().unwrap();
            // A new canvas paints everything; otherwise only the damage.
            r.render(
                engine,
                w,
                text,
                if damage.is_empty() {
                    &[norn_ui_engine::Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                    }]
                } else {
                    &damage
                },
                *scale,
            );
            r.save_png(w, std::path::Path::new(&path))
        })();
        engine.push_event(norn_ui_engine::Event::new(
            "screenshot",
            vec![
                ("window", Data::Num(w as f64)),
                ("path", Data::Str(path)),
                ("error", result.err().map_or(Data::Nil, Data::Str)),
            ],
        ));
        return;
    }
    // Input lands on the current layout, as a real pointer would.
    let windows: Vec<u64> = engine.windows.keys().copied().collect();
    for w in windows {
        engine.update_layout(w, text);
    }
    let e = &mut *engine;
    let w = num(xs, 1) as u64;
    match xs.first().and_then(Data::as_key) {
        Some("move") => e.pointer_moved(w, num(xs, 2), num(xs, 3), text),
        Some("down") => e.pointer_down(w, num(xs, 2), num(xs, 3), &[], text),
        Some("up") => {
            let button = xs
                .get(4)
                .and_then(Data::as_key)
                .unwrap_or("left")
                .to_owned();
            e.pointer_up(
                w,
                num(xs, 2),
                num(xs, 3),
                &button,
                num(xs, 5).max(1.0) as u32,
                &[],
            )
        }
        // [:click w x y modifiers? count?]
        Some("click") => {
            let mods = modifiers(xs.get(4));
            let mods: Vec<&str> = mods.iter().map(String::as_str).collect();
            let count = xs.get(5).and_then(Data::as_num).unwrap_or(1.0) as u32;
            e.pointer_moved(w, num(xs, 2), num(xs, 3), text);
            e.pointer_down(w, num(xs, 2), num(xs, 3), &mods, text);
            e.pointer_up(w, num(xs, 2), num(xs, 3), "left", count, &mods);
        }
        Some("click-node") => {
            if let Some(r) = e.rect(num(xs, 2) as u64) {
                let (x, y) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                e.pointer_moved(w, x, y, text);
                e.pointer_down(w, x, y, &[], text);
                e.pointer_up(w, x, y, "left", 1, &[]);
            }
        }
        Some("key") => {
            let mods = modifiers(xs.get(3));
            let mods: Vec<&str> = mods.iter().map(String::as_str).collect();
            e.key(
                w,
                xs.get(2).cloned().unwrap_or(Data::Nil),
                &mods,
                false,
                text,
            )
        }
        // An input method composing: [:preedit w text] ("" ends it).
        Some("preedit") => e.ime_preedit(xs.get(2).and_then(Data::as_str).unwrap_or(""), None),
        Some("type") => e.text_input(w, xs.get(2).and_then(Data::as_str).unwrap_or(""), text),
        // Where an editor's primary caret is: [:caret-rect w node req].
        Some("caret-rect") => {
            let r = e
                .editor_caret_rect(num(xs, 2) as u64, text)
                .unwrap_or_default();
            e.push_event(norn_ui_engine::Event::new(
                "rect",
                vec![
                    ("req", xs.get(3).cloned().unwrap_or(Data::Nil)),
                    ("x", Data::Num(r.x as f64)),
                    ("y", Data::Num(r.y as f64)),
                    ("w", Data::Num(r.w as f64)),
                    ("h", Data::Num(r.h as f64)),
                ],
            ))
        }
        Some("rect") => {
            let r = e.rect(num(xs, 2) as u64).unwrap_or_default();
            e.push_event(norn_ui_engine::Event::new(
                "rect",
                vec![
                    ("req", xs.get(3).cloned().unwrap_or(Data::Nil)),
                    ("x", Data::Num(r.x as f64)),
                    ("y", Data::Num(r.y as f64)),
                    ("w", Data::Num(r.w as f64)),
                    ("h", Data::Num(r.h as f64)),
                ],
            ))
        }
        // [:wheel w x y dx dy modifiers?]
        Some("wheel") => {
            let (x, y, dx, dy) = (num(xs, 2), num(xs, 3), num(xs, 4), num(xs, 5));
            let mods = modifiers(xs.get(6));
            let mods: Vec<&str> = mods.iter().map(String::as_str).collect();
            if !e.dispatch_wheel(w, x, y, dx, dy, &mods) {
                e.scroll_wheel(w, x, y, dx, dy, text);
            }
        }
        // [:resize w width height scale?]
        Some("resize") => e.resized(w, num(xs, 2), num(xs, 3), xs.get(4).and_then(Data::as_num).unwrap_or(1.0) as f32),
        // The window moved on screen: [:move-window w x y].
        Some("move-window") => e.moved(w, num(xs, 2), num(xs, 3)),
        // A menu command, as if chosen from the menu bar: [:command :save].
        Some("command") => e.command(xs.get(1).cloned().unwrap_or(Data::Nil)),
        // Files dropped from the OS: [:drop w x y path].
        Some("drop") => e.files_dropped(
            w,
            num(xs, 2),
            num(xs, 3),
            xs.get(4).and_then(Data::as_str).unwrap_or(""),
        ),
        // What the next file dialog answers (headless): [:dialog-answer [paths…]].
        // Copy the selection (headless: to the stand-in clipboard): [:copy w].
        Some("copy") => {
            if let Some(t) = e.copy_selection() {
                STUB.lock().unwrap().clipboard = Some(t);
            }
        }
        // What the next context menu answers (headless): [:menu-answer :cmd].
        Some("menu-answer") => STUB
            .lock()
            .unwrap()
            .menus
            .push_back(xs.get(1).cloned().unwrap_or(Data::Nil)),
        Some("dialog-answer") => STUB
            .lock()
            .unwrap()
            .dialogs
            .push_back(xs.get(1).cloned().unwrap_or(Data::Nil)),
        Some("hide") => e.set_visible(w, false),
        Some("show") => e.set_visible(w, true),
        _ => {}
    }
}

/// The headless backend's stand-ins for the platform: dialogs answer what
/// tests queued (nil once none are left), and the clipboard is in memory.
#[derive(Default)]
pub(crate) struct Stub {
    dialogs: std::collections::VecDeque<Data>,
    /// What the next context menus answer (a command, or nil: dismissed).
    menus: std::collections::VecDeque<Data>,
    clipboard: Option<String>,
    menu: Option<Data>,
}

fn stub_platform(e: &mut Engine) {
    let mut stub = STUB.lock().unwrap();
    for (op, args) in std::mem::take(&mut e.platform) {
        let req = || args.first().cloned().unwrap_or(Data::Nil);
        match op.as_str() {
            "menu" => stub.menu = Some(req()),
            "dialog" => {
                let paths = stub.dialogs.pop_front().unwrap_or(Data::Nil);
                e.push_event(norn_ui_engine::Event::new(
                    "dialog",
                    vec![("req", req()), ("paths", paths)],
                ));
            }
            "context-menu" => match stub.menus.pop_front().unwrap_or(Data::Nil) {
                Data::Nil => e.push_event(norn_ui_engine::Event::new(
                    "menu-dismissed",
                    vec![("req", req())],
                )),
                cmd => e.push_event(norn_ui_engine::Event::new(
                    "command",
                    vec![("req", req()), ("command", cmd)],
                )),
            },
            "clipboard-set" => {
                stub.clipboard = args.first().and_then(Data::as_str).map(str::to_owned)
            }
            "clipboard-get" => {
                let text = stub.clipboard.clone().map_or(Data::Nil, Data::Str);
                e.push_event(norn_ui_engine::Event::new(
                    "clipboard",
                    vec![("req", req()), ("text", text)],
                ));
            }
            _ => {}
        }
    }
}

/// The headless backend's loop, on the main thread.
pub(crate) fn headless() -> ! {
    let mut h = Headless {
        engine: Engine::new(),
        text: Text::new(),
        painter: None,
    };
    loop {
        let cmds: Vec<Cmd> = {
            let mut q = SHARED.queue.lock().unwrap();
            // Sleep until there's work or a timer (a tooltip, a blink) is due.
            while q.is_empty() {
                match h.engine.next_wakeup() {
                    Some(t) => {
                        let now = std::time::Instant::now();
                        if t <= now {
                            break;
                        }
                        let (g, r) = SHARED.wake.wait_timeout(q, t - now).unwrap();
                        q = g;
                        if r.timed_out() {
                            break;
                        }
                    }
                    None => q = SHARED.wake.wait(q).unwrap(),
                }
            }
            q.drain(..).collect()
        };
        h.engine.tick(std::time::Instant::now());
        for cmd in cmds {
            execute(cmd, &mut h.engine, &mut h.text, &mut h.painter);
        }
        stub_platform(&mut h.engine);
        let windows: Vec<u64> = h.engine.windows.keys().copied().collect();
        for w in windows {
            // Lay out, and paint the damage once there is a painter.
            let damage = h.engine.frame(w, &mut h.text);
            // NORN_UI_DAMAGE=1: what each frame repaints (for budgets).
            if !damage.is_empty() && std::env::var("NORN_UI_DAMAGE").is_ok_and(|v| v == "1") {
                let rects: Vec<String> = damage
                    .iter()
                    .map(|r| format!("{}x{}@{},{}", r.w, r.h, r.x, r.y))
                    .collect();
                eprintln!("norn-ui: window {w} repaints {}", rects.join(" "));
            }
            if let Some((r, scale)) = h.painter.as_mut() {
                r.render(&h.engine, w, &mut h.text, &damage, *scale);
            }
        }
        deliver(&mut h.engine);
    }
}
