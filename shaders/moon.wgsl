@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
    let uv = vec2((in.uv.x - 0.5) * uniforms.viewport.x / uniforms.viewport.y + 0.5, in.uv.y);
    let background = vec3(0.025, 0.035, 0.06);
    if any(uv < vec2(0.0)) || any(uv > vec2(1.0)) {
        return vec4(background, 1.0);
    }
    var art = textureSampleLevel(source, source_sampler, uv, 0.0);
    art = vec4(art.rgb * art.a, art.a);
    let p = (uv - vec2(0.5, 0.485)) / 0.4;
    let radius = length(p);
    let normal = vec3(p.x, -p.y, sqrt(max(0.0, 1.0 - dot(p, p))));
    let angle = uniforms.viewport.w * 6.28318530718;
    let day = smoothstep(-0.025, 0.055, dot(normal, vec3(sin(angle), 0.0, -cos(angle))));
    let disc = 1.0 - smoothstep(0.96, 1.015, radius);
    let shade = mix(vec3(0.16, 0.12, 0.24), vec3(1.0), day);
    let gold = smoothstep(0.04, 0.2, art.g - art.b) * smoothstep(0.15, 0.55, art.g);
    let ritual_phase = uniforms.viewport.z * 0.3;
    let sweep = pow(0.5 + 0.5 * cos(atan2(p.y, p.x) - ritual_phase), 10.0);
    let breath = 0.5 + 0.5 * sin(ritual_phase * 8.0);
    var rgb = art.rgb * mix(vec3(1.12 + breath * 0.22), shade, disc);
    rgb += vec3(1.0, 0.62, 0.2) * gold * (0.3 + sweep * 0.85) * disc;
    rgb = min(rgb, vec3(art.a));
    return vec4(rgb + background * (1.0 - art.a), 1.0);
}
