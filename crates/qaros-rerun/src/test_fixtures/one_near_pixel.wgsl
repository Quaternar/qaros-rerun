// Test fixture for the eye output pass: a reverse-Z depth pattern with one nearer pixel, so a
// test can check which depth survives when the pass resamples to a smaller target.
// Top-left pixel: 0.5. Every other pixel: 0.25.

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4f {
    let corner = vec2f(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    return vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fragment_main(@builtin(position) position: vec4f) -> @builtin(frag_depth) f32 {
    let is_top_left = position.x < 1.0 && position.y < 1.0;
    return select(0.25, 0.5, is_top_left);
}
