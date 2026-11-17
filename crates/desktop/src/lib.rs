//! The desktop backend (plan 2.6): winit windows with wgpu surfaces, input,
//! and the idle rules (design §9.1). macOS first; winit and wgpu make Linux
//! and Windows mostly configuration.
//!
//! The backend owns the event loop on the process's first thread and knows
//! nothing of Skein: `run` takes a `pull` callback that applies queued work
//! (patch batches, scripted input) to the engine, and a `deliver` callback
//! that sends the engine's events on. The host wakes the loop with the
//! waker it is given.
//!
//! **Idle rules:** the loop waits (no render loop). It draws only when the
//! engine reports damage, and only that damage is repainted into the
//! window's canvas; the canvas is then copied to the screen. Occluded or
//! minimised windows draw nothing. The low-power GPU is preferred.

use skein_ui_engine::{Data, Engine, Text};
use skein_ui_render::Renderer;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId as OsId};

/// Wakes the event loop from any thread.
pub type Waker = Box<dyn Fn() + Send + Sync>;

struct OsWindow {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    scale: f32,
    cursor: (f32, f32),
    title: String,
    /// A frame is drawn on the canvas but couldn't be shown yet (the
    /// window wasn't on screen): show it at the next redraw.
    unshown: bool,
    /// A popup's position in its owner (logical), as last placed.
    position: Option<(f32, f32)>,
}

struct App<P, D> {
    engine: Engine,
    text: Text,
    instance: wgpu::Instance,
    adapter: Option<wgpu::Adapter>,
    renderer: Option<Renderer>,
    windows: HashMap<u64, OsWindow>,
    by_os: HashMap<OsId, u64>,
    modifiers: ModifiersState,
    last_click: Option<(Instant, (f32, f32), u32)>,
    pull: P,
    deliver: D,
    on_exit: fn(),
    error: Option<String>,
    clipboard: Option<arboard::Clipboard>,
    /// The app's menu bar (kept alive while shown).
    menu: Option<muda::Menu>,
    /// Menu items chosen, from muda's handler thread.
    commands: Arc<std::sync::Mutex<Vec<String>>>,
    /// A context menu was answered with an item whose command hasn't
    /// arrived yet: the request it answers.
    context_req: Option<Data>,
    /// Where the IME candidate window was last placed.
    ime_area: Option<(u64, skein_ui_engine::Rect)>,
    /// Wheel and trackpad scrolling since the last step, by window:
    /// (x, y, dx, dy), the deltas summed. A trackpad sends far more events
    /// than there are frames; applied one by one, each laid out and drew
    /// the window, and the backlog kept it scrolling seconds after.
    wheel: HashMap<u64, (f32, f32, f32, f32)>,
    /// Events came: step once the batch is in (about_to_wait).
    needs_step: bool,
    /// Big wheel steps still to scroll, by window: applied over a few
    /// frames rather than at once.
    glide: HashMap<u64, Glide>,
}

/// A wheel step being scrolled smoothly: at (x, y), (dx, dy) still to go,
/// last moved at `at`.
struct Glide {
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
    at: Instant,
}

/// A single wheel event bigger than this (px) glides.
const GLIDE_OVER: f32 = 40.0;
/// The glide's time constant (s): about 95% there after three of these.
const GLIDE_TAU: f32 = 0.05;

/// Menu items from the protocol's menu tree: [label command shortcut?],
/// [label [items…]] for a submenu, or :separator.
fn menu_items(items: &[Data]) -> Vec<Box<dyn muda::IsMenuItem>> {
    let mut out: Vec<Box<dyn muda::IsMenuItem>> = Vec::new();
    for it in items {
        if it.as_key() == Some("separator") {
            out.push(Box::new(muda::PredefinedMenuItem::separator()));
            continue;
        }
        let Some(xs) = it.as_list() else { continue };
        let label = xs.first().and_then(Data::as_str).unwrap_or("");
        match xs.get(1) {
            Some(Data::List(sub)) => {
                let kids = menu_items(sub);
                let refs: Vec<&dyn muda::IsMenuItem> = kids.iter().map(|b| b.as_ref()).collect();
                if let Ok(m) = muda::Submenu::with_items(label, true, &refs) {
                    out.push(Box::new(m));
                }
            }
            Some(Data::Key(cmd)) => {
                let accel = xs
                    .get(2)
                    .and_then(Data::as_str)
                    .and_then(|k| k.replace("cmd", "CmdOrCtrl").parse::<muda::accelerator::Accelerator>().ok());
                match cmd.as_str() {
                    "quit" => out.push(Box::new(muda::PredefinedMenuItem::quit(Some(label)))),
                    "hide" => out.push(Box::new(muda::PredefinedMenuItem::hide(Some(label)))),
                    _ => out.push(Box::new(muda::MenuItem::with_id(cmd.as_str(), label, true, accel))),
                }
            }
            _ => {}
        }
    }
    out
}

