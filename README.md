# Hearth

A native Rust scene graph and Weft workspace UI. Rust owns rendering and
animation; Scheme supplies scene and workspace changes outside the frame loop.

```sh
guix shell -m manifest.scm
cargo run -- --frames 120
cargo clippy --all-targets -- -D warnings
cargo test
```

The demo opens a nonexclusive 480×320 layer-shell surface named `hearth` at
the top right. Without `--frames`, it remains open and redraws on configure;
finite runs use compositor frame callbacks. It reports the Vulkan adapter and
presented frame count. Quickshell remains running.

The manifest supplies Cargo, Clippy, rustfmt, GCC, Wayland and Vulkan loader.
`.cargo/config.toml` selects GCC; do not use `-Zbuild-std` on Guix. The host must
supply its matching GPU driver and a Vulkan ICD. On Guix System the default
profile already exposes these. If Vulkan cannot find the NVIDIA ICD, set
`VK_DRIVER_FILES` to the host profile's `share/vulkan/icd.d/nvidia_icd.x86_64.json`.

Retained scenes support ordered Rect, translated Group and nested Clip nodes.
`Scene::replace` validates an entire update before applying it; identical updates
stay clean. Colors are linear RGBA and both native and snapshot targets use sRGB.

```sh
cargo run -- --scene
cargo run -- --snapshot artifacts/scene.png
cargo test --test snapshots
```

Static surfaces wait for compositor events without requesting frame callbacks.
`--frames COUNT` explicitly enables repeated rendering for a finite benchmark.
The GPU snapshot test compares decoded RGBA pixels exactly with
`snapshots/m1-scene.png`; a failure writes `artifacts/m1-scene-actual.png`.
Update a baseline explicitly with `cargo run -- --snapshot snapshots/m1-scene.png`
and inspect it at 1× before committing. GPU tests fail if Vulkan is unavailable.

`Prop<T>` supports immediate, eased tween and analytic spring transitions, with
`Px`, `Rgba`, `Seconds` and `Phase` units. Retargeting preserves spring velocity.

```sh
cargo run -- --animate
cargo run -- --animate --frames 180 2> artifacts/frame-times.log
cargo run -- --animation-snapshots snapshots
```

The animated demo advances from Rust's monotonic clock on compositor callbacks.
It stops requesting frames when its spring settles. The finite benchmark keeps
rendering to its explicit frame count and logs each actual interval. Animation
snapshots sample the same spring at 60 Hz at 0, 0.15, 0.5 and 2 seconds; the GPU
tests compare all four PNGs exactly.

## Weft zones

```sh
cargo run -- --zones fixtures/weft-zones.json --snapshot artifacts/zones.png
mkdir -m 700 "$XDG_RUNTIME_DIR/hearth"
cargo run -- --listen "$XDG_RUNTIME_DIR/hearth/ui.sock"
# In another terminal, outside the render process:
guile -L scheme scheme/live.scm "$XDG_RUNTIME_DIR/hearth/ui.sock"
```

The panel shows typed text, metric, table and wiring data. Text uses Glyphon and
Cosmic Text, with bundled licensed DejaVu fonts for deterministic snapshots.
Long tables and excess zones have explicit preview limits. The socket wakes the
Wayland loop only when data arrives; Scheme never runs in a frame callback.
SIGINT/SIGTERM close the surface and remove the owned socket. Existing sockets
are never overwritten. See [the protocol](docs/protocol.md) for limits and bindings.

`cargo test --test zones_snapshot` compares `snapshots/w0-zones.png` exactly.
Regenerate it explicitly with the fixture command above targeting that path,
then inspect the PNG at 1×. The live producer needs Guile, guile-json and the
installed Weft CLI. It reads data only; the panel does not approve effects or
claim terminal leases.

Animated inputs have magnitude at most 1e12. Spring frequency (Hz) and damping
ratio must each lie in 1e-6..=1e6; tolerance is finite and positive. Scene updates
validate accumulated translations and extents before replacing the visible tree.

## Activity

```sh
cargo run -- --state fixtures/weft-activity.json --view activity --snapshot artifacts/activity.png
cargo run -- --view activity --listen "$XDG_RUNTIME_DIR/hearth/ui.sock"
guile -L scheme scheme/live.scm "$XDG_RUNTIME_DIR/hearth/ui.sock" --activity
```

Activity shows session host and delivery, watch targets, retained notices and
terminal lease holders. Missing host information is explicit; lease expiry is
unavailable in the current Weft API. Notices cover current watch subscribers
within Weft's bounded history. An inspection warning is not an empty inventory.
The snapshot carries separate generations for trusted activity and owner effects.
