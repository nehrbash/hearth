# Shader and layer effects

`Layer` is a reusable sRGB render target that can also be sampled or copied.
`Shader` accepts a WGSL fragment function over a fullscreen quad with a uniform
buffer, sampled source and filtering sampler. Uniform changes stay in Rust.
Invalid shader modules return an error through wgpu's validation scope.
The fragment entry point is `fragment(in: VertexOut) -> @location(0) vec4<f32>`;
`in.uv` spans zero to one. Bindings expose `uniforms.viewport` (width, height,
time, phase), `uniforms.parameters`, `source` and `source_sampler`. The low-level shader pass replaces its complete target. Scene shader nodes
render into bounded targets, then composite in painter order using premultiplied
RGBA. Custom fragments must return premultiplied color; they receive a transparent
source texture. WGSL is trusted application code, not untrusted Weft data.

`EffectRenderer` holds pipelines, the moon texture and reusable intermediate
layers. Its modes demonstrate a textured lunar terminator, an unfiltered scene,
a separable two-pass Gaussian blur, and rounded panels with a circular smooth
union. `Separated` supplies the same panel geometry with blending disabled.
Textures are reused while the output size stays unchanged.

The moon port preserves Burl's painted sprite, spherical lighting, lunar phase,
emissive corona and ritual gold sweep. Its phase completes in eight demo seconds;
Burl's live weather phase remains the eventual application input. Qt's surrounding
seal and outer halo are separate QML items and are not part of this shader.

## Provenance

The WGSL moon derives from Burl's `assets/shaders/lunar-moon.frag`; the sprite is
an unchanged copy of `assets/images/nocturne/crimson-moon.png`. The joined-panel
shader uses the rounded-box and circular smooth-min formulas from
`plugin/src/Burl/Blobs/shaders/blob.frag`. Burl supplies GPL version 3 in its
repository LICENSE at commit `f5df3303e25b9445ab5f038f1d33b80d54607f6e`; a copy is in `fixtures/effects/LICENSE-BURL`. Hearth's package
license is GPL-3.0-or-later.

The blur is a nine-tap Gaussian in each axis. Its source is first rendered into
a layer, horizontal blur writes a second layer, and vertical blur samples that
result into the target. It performs no CPU image filtering.

## Retained scene nodes

`Node::Shader { bounds, kind, time, parameters }` supports `ShaderKind::Moon`,
`Joined`, or `Custom(String)`. Moon time controls the eight-second phase;
joined panels use `parameters[3]` as their smooth-union radius.

`Node::Layer { bounds, children, effect }` renders children in local coordinates
onto transparency. `LayerEffect::None` composites directly; `Blur { radius }`
uses horizontal and vertical GPU passes. Group translation and ancestor clips
apply to the resulting texture without shifting the child layout.

Renderers retain targets by traversal slot, size and shader kind. Nested renderers
share one moon texture. Offscreen nodes are limited to 4096 pixels per side,
128 nodes, and 16 million aggregate pixels charging three targets per node.
Scene nesting remains limited to 64 levels. Blur radius must be finite and between
zero and 64. Geometry and uniform failures reject scene replacement atomically.
Recursive shader compilation finishes before clearing the destination, so invalid
WGSL leaves its previous image intact and returns an error.