/// SKEIN_UI_FRAMES=1: report steps, event handling and waits for a surface
/// slower than a frame, and what took the time.
fn frames_trace() -> bool {
    static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *D.get_or_init(|| std::env::var("SKEIN_UI_FRAMES").is_ok_and(|v| v == "1"))
}

fn debug() -> bool {
    static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *D.get_or_init(|| std::env::var("SKEIN_UI_DEBUG").is_ok_and(|v| v == "1"))
}

fn named(k: &NamedKey) -> String {
    // ArrowLeft → arrow-left
    let mut out = String::new();
    for (i, c) in format!("{k:?}").chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// A tiny executor: wgpu's native futures resolve without a reactor.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker as TaskWaker};
    fn noop(_: *const ()) {}
    fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: the vtable's functions do nothing with the (null) data pointer.
    let waker = unsafe { TaskWaker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

impl<P: FnMut(&mut Engine, &mut Text), D: FnMut(&mut Engine)> App<P, D> {
    fn mods(&self) -> Vec<&'static str> {
        let m = self.modifiers;
        let mut out = Vec::new();
        if m.shift_key() {
            out.push("shift");
        }
        if m.control_key() {
            out.push("ctrl");
        }
        if m.alt_key() {
            out.push("alt");
        }
        if m.super_key() {
            out.push("cmd");
        }
        out
    }

    /// Open, close and retitle OS windows to match the engine's.
    fn sync_windows(&mut self, el: &ActiveEventLoop) {
        let gone: Vec<u64> = self.windows.keys().filter(|w| !self.engine.windows.contains_key(w)).copied().collect();
        for w in gone {
            if let Some(os) = self.windows.remove(&w) {
                self.by_os.remove(&os.window.id());
            }
        }
        let ids: Vec<u64> = self.engine.windows.keys().copied().collect();
        for w in ids {
            let title = self.engine.windows[&w]
                .props
                .iter()
                .find(|(k, _)| k == "title")
                .and_then(|(_, v)| v.as_str())
                .unwrap_or("Skein")
                .to_owned();
            let place = self.popup_place(w);
            if let Some(os) = self.windows.get_mut(&w) {
                if os.title != title {
                    os.window.set_title(&title);
                    os.title = title;
                }
                if let Some((at, p)) = place
                    && os.position != Some(p)
                {
                    os.window.set_outer_position(at);
                    os.position = Some(p);
                }
                continue;
            }
            if let Err(e) = self.open(el, w, title) {
                eprintln!("skein-ui: {e}");
                self.error = Some(e);
            }
        }
    }

    /// Where popup w goes on screen: its owner's content origin plus its
    /// :position (logical), and that position.
    fn popup_place(&self, w: u64) -> Option<(winit::dpi::PhysicalPosition<i32>, (f32, f32))> {
        let owner = self.engine.owner_of(w)?;
        let os = self.windows.get(&owner)?;
        let props = &self.engine.windows.get(&w)?.props;
        let pos = props.iter().find(|(k, _)| k == "position").and_then(|(_, v)| v.as_list()).unwrap_or(&[]);
        let (x, y) = (pos.first().and_then(Data::as_num).unwrap_or(0.0) as f32, pos.get(1).and_then(Data::as_num).unwrap_or(0.0) as f32);
        let origin = os.window.inner_position().ok()?;
        Some((winit::dpi::PhysicalPosition::new(origin.x + (x * os.scale) as i32, origin.y + (y * os.scale) as i32), (x, y)))
    }

    /// Keep the input method's candidate window at the focused caret.
    fn place_ime(&mut self) {
        let area = self.engine.ime_area(&mut self.text);
        if area == self.ime_area {
            return;
        }
        self.ime_area = area;
        if let Some((w, r)) = area
            && let Some(os) = self.windows.get(&w)
        {
            os.window.set_ime_cursor_area(winit::dpi::LogicalPosition::new(r.x as f64, r.y as f64), LogicalSize::new(r.w.max(1.0) as f64, r.h.max(1.0) as f64));
        }
    }

    /// Windows the engine sized to fit their content: resize the OS ones.
    fn fit_windows(&mut self) {
        for (w, fw, fh) in std::mem::take(&mut self.engine.fitted) {
            if let Some(os) = self.windows.get(&w) {
                let _ = os.window.request_inner_size(LogicalSize::new(fw.max(1.0) as f64, fh.max(1.0) as f64));
            }
        }
    }

    /// Show a context menu at (x, y) in window w (it blocks until it
    /// closes); the choice arrives as a menu event.
    fn context_menu(&mut self, req: Data, w: u64, x: f32, y: f32, tree: &[Data]) {
        let items = menu_items(tree);
        let refs: Vec<&dyn muda::IsMenuItem> = items.iter().map(|b| b.as_ref()).collect();
        let (Ok(menu), Some(os)) = (muda::Menu::with_items(&refs), self.windows.get(&w)) else {
            self.engine.push_event(skein_ui_engine::Event::new("menu-dismissed", vec![("req", req)]));
            return;
        };
        let chosen = show_context_menu(&menu, &os.window, x, y);
        if !chosen {
            self.engine.push_event(skein_ui_engine::Event::new("menu-dismissed", vec![("req", req)]));
            return;
        }
        // The item's menu event may already be queued, or come next.
        let first = { let mut c = self.commands.lock().unwrap(); (!c.is_empty()).then(|| c.remove(0)) };
        match first {
            Some(cmd) => self.engine.push_event(skein_ui_engine::Event::new("command", vec![("req", req), ("command", Data::key(&cmd))])),
            None => self.context_req = Some(req),
        }
    }

    fn open(&mut self, el: &ActiveEventLoop, w: u64, title: String) -> Result<(), String> {
        let (lw, lh) = self.engine.windows[&w].size;
        let mut attrs = WindowAttributes::default().with_title(&title).with_inner_size(LogicalSize::new(lw.max(1.0) as f64, lh.max(1.0) as f64));
        let place = self.popup_place(w);
        // A top-level window's :position is where it goes on screen.
        if place.is_none()
            && let Some(Data::List(p)) = self.engine.windows[&w].props.iter().find(|(k, _)| k == "position").map(|(_, v)| v)
            && let (Some(x), Some(y)) = (p.first().and_then(Data::as_num), p.get(1).and_then(Data::as_num))
        {
            attrs = attrs.with_position(winit::dpi::LogicalPosition::new(x, y));
        }
        if let Some((at, _)) = place {
            // A popup: borderless, above its owner, not taking focus.
            attrs = attrs
                .with_decorations(false)
                .with_resizable(false)
                .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
                .with_active(false)
                .with_position(at);
        }
        let window = Arc::new(el.create_window(attrs).map_err(|e| format!("Can't open a window: {e}"))?);
        window.set_ime_allowed(true);
        let surface = self.instance.create_surface(window.clone()).map_err(|e| format!("No GPU surface: {e}"))?;
        if self.renderer.is_none() {
            let adapter = block_on(self.instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                ..Default::default()
            }))
            .map_err(|e| format!("No GPU adapter: {e}"))?;
            self.renderer = Some(Renderer::with_adapter(&adapter)?);
            self.adapter = Some(adapter);
        }
        let r = self.renderer.as_ref().unwrap();
        let caps = surface.get_capabilities(self.adapter.as_ref().unwrap());
        // The canvas holds sRGB-encoded values already: present them as they are.
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            color_space: Default::default(),
        };
        surface.configure(&r.device, &config);
        let scale = window.scale_factor() as f32;
        self.by_os.insert(window.id(), w);
        self.windows.insert(w, OsWindow { window, surface, config, scale, cursor: (0.0, 0.0), title, unshown: false, position: place.map(|(_, p)| p) });
        self.engine.resized(w, size.width as f32 / scale, size.height as f32 / scale, scale);
        Ok(())
    }

    /// Menu commands chosen since the last step: text-field editing ones
    /// act on the focused field; all go to Skein as :command.
    fn commands(&mut self) {
        let chosen: Vec<String> = std::mem::take(&mut *self.commands.lock().unwrap());
        for cmd in chosen {
            // The menu's Copy takes selected rich text too.
            if cmd == "copy"
                && self.engine.focused_field().is_none()
                && let (Some(t), Some(cb)) = (self.engine.rich_selection_text(), self.clipboard.as_mut())
            {
                let _ = cb.set_text(t);
            }
            if let Some(req) = self.context_req.take() {
                self.engine.push_event(skein_ui_engine::Event::new("command", vec![("req", req), ("command", Data::key(&cmd))]));
                continue;
            }
            if self.engine.focused_field().is_some() {
                match cmd.as_str() {
                    "copy" => {
                        if let (Some(t), Some(cb)) = (self.engine.copy_selection(), self.clipboard.as_mut()) {
                            let _ = cb.set_text(t);
                        }
                    }
                    "cut" => {
                        if let Some(t) = self.engine.cut_selection(&mut self.text)
                            && let Some(cb) = self.clipboard.as_mut()
                        {
                            let _ = cb.set_text(t);
                        }
                    }
                    "paste" => {
                        if let (Some(t), Some(w)) = (self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()), self.windows.keys().next().copied()) {
                            self.engine.paste(&t, w, &mut self.text);
                        }
                    }
                    "select-all" => {
                        let mods = ["cmd"];
                        self.engine.field_key(&Data::str("a"), &mods, &mut self.text);
                    }
                    _ => {}
                }
            }
            self.engine.command(Data::key(&cmd));
        }
    }

    /// Menus, dialogs and the clipboard, as Skein asked.
    fn platform(&mut self) {
        for (op, args) in std::mem::take(&mut self.engine.platform) {
            match op.as_str() {
                "menu" => {
                    let items = menu_items(args.first().and_then(Data::as_list).unwrap_or(&[]));
                    let refs: Vec<&dyn muda::IsMenuItem> = items.iter().map(|b| b.as_ref()).collect();
                    if let Ok(m) = muda::Menu::with_items(&refs) {
                        #[cfg(target_os = "macos")]
                        m.init_for_nsapp();
                        self.menu = Some(m);
                    }
                }
                "context-menu" => {
                    // [:context-menu req tree window x y]
                    let req = args.first().cloned().unwrap_or(Data::Nil);
                    let tree = args.get(1).and_then(Data::as_list).map(<[Data]>::to_vec).unwrap_or_default();
                    let num = |i: usize| args.get(i).and_then(Data::as_num).unwrap_or(0.0);
                    self.context_menu(req, num(2) as u64, num(3) as f32, num(4) as f32, &tree);
                }
                "dialog" => {
                    let req = args.first().cloned().unwrap_or(Data::Nil);
                    let spec = args.get(1).cloned().unwrap_or(Data::Nil);
                    let mut d = rfd::FileDialog::new();
                    if let Some(t) = spec.get("title").and_then(Data::as_str) {
                        d = d.set_title(t);
                    }
                    if let Some(dir) = spec.get("directory").and_then(Data::as_str) {
                        d = d.set_directory(dir);
                    }
                    if let Some(Data::List(fs)) = spec.get("filters") {
                        for f in fs {
                            if let Some([name, Data::List(exts), ..]) = f.as_list() {
                                let exts: Vec<String> = exts.iter().filter_map(|e| e.as_str().map(str::to_owned)).collect();
                                d = d.add_filter(name.as_str().unwrap_or(""), &exts);
                            }
                        }
                    }
                    let paths: Option<Vec<std::path::PathBuf>> = match spec.get("kind").and_then(Data::as_key).unwrap_or("open-file") {
                        "open-files" => d.pick_files(),
                        "folder" => d.pick_folder().map(|p| vec![p]),
                        "save-file" => d.save_file().map(|p| vec![p]),
                        _ => d.pick_file().map(|p| vec![p]),
                    };
                    let paths = paths.map_or(Data::Nil, |ps| Data::List(ps.iter().map(|p| Data::Str(p.to_string_lossy().into_owned())).collect()));
                    self.engine.push_event(skein_ui_engine::Event::new("dialog", vec![("req", req), ("paths", paths)]));
                }
                "clipboard-set" => {
                    if let (Some(t), Some(cb)) = (args.first().and_then(Data::as_str), self.clipboard.as_mut()) {
                        let _ = cb.set_text(t.to_owned());
                    }
                }
                "clipboard-get" => {
                    let text = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()).map_or(Data::Nil, Data::Str);
                    self.engine.push_event(skein_ui_engine::Event::new("clipboard", vec![("req", args.first().cloned().unwrap_or(Data::Nil)), ("text", text)]));
                }
                _ => {}
            }
        }
    }

    /// Draw what changed and show it, then send events on.
    fn step(&mut self, el: &ActiveEventLoop) {
        let t0 = Instant::now();
        (self.pull)(&mut self.engine, &mut self.text);
        let pulled = t0.elapsed();
        let mut times = Vec::new();
        self.commands();
        self.platform();
        self.fit_windows();
        self.place_ime();
        self.sync_windows(el);
        let ids: Vec<u64> = self.windows.keys().copied().collect();
        for w in ids {
            let t1 = Instant::now();
            let damage = self.engine.frame(w, &mut self.text);
            let laid = t1.elapsed();
            if damage.is_empty() {
                times.push((w, laid, Duration::ZERO));
                continue;
            }
            let t2 = Instant::now();
            let scale = self.windows[&w].scale;
            if debug() {
                eprintln!("skein-ui: window {w}: repaint {damage:?} at scale {scale}");
            }
            if let Some(r) = self.renderer.as_mut() {
                r.render(&self.engine, w, &mut self.text, &damage, scale);
            }
            self.present(w);
            times.push((w, laid, t2.elapsed()));
        }
        (self.deliver)(&mut self.engine);
        if frames_trace() && t0.elapsed() > Duration::from_millis(12) {
            let ws: Vec<String> = times.iter().map(|(w, l, d)| format!("window {w}: layout {:.1} ms, draw {:.1} ms", l.as_secs_f64() * 1e3, d.as_secs_f64() * 1e3)).collect();
            eprintln!("skein-ui: slow step {:.1} ms (ops {:.1} ms; {})", t0.elapsed().as_secs_f64() * 1e3, pulled.as_secs_f64() * 1e3, ws.join("; "));
        }
    }

    fn present(&mut self, w: u64) {
        let (Some(r), Some(os)) = (self.renderer.as_mut(), self.windows.get_mut(&w)) else { return };
        let t0 = Instant::now();
        let got = os.surface.get_current_texture();
        if frames_trace() && t0.elapsed() > Duration::from_millis(20) {
            eprintln!(
                "skein-ui: window {w}: waited {:.1} ms for a surface ({}, visible {:?})",
                t0.elapsed().as_secs_f64() * 1e3,
                match &got {
                    wgpu::CurrentSurfaceTexture::Success(_) => "ok",
                    wgpu::CurrentSurfaceTexture::Suboptimal(_) => "suboptimal",
                    wgpu::CurrentSurfaceTexture::Timeout => "timeout",
                    wgpu::CurrentSurfaceTexture::Occluded => "occluded",
                    _ => "other",
                },
                os.window.is_visible()
            );
        }
        if debug() {
            eprintln!("skein-ui: window {w}: present ({:?}, {}×{} {:?})", std::mem::discriminant(&got), os.config.width, os.config.height, os.config.format);
        }
        let frame = match got {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            // Not on screen yet, or timed out: show it when the OS next
            // asks for a redraw. Outdated: reconfigure and do the same.
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                os.unshown = true;
                return;
            }
            _ => {
                os.surface.configure(&r.device, &os.config);
                os.unshown = true;
                return;
            }
        };
        os.unshown = false;
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        os.window.pre_present_notify();
        r.present(w, &view, os.config.format);
        r.queue.present(frame);
    }

    fn key(&mut self, w: u64, ev: &KeyEvent) {
        if ev.state != ElementState::Pressed {
            return;
        }
        let key = match &ev.logical_key {
            Key::Character(s) => Data::Str(s.to_string()),
            Key::Named(n) => Data::Key(named(n)),
            _ => return,
        };
        let mods = self.mods();
        // Copy, cut and paste for text fields, with the system clipboard.
        let shortcut = if cfg!(target_os = "macos") { self.modifiers.super_key() } else { self.modifiers.control_key() };
        // Copy also takes selected rich text when no field has focus.
        if shortcut && self.engine.focused_field().is_none() && key.as_str() == Some("c") && self.engine.rich_selection_text().is_some() {
            if let (Some(t), Some(cb)) = (self.engine.copy_selection(), self.clipboard.as_mut()) {
                let _ = cb.set_text(t);
            }
            return;
        }
        if shortcut && self.engine.focused_field().is_some() {
            match key.as_str() {
                Some("c") => {
                    if let (Some(t), Some(cb)) = (self.engine.copy_selection(), self.clipboard.as_mut()) {
                        let _ = cb.set_text(t);
                    }
                    return;
                }
                Some("x") => {
                    if let Some(t) = self.engine.cut_selection(&mut self.text)
                        && let Some(cb) = self.clipboard.as_mut()
                    {
                        let _ = cb.set_text(t);
                    }
                    return;
                }
                Some("v") => {
                    if let Some(t) = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
                        self.engine.paste(&t, w, &mut self.text);
                    }
                    return;
                }
                _ => {}
            }
        }
        self.engine.key(w, key, &mods, ev.repeat, &mut self.text);
        // Typed text (not shortcuts) also arrives as text.
        if let Some(t) = &ev.text
            && !self.modifiers.control_key()
            && !self.modifiers.super_key()
            && !t.chars().all(char::is_control)
        {
            let t = t.to_string();
            self.text_input(w, &t);
        }
    }

    fn text_input(&mut self, w: u64, t: &str) {
        self.engine.text_input(w, t, &mut self.text);
    }
}

