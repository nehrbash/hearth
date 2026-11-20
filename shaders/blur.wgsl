@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
    let step = uniforms.parameters.xy * uniforms.parameters.z / uniforms.viewport.xy;
    let weights = array(0.2270270270, 0.1945945946, 0.1216216216, 0.0540540541, 0.0162162162);
    var color = textureSampleLevel(source, source_sampler, in.uv, 0.0) * weights[0];
    for (var i = 1u; i < 5u; i += 1u) {
        let offset = step * f32(i);
        color += textureSampleLevel(source, source_sampler, in.uv + offset, 0.0) * weights[i];
        color += textureSampleLevel(source, source_sampler, in.uv - offset, 0.0) * weights[i];
    }
    return color;
}
