# norn-ui

This checkout, `norn-ui2`, provides the `norn-ui` Norn package and the
`norn.ui` API from the original toolkit. Applications run on the stock
`norn` executable. The toolkit provides the native renderer for every app
that uses this checkout.

## Use it in an app

Point the existing dependency name at this checkout in the app's `norn.edn`.

```clojure
:deps {norn-ui {:path "../norn-ui2"}}
```

Keep using `(require '[norn.ui :as ui])` and the same window, widget and
event APIs. Run the app script with the current Norn release.

```sh
../norn-lang/target/release/norn app.norn
```

For Mandelbrot, change the dependency path in `madelbrot/norn.edn` and use
`../norn-lang/target/release/norn mandelbrot.norn` from the app directory.
Its viewer already calls `ui/wait-closed` to keep the app alive until its
windows close.

## Renderer installation

The Apple Silicon distribution includes `bin/macos-aarch64/norn-ui-host`.
From a source checkout, build the renderer once in this directory.

```sh
cargo build --release --locked -p norn-ui-host
```

The package looks for a renderer set by `NORN_UI_HOST` first, then the
local release build, then the bundled renderer for the current OS and
architecture. Set `NORN_UI_HOST` to an absolute path when using a custom
Cargo target directory or restoring an image whose original toolkit
directory has moved. Apps share the installation and launch their own
renderer process when they first use the UI.

## Architecture

`lib/norn/ui.norn` owns views, state, patch generation and event handlers.
Its `norn.ui.native` bridge uses stock Norn's `norn.process`, `norn.io`
and `norn.bin` to send patch batches and receive semantic events over
standard pipes. There are no listening sockets. The renderer owns the
winit event loop on its own main thread and uses the original layout,
widgets and wgpu painting code. It exits when the app closes its pipe.
A saved Norn image starts a fresh renderer and reconstructs its windows
when restored.

## Tests

From this directory, run the original UI suite against stock Norn.

```sh
NORN_UI_HEADLESS=1 ../norn-lang/target/release/norn test --serial tests
```

Headless mode supports layout, simulated input and event assertions.
PNG screenshots require a wgpu adapter. With `NORN_UI_HEADLESS` unset,
the same package opens desktop windows.