impl<P: FnMut(&mut Engine, &mut Text), D: FnMut(&mut Engine)> ApplicationHandler<()> for App<P, D> {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        self.step(el);
    }

    /// The event loop is ending. Skein closes windows itself, so if some are
    /// still open, the system asked the app to quit: say so, as otherwise
    /// it looks like the program just stopped.
    fn exiting(&mut self, _: &ActiveEventLoop) {
        if !self.engine.windows.is_empty() {
            eprintln!(
                "skein-ui: quitting because the system asked the app to quit ({} window(s) open)",
                self.engine.windows.len()
            );
        }
        if self.error.is_none() {
            (self.on_exit)();
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, _: ()) {
        self.step(el);
    }

    /// A frame that couldn't be shown is retried on a short timer (never a
    /// spin); with nothing pending the loop just waits.
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // The batch of events is in: scroll by what the wheel summed to,
        // then draw once.
        for (w, (x, y, dx, dy)) in std::mem::take(&mut self.wheel) {
            self.engine.scroll_wheel(w, x, y, dx, dy, &mut self.text);
        }
        // Gliding: each frame takes a share of what is left, more the
        // longer since the last (an ease-out over about 150 ms).
        let now = Instant::now();
        for (w, g) in self.glide.iter_mut() {
            let dt = now.duration_since(g.at).as_secs_f32();
            let share = (1.0 - (-dt / GLIDE_TAU).exp()).max(0.2);
            let (mut sx, mut sy) = (g.dx * share, g.dy * share);
            if (g.dx - sx).abs() < 0.5 && (g.dy - sy).abs() < 0.5 {
                (sx, sy) = (g.dx, g.dy);
            }
            g.dx -= sx;
            g.dy -= sy;
            g.at = now;
            self.engine.scroll_wheel(*w, g.x, g.y, sx, sy, &mut self.text);
            self.needs_step = true;
        }
        self.glide.retain(|_, g| g.dx != 0.0 || g.dy != 0.0);
        if std::mem::take(&mut self.needs_step) {
            self.step(el);
        }
        // A blink, a tooltip or a transition due: do it now (events arriving
        // all the time, as the pointer moves, mustn't hold it back).
        if self.engine.next_wakeup().is_some_and(|t| t <= Instant::now()) {
            self.engine.tick(Instant::now());
            self.step(el);
        }
        // Wake for the caret blink (only while it blinks) or to show a frame
        // the OS wasn't ready for; otherwise just wait.
        let retry = self.windows.values().any(|o| o.unshown).then(|| Instant::now() + Duration::from_millis(50));
        let gliding = (!self.glide.is_empty()).then(|| Instant::now() + Duration::from_millis(8));
        match [retry, gliding, self.engine.next_wakeup()].into_iter().flatten().min() {
            Some(t) => el.set_control_flow(ControlFlow::WaitUntil(t)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }

    fn new_events(&mut self, el: &ActiveEventLoop, cause: winit::event::StartCause) {
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            let pending: Vec<u64> = self.windows.iter().filter(|(_, o)| o.unshown).map(|(w, _)| *w).collect();
            for w in pending {
                self.present(w);
            }
            self.engine.tick(Instant::now());
            self.step(el);
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: OsId, event: WindowEvent) {
        if !frames_trace() {
            return self.window_event_now(el, id, event);
        }
        let name = format!("{event:?}").chars().take(60).collect::<String>();
        let t = Instant::now();
        self.window_event_now(el, id, event);
        if t.elapsed() > Duration::from_millis(20) {
            eprintln!("skein-ui: handling {name} took {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
        }
    }
}

impl<P: FnMut(&mut Engine, &mut Text), D: FnMut(&mut Engine)> App<P, D> {
    fn window_event_now(&mut self, el: &ActiveEventLoop, id: OsId, event: WindowEvent) {
        let Some(&w) = self.by_os.get(&id) else { return };
        match event {
            WindowEvent::CloseRequested => self.engine.close_requested(w),
            WindowEvent::Moved(p) => {
                if let Some(os) = self.windows.get(&w)
                    && self.engine.owner_of(w).is_none()
                {
                    let s = os.scale.max(0.1);
                    self.engine.moved(w, p.x as f32 / s, p.y as f32 / s);
                }
            }
            WindowEvent::Resized(PhysicalSize { width, height }) => {
                if let (Some(r), Some(os)) = (self.renderer.as_ref(), self.windows.get_mut(&w)) {
                    os.config.width = width.max(1);
                    os.config.height = height.max(1);
                    os.surface.configure(&r.device, &os.config);
                    let s = os.scale;
                    self.engine.resized(w, width as f32 / s, height as f32 / s, s);
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(os) = self.windows.get_mut(&w) {
                    os.scale = scale_factor as f32;
                }
            }
            WindowEvent::Occluded(hidden) => {
                if debug() {
                    eprintln!("skein-ui: window {w}: occluded {hidden}");
                }
                self.engine.set_visible(w, !hidden);
                if !hidden && self.windows.get(&w).is_some_and(|o| o.unshown) {
                    self.present(w);
                }
            }
            WindowEvent::Focused(f) => self.engine.set_window_focus(w, f),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(os) = self.windows.get_mut(&w) {
                    os.cursor = (position.x as f32 / os.scale, position.y as f32 / os.scale);
                    let (x, y) = os.cursor;
                    self.engine.pointer_moved(w, x, y, &mut self.text);
                    use winit::window::CursorIcon as C;
                    let icon = match self.engine.cursor() {
                        "pointer" => C::Pointer,
                        "text" => C::Text,
                        "col-resize" => C::ColResize,
                        "row-resize" => C::RowResize,
                        "grab" => C::Grab,
                        "move" => C::Move,
                        "not-allowed" => C::NotAllowed,
                        _ => C::Default,
                    };
                    if let Some(os) = self.windows.get(&w) {
                        os.window.set_cursor(icon);
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => self.engine.pointer_moved(w, -1.0, -1.0, &mut self.text),
            WindowEvent::MouseInput { state, button, .. } => {
                let (x, y) = self.windows.get(&w).map_or((0.0, 0.0), |o| o.cursor);
                let name = match button {
                    MouseButton::Left => "left",
                    MouseButton::Right => "right",
                    MouseButton::Middle => "middle",
                    _ => return,
                };
                match state {
                    ElementState::Pressed => {
                        let mods = self.mods();
                        self.engine.pointer_down(w, x, y, &mods, &mut self.text)
                    }
                    ElementState::Released => {
                        let now = Instant::now();
                        let count = match self.last_click {
                            Some((t, (lx, ly), n)) if now - t < Duration::from_millis(400) && (lx - x).abs() < 4.0 && (ly - y).abs() < 4.0 => n + 1,
                            _ => 1,
                        };
                        self.last_click = Some((now, (x, y), count));
                        let mods = self.mods();
                        self.engine.pointer_up(w, x, y, name, count, &mods);
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y) = self.windows.get(&w).map_or((0.0, 0.0), |o| o.cursor);
                let scale = self.windows.get(&w).map_or(1.0, |o| o.scale);
                // Lines are three rows of 16 px; pixel deltas come from
                // trackpads, and from mice (macOS turns a wheel's clicks
                // into pixels, accelerated).
                let (dx, dy, line) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(lx, ly) => (-lx * 48.0, -ly * 48.0, true),
                    winit::event::MouseScrollDelta::PixelDelta(p) => (-p.x as f32 / scale, -p.y as f32 / scale, false),
                };
                // Deliver application input at its original pointer position,
                // before the smoothing used for default scrolling.
                let mods = self.mods();
                if self.engine.dispatch_wheel(w, x, y, dx, dy, &mods) {
                    self.needs_step = true;
                    return;
                }
                // A big step (a wheel's click, a line) glides there rather
                // than jumping; a trackpad's small ones are already smooth.
                if line || dx.abs().max(dy.abs()) > GLIDE_OVER {
                    let g = self.glide.entry(w).or_insert(Glide { x, y, dx: 0.0, dy: 0.0, at: Instant::now() });
                    (g.x, g.y, g.dx, g.dy) = (x, y, g.dx + dx, g.dy + dy);
                    self.needs_step = true;
                    return;
                }
                let e = self.wheel.entry(w).or_insert((x, y, 0.0, 0.0));
                *e = (x, y, e.2 + dx, e.3 + dy);
                // applied, and drawn, once the batch is in
                self.needs_step = true;
                return;
            }
            WindowEvent::KeyboardInput { event, .. } => self.key(w, &event),
            WindowEvent::DroppedFile(path) => {
                let (x, y) = self.windows.get(&w).map_or((0.0, 0.0), |o| o.cursor);
                self.engine.files_dropped(w, x, y, &path.to_string_lossy());
            }
            WindowEvent::Ime(Ime::Preedit(t, cursor)) => self.engine.ime_preedit(&t, cursor.map(|(b, _)| b)),
            WindowEvent::Ime(Ime::Commit(t)) => {
                self.engine.ime_preedit("", None);
                self.text_input(w, &t)
            }
            // The OS wants the window shown again: the canvas already holds
            // the frame.
            WindowEvent::RedrawRequested => self.present(w),
            _ => {}
        }
        self.step(el);
    }
}

/// Run the desktop event loop on this thread (the process's first). `start`
/// receives the waker to hand to whoever queues work. `on_exit` runs when
/// the system ends the loop normally, including macOS application termination.
pub fn run<P, D>(start: impl FnOnce(Waker), pull: P, deliver: D, on_exit: fn()) -> Result<(), String>
where
    P: FnMut(&mut Engine, &mut Text),
    D: FnMut(&mut Engine),
{
    let event_loop = EventLoop::<()>::with_user_event().build().map_err(|e| format!("No event loop: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let menu_proxy = event_loop.create_proxy();
    start(Box::new(move || {
        let _ = proxy.send_event(());
    }));
    let commands: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let chosen = commands.clone();
    muda::MenuEvent::set_event_handler(Some(move |e: muda::MenuEvent| {
        chosen.lock().unwrap().push(e.id.0.clone());
        let _ = menu_proxy.send_event(());
    }));
    let mut app = App {
        engine: Engine::new(),
        text: Text::new(),
        instance: wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        adapter: None,
        renderer: None,
        windows: HashMap::new(),
        by_os: HashMap::new(),
        modifiers: ModifiersState::default(),
        last_click: None,
        pull,
        deliver,
        on_exit,
        error: None,
        clipboard: arboard::Clipboard::new().ok(),
        menu: None,
        commands,
        context_req: None,
        ime_area: None,
        wheel: HashMap::new(),
        needs_step: false,
        glide: HashMap::new(),
    };
    event_loop.run_app(&mut app).map_err(|e| format!("The event loop failed: {e}"))?;
    match app.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Pop up `menu` at (x, y) (logical) in `window`; true if an item was chosen.
#[cfg(target_os = "macos")]
fn show_context_menu(menu: &muda::Menu, window: &Window, x: f32, y: f32) -> bool {
    use muda::ContextMenu;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(h) = window.window_handle() else { return false };
    let RawWindowHandle::AppKit(h) = h.as_raw() else { return false };
    // muda measures y from the view's bottom (`height - y`), as for an
    // unflipped NSView, but winit's view is flipped (y from the top): so
    // give it y from the bottom, and the two flips land where the click was.
    let height = window.inner_size().to_logical::<f64>(window.scale_factor()).height;
    let at = muda::dpi::Position::Logical(muda::dpi::LogicalPosition::new(x as f64, height - y as f64));
    // SAFETY: the view belongs to a live window on this (the main) thread.
    unsafe { menu.show_context_menu_for_nsview(h.ns_view.as_ptr() as *const _, Some(at)) }
}

#[cfg(not(target_os = "macos"))]
fn show_context_menu(_: &muda::Menu, _: &Window, _: f32, _: f32) -> bool {
    false
}
