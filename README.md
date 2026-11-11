# skein-ui

Native desktop user interfaces for [Skein](https://github.com/crazymevt/skein), built
from Skein objects and events: every widget is backed by the platform's own control, so
programs look and behave the way their users expect.

```clojure
(ns hello.main
  (:require [skein.ui :as ui]
            [skein.ui.headless :as headless]))

(defn -main [& args]
  (ui/use-backend! (headless/backend))
  (let [name (ui/entry {:placeholder "Your name"})
        w (ui/window {:title "Hello"}
            (ui/vbox {:spacing 8}
              (ui/label {:text "Who are you?"})
              name
              (ui/button {:label "Greet"
                          :on-clicked (fn [e] (println "Hello," (:text name)))})))]
    (ui/show! w)))
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

This is Skein's milestone M2.14 (its D-090 and D-097), a port of the design of LGT, a
Common Lisp toolkit. It has the core, layout and the simple controls (labels, buttons,
toggles, check boxes, radio buttons, entries, sliders, spin buttons, progress bars,
spinners, combo boxes, images and frames) on the headless backend, for tests. LGT's
other widgets, then the Cocoa, Win32 and GTK 4 backends, come next.

## Using it

```clojure
{:name "app"
 :main app.main/-main
 :deps {skein/ui {:git "https://github.com/crazymevt/skein-ui" :tag "v0.1.0"}}}
```

## Testing

With a Skein checkout beside this one:

```sh
../skein/skein test
```

CI does the same on macOS, Linux and Windows with Skein's main branch.

## License

MIT, as Skein.
