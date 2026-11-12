# skein-ui

Native desktop user interfaces for [Skein](https://github.com/crazymevt/skein), built
from Skein objects and events: every widget is backed by the platform's own control, so
programs look and behave the way their users expect.

```clojure
(ns hello.main
  (:require [skein.ui :as ui]
            [skein.ui.cocoa :as cocoa]))

(defn -main [& args]
  (ui/use-backend! (cocoa/backend))
  (let [name (ui/entry {:placeholder "Your name"})
        w (ui/window {:title "Hello"}
            (ui/vbox {:spacing 8}
              (ui/label {:text "Who are you?"})
              name
              (ui/button {:label "Greet"
                          :on-clicked (fn [e] (println "Hello," (:text name)))})))]
    (ui/on w :close-request (fn [e] (ui/quit!)))
    (ui/show! w)
    (ui/run!)))
```

- **Widgets** are made by functions that take an option map and children:
  `(ui/button {:label "Save" :on-clicked f})`. Their classes have a `-widget` suffix
  (`ui/button-widget`), for `defclass` and `defmethod`.
- **Properties** are cell slots: `(:text label)` reads one, and
  `(set-slot! label :text "Hi")` or `(update-slot! slider :value + 5)` changes it; the
  native control follows. Specialize `ui/property-changed` to react in a subclass.
- **Events** are objects emitted under a signal, such as `:clicked`. Connect handlers
  with `ui/on` (or `:on-clicked` options) and disconnect them with `ui/off`, or
  specialize `ui/handle-event`. `ui/veto` refuses a vetoable event, such as a window's
  `:close-request`.
- **Layout** is boxes (`ui/vbox`, `ui/hbox`) and grids (`ui/grid`), done the same on
  every platform from the widgets' natural sizes and their layout options: `:hexpand`,
  `:vexpand` and `:expand` take extra space, `:halign` and `:valign` place a widget in
  its space, `:margin` goes around it, and `:column` and `:row` place it in a grid.

- **Drawing:** a `ui/canvas` emits `:draw` with a graphics context to draw on
  (`ui/fill-rectangle!`, `ui/draw-line!`, `ui/draw-text!`, ...), and mouse and key events.
- **Menus, dialogs and the rest:** menu bars with shortcuts (`ui/menu-bar`, `ui/menu`,
  `ui/menu-item`), the platform's alerts and file choosers (`ui/message-box`,
  `ui/choose-file`), timers (`ui/after`, `ui/every`) and the clipboard.

This is Skein's milestone M2.14 (its D-090 and D-097), a port of the design of LGT, a
Common Lisp toolkit. It has LGT's widgets except the web view: labels, buttons, toggles,
check boxes, radio buttons, entries, text views, sliders, spin buttons, progress bars,
spinners, combo boxes, list boxes, sortable tables, trees, images, color and date
pickers, frames, notebooks, stacks, tabbed panes (closable, reorderable tabs that drag
between panes), split panes, scrolled windows, canvases, popovers, completion popups, and
code views on shared text buffers (undo, styled runs, line numbers).

For Skein Studio (Skein's M3.6, D-108) it adds rich text from Hiccup data
(`ui/rich-text`, with `append-content!` and `:link-activated`), runs with fonts and
paragraphs (`:size`, `:family`, `:indent`, `:first-indent`, `:space-above`,
`:space-below`, `:block-background`, `:tabs`), editable table cells (`:editable`
columns, `:cell-edited`), the row and cell at a point (`item-at-point`,
`cell-at-point`) and window snapshots (`snapshot`, as PNG bytes or a file).

- **Backends:** `skein.ui.cocoa` (AppKit, on macOS), `skein.ui.gtk` (GTK 4, on Linux, and
  on macOS with Homebrew's `gtk4`) and `skein.ui.headless` (no screen, for tests). Win32
  comes next.
- **Threads:** the toolkit runs on the main thread, where `-main` and `ui/run!` run.
  Widgets can be made and changed from any fiber: the call is handed to the main thread,
  which the backend wakes from the event loop.

## Using it

```clojure
{:name "app"
 :main app.main/-main
 :deps {skein/ui {:git "https://github.com/crazymevt/skein-ui" :tag "v0.1.0"}}}
```

## Examples

`examples/` is a project of its own (it uses this checkout through `:deps {skein/ui
{:local ".."}}`): the examples of LGT, the toolkit skein-ui follows, ported, and a launcher
that lists them. From `examples/`:

```sh
../../skein/skein run src/examples/launcher.skn                    # the launcher
../../skein/skein run src/examples/launcher.skn widget-explorer    # one example
../../skein/skein run src/examples/launcher.skn editor --gtk       # with GTK on macOS
```

The widget explorer shows every kind of widget live beside the source that makes it.
Each example is also built and exercised by a test on the headless backend.

## Testing

With a Skein checkout beside this one:

```sh
../skein/skein test
(cd examples && ../../skein/skein test)
```

CI does the same on macOS, Linux and Windows with Skein's main branch.

## License

MIT, as Skein.
