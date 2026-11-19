//! The skein-ui renderer. The package launches it lazily and uses its
//! standard pipes for patch batches and UI events. The main thread owns
//! winit on desktop, or runs the deterministic headless backend.

mod backend;
mod patch;
mod wire;

use skein_ui_engine::raster::{DecodeRequest, RasterImage};
use skein_ui_engine::{Data, Engine, Op, Text};
use skein_ui_render::Renderer;
use std::collections::VecDeque;
use std::io::{self, BufReader};
use std::sync::{Condvar, LazyLock, Mutex, OnceLock, mpsc};
use std::thread;

enum Cmd {
    Batch(Vec<Op>),
    Input(Vec<Data>),
    Invalid(String),
    /// An image decoded on the decode pool.
    Decoded(DecodeRequest, Result<RasterImage, String>),
}

struct Shared {
    queue: Mutex<VecDeque<Cmd>>,
    wake: Condvar,
}

static SHARED: Shared = Shared {
    queue: Mutex::new(VecDeque::new()),
    wake: Condvar::new(),
};
static WAKER: OnceLock<skein_ui_desktop::Waker> = OnceLock::new();
static STUB: LazyLock<Mutex<backend::Stub>> = LazyLock::new(Mutex::default);

fn execute(cmd: Cmd, engine: &mut Engine, text: &mut Text, painter: &mut Option<(Renderer, f32)>) {
    match cmd {
        Cmd::Batch(ops) => {
            engine.apply(ops);
        }
        Cmd::Input(xs) => backend::input(engine, text, painter, &xs),
        // (counted as a batch, so the numbers stay in step with the app's)
        Cmd::Invalid(message) => engine.reject(message),
        Cmd::Decoded(request, result) => engine.decoded(request, result),
    }
}

fn push_command(cmd: Cmd) {
    SHARED.queue.lock().unwrap().push_back(cmd);
    SHARED.wake.notify_one();
    if let Some(wake) = WAKER.get() {
        wake();
    }
}

/// The desktop backend decodes images on a few threads of their own, so a
/// large image doesn't stall the window's event loop. Results come back as
/// commands, applied like any other.
fn decode_off_thread(requests: Vec<DecodeRequest>) {
    static POOL: OnceLock<Mutex<mpsc::Sender<DecodeRequest>>> = OnceLock::new();
    if requests.is_empty() {
        return;
    }
    let pool = POOL.get_or_init(|| {
        let (send, receive) = mpsc::channel::<DecodeRequest>();
        let receive = std::sync::Arc::new(Mutex::new(receive));
        let threads = thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 4));
        for i in 0..threads {
            let receive = receive.clone();
            let _ = thread::Builder::new().name(format!("skein-ui-decode-{i}")).spawn(move || {
                loop {
                    let Ok(request) = receive.lock().unwrap().recv() else { return };
                    let result = request.decode();
                    push_command(Cmd::Decoded(request, result));
                }
            });
        }
        Mutex::new(send)
    });
    let send = pool.lock().unwrap();
    for request in requests {
        let _ = send.send(request);
    }
}

fn send(data: &Data) {
    if let Err(error) = wire::send(&mut io::stdout().lock(), data) {
        if error.kind() == io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        fail(error);
    }
}

fn deliver(engine: &mut Engine) {
    for event in engine.take_events() {
        let mut fields = vec![(Data::key("event"), Data::key(event.kind))];
        fields.extend(
            event
                .fields
                .into_iter()
                .map(|(name, value)| (Data::key(name), value)),
        );
        send(&Data::Map(fields));
    }
}

/// A request, or why it can't be read. What can't be read is reported as a
/// rejected batch: the app sends well-formed frames, so in practice it is a
/// batch with an op the renderer doesn't take.
fn parse(data: Data) -> Result<Cmd, String> {
    let Data::List(mut request) = data else {
        return Err("A UI request is a vector".into());
    };
    let Some(payload) = request.pop() else {
        return Err("A UI request needs a payload".into());
    };
    let Data::List(items) = payload else {
        return Err("A UI payload is a vector".into());
    };
    match request.as_slice() {
        [Data::Key(kind)] if kind == "apply" => {
            let ops = items
                .iter()
                .map(patch::parse_op)
                .collect::<Result<_, _>>()?;
            Ok(Cmd::Batch(ops))
        }
        [Data::Key(kind)] if kind == "simulate" => Ok(Cmd::Input(items)),
        _ => Err("Unknown UI request".into()),
    }
}

fn read_requests() {
    let mut input = BufReader::new(io::stdin().lock());
    loop {
        match wire::read_frame(&mut input) {
            Ok(Some(frame)) => {
                let cmd = wire::decode(&frame)
                    .map_err(|e| e.to_string())
                    .and_then(parse)
                    .unwrap_or_else(Cmd::Invalid);
                push_command(cmd);
            }
            // The app exited and closed its pipe. This also works during
            // a dialog, when the main event loop may be in a modal call.
            Ok(None) => std::process::exit(0),
            // The app exited partway through writing a frame (it can, as
            // it quits): it is gone all the same.
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => std::process::exit(0),
            Err(error) => fail(error),
        }
    }
}

fn ready() {
    send(&Data::List(vec![Data::key("ready"), Data::Num(1.0)]));
}

fn fail(error: impl std::fmt::Display) -> ! {
    eprintln!("skein-ui-host: {error}");
    std::process::exit(1);
}

fn main() {
    if let Err(error) = thread::Builder::new()
        .name("skein-ui-input".into())
        .spawn(read_requests)
    {
        fail(error);
    }
    if *backend::HEADLESS {
        ready();
        backend::headless();
    }

    if let Err(error) = skein_ui_desktop::run(
        |waker| {
            let _ = WAKER.set(waker);
            ready();
        },
        |engine, text| {
            engine.set_async_decode(true);
            let commands: Vec<Cmd> = SHARED.queue.lock().unwrap().drain(..).collect();
            let mut no_painter = None;
            for cmd in commands {
                execute(cmd, engine, text, &mut no_painter);
            }
            decode_off_thread(engine.take_decode_requests());
        },
        deliver,
        || send(&Data::List(vec![Data::key("quit")])),
    ) {
        fail(error);
    }
}
