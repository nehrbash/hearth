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

Retained scenes support ordered Rect and Text nodes, translated Group and nested Clip nodes.
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

## Owner decisions

```sh
cargo run -- --state fixtures/weft-owner.json --view owner --snapshot artifacts/owner.png
cargo run -- --view owner --listen "$XDG_RUNTIME_DIR/hearth/ui.sock" --owner-controls
guile -L scheme scheme/live.scm "$XDG_RUNTIME_DIR/hearth/ui.sock" --activity
```

Owner views default to read-only. `--owner-controls` enables explicit pointer
press/release decisions through `weft owner`; render updates cannot issue commands.
Proposals use escaped text so control characters remain visible. Approval is
disabled when the complete proposal does not fit. Rejection remains available
for pending proposals. Interrupted proposals require a fresh proposal in Weft.

The UI checks the displayed generation, exact content and pending state, and
allows one request at a time with no automatic retry. Weft verifies exact content
and pending state at execution. Its current command API has no atomic generation
argument, so a broker restart between inspection and execution is not guarded by
the UI's local generation check. Closing the view does not revoke a submitted
request. Broker responses are not proof of successful external effects; receipts
arrive through subsequent state snapshots. Tests use mocked executors only.

Text nodes preserve painter order across rectangle and text batches. Labels support
clipping, word wrapping, and single-line ellipsis at shaped grapheme boundaries.
Bundled fonts cover the mixed-script fixtures; system-font fallback is an explicit
TextSystem option. `tests/text_scene_snapshot.rs` exercises translation, nested
clipping, Arabic/Hebrew, combining marks, and overpainting at native resolution.

Retained Shader and Layer nodes render at local bounds, obey ancestor clips,
and compose in scene order. Layers render their children into transparent targets;
blur uses two GPU passes. Shader uniforms are Rust data. Invalid nested WGSL is
rejected before clearing the previous frame. Resource limits, the custom fragment
contract, and Burl asset provenance are in [effects](docs/effects.md).

## Workspace bar

```sh
cargo run -- --workspaces
cargo run -- --workspace-state fixtures/workspaces.json --monitor DP-1 --snapshot artifacts/workspaces.png
```

The read-only vertical bar follows Hyprland workspace events on a worker thread.
It shows fixed slots 1–7, occupancy, the active workspace on its actual output,
other-monitor markers, and additional named/special workspaces. Empty input
regions pass pointer events through; the bar reserves no desktop space.

Compositor discovery matches the current Wayland display. An ambiguous match
requires `--hyprland-instance SIGNATURE`. Snapshot requests are bounded; a lost
connection is shown explicitly while reconnecting to the same instance. Monitor
selection is available for headless snapshots; native placement determines its
monitor. This implements the workspace indicator's state semantics; Burl's tarot
art, app seals, popouts and workspace dispatch remain separate widget work.

## Scheme scenes and reactive widgets

```sh
cargo run -- --view scene --listen "$XDG_RUNTIME_DIR/hearth/ui.sock"
guile -L scheme scheme/scene-demo.scm "$XDG_RUNTIME_DIR/hearth/ui.sock"
cargo run -- --view scene --state fixtures/scheme-scene.json --snapshot artifacts/scheme-scene.png
```

The `(scene)` module constructs retained scene data and sends it through the same
private socket as workspace snapshots. `--view scene` uses a 640×400 demo surface.
The Rust `SceneBuilder` composes nodes; `BoundScene` connects signals to validated
scenes. Signals coalesce, unchanged values stay clean, and disposing an owner
releases its computations. A panicking computation is disposed before unwinding.
The fixture proves identical Scheme/Rust trees for status, metric and session
widgets; two PNGs verify a signal-driven update without manual invalidation.
