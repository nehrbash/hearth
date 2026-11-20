# Retained vector paths

`Node::Path(PathData)` draws immutable meshes built with Lyon. Construct a path
with `PathData::new(commands, fill, stroke)`; `Command` supports Move, Line,
Quad, Cubic and Close. Fills support nonzero and even-odd rules. Strokes expose
width, cap and join. Both styles use linear RGBA.

Meshes are tessellated once during construction and shared when scenes are
cloned. Drawing preserves painter order, inherited translations, nested clip
scissors and offscreen layers. The existing single-sample triangle pipeline
produces pixel-stepped edges; paths do not add a separate antialiasing pass.

Input is limited to 4096 commands and coordinates within ±1,000,000. Curve
flattening at 0.25 pixels is capped at 65,536 segments. A mesh can contain at
most 65,536 vertices and 393,216 indices; scene totals are bounded too. Invalid
geometry is rejected before scene replacement. The Scheme wire schemas do not
expose Path yet; this is the retained Rust scene API.

```sh
cargo test --test path_snapshot
HEARTH_UPDATE_SNAPSHOTS=1 cargo test --test path_snapshot
```

Only use the second command when intentionally replacing the baseline, then
inspect `snapshots/m3-paths.png` at 1×. It covers curves, fills, stroke caps,
transforms, clips, layers and painter order. Algorithm reference:
[Lyon tessellation](https://docs.rs/lyon_tessellation/latest/lyon_tessellation/).
