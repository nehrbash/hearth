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
