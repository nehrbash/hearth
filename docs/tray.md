# System tray

Hearth runs a separate StatusNotifier worker and a Scheme controller. The worker
joins an existing watcher as a host. If no watcher exists, it requests ownership
without replacing an incumbent or queuing for a later takeover. Development
checks use private buses; the owner's desktop watcher remains untouched.

Items are identified by their unique D-Bus owner and object path. Explicit
controls bind the current worker session and observed item/menu state. The UI
provides activation, secondary activation, context menus and scrolling, and
renders bounded dbusmenu layouts with disabled entries, separators and check
states. Unknown outcomes are never automatically retried.

The [Status Notifier specification](https://specifications.freedesktop.org/status-notifier-item/latest-single/)
defines raw pixmaps as network-byte-order ARGB. The adapter converts them to
RGBA for native rendering. Named icons use borrowed freedesktop-icons lookup;
`HEARTH_ICON_THEME` selects the theme, defaulting to `hicolor`. Absolute local
paths and direct files in an item's icon theme path are also supported.

PNG and SVG files are capped at 256 KiB. PNGs have a four-megapixel budget and
4096-pixel side limit; SVG dimensions also cap at 4096, with nesting depth 32.
SVG filters, SVG text, compressed SVGZ, and external or embedded image references
are unsupported. Raster
output fits within 64×64 pixels. SVG parsing and rasterization run outside the
frame loop; the renderer receives decoded RGBA. A missing or unsupported icon
can fall back to the item's raw pixmap.

Weft receives sanitized item/menu state rather than image byte arrays. The
native UI and observation publisher are independent processes, so broker
availability cannot block pointer controls or frames.

Run the native renderer on channel `tray` at 720×760, then start its controller:

```sh
guile -L scheme scheme/tray-ui.scm RENDERER_SOCKET target/debug/hearth-tray
```

Publish observations separately:

```sh
target/debug/hearth-tray --observe | guile -L scheme scheme/tray-ui.scm --publish
```

`--bus-address ADDRESS` selects a private bus, and `--once` exits after one
observation. The publisher strips icon bytes before saving `hearth-tray` and
marks the state unavailable on EOF. The UI channel is sensitive, preventing a
generic scene mirror from copying icons or unbounded menu content into Weft.

Menus use both the application's layout revision and a local content epoch.
The epoch also changes for property updates that leave the layout revision
unchanged. Opening a menu calls `AboutToShow` and waits for refreshed state
before enabling entries. Application replies acknowledge method completion;
they do not prove that a window became visible.
