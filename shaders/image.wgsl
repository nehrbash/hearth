@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
    let local = (in.uv * uniforms.viewport.xy - uniforms.parameters.xy) / uniforms.parameters.zw;
    if any(local < vec2(0.0)) || any(local > vec2(1.0)) { discard; }
    let ratio = uniforms.crop.zw / uniforms.parameters.zw;
    let maximum = max(0.0, floor(log2(min(uniforms.crop.z, uniforms.crop.w))) - 1.0);
    let level = clamp(log2(max(ratio.x, ratio.y)), 0.0, maximum);
    let footprint = select(exp2(ceil(level)), 0.5, level <= 0.0);
    let inset = min(vec2(footprint), uniforms.crop.zw * 0.5);
    let pixel = clamp(uniforms.crop.xy + local * uniforms.crop.zw,
        uniforms.crop.xy + inset, uniforms.crop.xy + uniforms.crop.zw - inset);
    let uv = pixel / vec2<f32>(textureDimensions(source));
    return textureSampleLevel(source, source_sampler, uv, level) * uniforms.viewport.z;
}
