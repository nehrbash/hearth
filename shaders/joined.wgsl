fn rounded_box(point: vec2<f32>, center: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let distance = abs(point - center) - half_size + radius;
    return length(max(distance, vec2(0.0))) + min(max(distance.x, distance.y), 0.0) - radius;
}

fn circular_min(a: f32, b: f32, radius: f32) -> f32 {
    return max(radius, min(a, b)) - length(max(vec2(radius) - vec2(a, b), vec2(0.0)));
}

@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
    let size = uniforms.viewport.xy;
    let point = in.uv * size;
    let half_size = vec2(size.x * 0.16, size.y * 0.29);
    let gap = 16.0 + 10.0 * sin(uniforms.viewport.z);
    let first = vec2(size.x * 0.5 - half_size.x - gap * 0.5, size.y * 0.43);
    let second = vec2(size.x * 0.5 + half_size.x + gap * 0.5, size.y * 0.57);
    let a = rounded_box(point, first, half_size, 28.0);
    let b = rounded_box(point, second, half_size, 28.0);
    var distance = min(a, b);
    if uniforms.parameters.w > 0.0 {
        distance = min(distance, circular_min(a, b, uniforms.parameters.w));
    }
    let aa = max(fwidth(distance), 0.5);
    let coverage = 1.0 - smoothstep(-aa, aa, distance);
    let rim = 1.0 - smoothstep(1.0, 3.0, abs(distance));
    let fill = mix(vec3(0.06, 0.13, 0.20), vec3(0.13, 0.34, 0.30), in.uv.y);
    let color = mix(fill, vec3(0.3, 0.62, 0.53), rim * 0.6);
    return vec4(mix(vec3(0.025, 0.035, 0.06), color, coverage), 1.0);
}
