# norn-ui

This checkout provides the `norn-ui` Norn package and the
`norn.ui` API from the original toolkit. Applications run on the stock
`norn` executable. The toolkit provides the native renderer for every app
that uses this checkout.

## Use it in an app

Point the existing dependency name at this checkout in the app's `norn.edn`.

```clojure
:deps {norn-ui {:path "../norn-ui"}}
```

Keep using `(require '[norn.ui :as ui])` and the same window, widget and
event APIs. Run the app script with the current Norn release.

```sh
../norn-lang/target/release/norn app.norn
```

For Mandelbrot, change the dependency path in `mandelbrot/norn.edn` and use
`../norn-lang/target/release/norn mandelbrot.norn` from the app directory.
Its viewer already calls `ui/wait-closed` to keep the app alive until its
windows close.

## Images

```clojure
[:image {:src "photo.jpg" :width 320 :height 240 :fit :cover}]
```

`:src` accepts a local path or the encoded contents of an image as Norn
bytes. PNG, JPEG, BMP, TIFF, ICO, and Netpbm (PBM, PGM, PPM, PAM) are
supported. Images use their intrinsic size when no dimensions are set.
With a specified box, `:contain` (the default) fits the whole image,
`:cover` fills the box with a centered crop, and `:fill` stretches to fit.
Images use smooth bilinear sampling by default. Set `:sampling :nearest`
to preserve hard pixel edges.
An invalid source draws an empty image. On the desktop, images decode on
background threads, so a large image doesn't stall the window. An image takes
its size once its pixels are ready, and a changed `:src` keeps showing the old
image until then.

Use `ui/decode-image` to work with the pixels directly. It accepts the
same inputs, reports decode errors, and works in headless mode without a
GPU. Encoded input and RGBA8 output are each capped at 64 MiB, with a
maximum of 8192 pixels per axis. JPEG EXIF orientation is applied.
Pass `{:size [width height]}` as a second argument to resample the
decoded pixels to those exact dimensions, with the same output limits.

```clojure
(let [{:keys [size rgba]} (ui/decode-image "photo.jpg")]
  [:canvas {:width 320 :height 240
            :commands [[:pixels [0 0 320 240] size rgba {:sampling :linear}]]}])

(ui/decode-image "photo.jpg" {:size [320 240]})
```

## Canvas pixels and input

```clojure
[:canvas {:width 120 :height 60
          :commands [[:pixels [0 0 120 60] [2 1] (bytes [255 0 0 255 0 0 255 255])]]
          :on-wheel (fn [{:keys [x y dx dy modifiers]}] …)}]
```

`:pixels` draws a buffer into `[x y width height]` in canvas coordinates.
The source size is in pixels. Supply a Norn byte string with four bytes per
pixel in RGBA order, tightly packed in rows from top to bottom. Colors use
straight alpha. The command uses nearest-neighbor scaling by default.
Pass `{:sampling :linear}` as the final argument for smooth bilinear scaling.
The command respects canvas clipping, translation and paint order.
Invalid buffers and dimensions above the GPU texture limit are skipped.

`:on-wheel` receives the cursor position relative to the element, scroll
deltas in logical pixels, and a modifier set. Positive deltas are right and
down. Going up from the element under the pointer, the first of an enabled
handler and a scroll container or editor that can still move that way takes
the wheel. A scroll view inside an element with `:on-wheel` keeps scrolling,
and the handler hears the wheel once the view reaches its end. Wheel events
still waiting for Norn combine, so a fast trackpad scroll arrives as a few
events with the summed deltas.

Pass `:on-resize (fn [{:keys [window size scale]}] …)` in `ui/window` options.
`size` is `[width height]` in logical pixels. `ui/window-size` already
contains the new size when the callback runs.

## Grids and editors that show what they're given

`ui/grid` without `:height` fills its parent. It draws the rows that fit
in `:viewport` pixels (1000 by default). When its columns are wider than
the grid, the body scrolls sideways. Pass `:scroll-x` and `:on-scroll-x`,
and the header follows the body.

An editor can show text that isn't edited, such as a terminal's screen.
`ui/editor-show!` sets a model's text (a rope), each line's spans, and the
carets. It leaves the undo history alone and sends only the lines given,
or every line. A span's style is a theme token or `"#rrggbb"`. Add
`;bg=<colour>` for a background, `;u` for an underline and `;s` for a
strike. `ui/editor-hooks!` takes these hooks:

- `:key-hook` and `:text-hook` see keys and typed text first.
- `:command-hook` sees menu commands such as `:copy` and `:paste` first.
- `:cells-hook` receives the columns and rows of text that fit, whenever
  that changes. The element needs `:report-cells true` for this.

`:gutter false` and `:current-line false` turn off the line numbers and the
band behind the caret's line.

The bundled icons include `:database`, `:table`, `:globe`, `:send`, `:lock`,
`:copy`, `:download` and `:key`.

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
