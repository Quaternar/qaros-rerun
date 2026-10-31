// The last pass of an eye on the GPU path: one fullscreen triangle per eye target.
//
// composited_color: Rerun's composited eye, already sRGB-encoded by re_renderer's compositor.
// reverse_depth: the picking layer's depth at the same resolution, Rerun's infinite reverse-Z
// (0 where nothing was drawn, near / distance elsewhere).
// stamp: the frame stamp (`--frame-stamp`) over the top `stamp_rows` rows of the eye; a texel
// with alpha is stamped, and a stamped pixel takes the stamp's color and `stamp_reverse_depth`.
// The conversion mirrors xr_math::DepthRange::encode_from_reverse_z.
//
// A depth target of the color's size is written together with color, pixel for pixel. A depth
// target of another size gets a pass of its own: each depth pixel takes the nearest depth
// (largest reverse-Z) of the eye pixels it covers, so nothing drawn is pushed behind what
// another app volume shows.

const ENCODING_FORWARD_NORMALIZED: u32 = 0u;

struct DepthOutput {
    near_plane: f32,
    far_plane: f32,
    encoding: u32,
    // 1 keeps every pixel (opaque background), 0 clears uncovered pixels to transparent.
    covers_everything: u32,
    // Eye pixels per depth target pixel in each dimension; (1, 1) when the sizes match.
    source_per_target: vec2f,
    // 0 without a stamp.
    stamp_rows: u32,
    stamp_reverse_depth: f32,
};

@group(0) @binding(0)
var composited_color: texture_2d<f32>;

@group(0) @binding(1)
var reverse_depth: texture_depth_2d;

@group(0) @binding(2)
var<uniform> depth_output: DepthOutput;

@group(0) @binding(3)
var stamp: texture_2d<f32>;

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4f {
    let corner = vec2f(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    return vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
}

fn is_stamped(pixel: vec2i) -> bool {
    if u32(pixel.y) >= depth_output.stamp_rows {
        return false;
    }
    return textureLoad(stamp, pixel, 0).a > 0.5;
}

fn eye_reverse_depth(pixel: vec2i) -> f32 {
    if is_stamped(pixel) {
        return depth_output.stamp_reverse_depth;
    }
    return textureLoad(reverse_depth, pixel, 0);
}

fn coverage(pixel: vec2i) -> f32 {
    let is_covered = depth_output.covers_everything != 0u
        || eye_reverse_depth(pixel) > 0.0;
    return select(0.0, 1.0, is_covered);
}

fn covered_color(pixel: vec2i) -> vec4f {
    if is_stamped(pixel) {
        return textureLoad(stamp, pixel, 0);
    }
    return textureLoad(composited_color, pixel, 0) * coverage(pixel);
}

fn encoded_depth(reverse: f32) -> f32 {
    let near = depth_output.near_plane;
    let far = depth_output.far_plane;
    // Nothing drawn is infinitely far, which the clamp puts on the far plane.
    let distance = select(far, near / reverse, reverse > 0.0);
    let clamped = clamp(distance, near, far);
    if depth_output.encoding == ENCODING_FORWARD_NORMALIZED {
        return far * (clamped - near) / (clamped * (far - near));
    }
    return near * (far - clamped) / (clamped * (far - near));
}

@fragment
fn fragment_color(@builtin(position) position: vec4f) -> @location(0) vec4f {
    return covered_color(vec2i(position.xy));
}

struct ColorAndDepth {
    @location(0) color: vec4f,
    @builtin(frag_depth) depth: f32,
};

@fragment
fn fragment_color_and_depth(@builtin(position) position: vec4f) -> ColorAndDepth {
    let pixel = vec2i(position.xy);
    var output: ColorAndDepth;
    output.color = covered_color(pixel);
    output.depth = encoded_depth(eye_reverse_depth(pixel));
    return output;
}

@fragment
fn fragment_resampled_depth(@builtin(position) position: vec4f) -> @builtin(frag_depth) f32 {
    let target_pixel = floor(position.xy);
    let source_size = vec2i(textureDimensions(reverse_depth));
    let first = vec2i(floor(target_pixel * depth_output.source_per_target));
    let end = min(
        vec2i(ceil((target_pixel + 1.0) * depth_output.source_per_target)),
        source_size,
    );
    var nearest = 0.0;
    for (var row = first.y; row < end.y; row++) {
        for (var column = first.x; column < end.x; column++) {
            nearest = max(nearest, eye_reverse_depth(vec2i(column, row)));
        }
    }
    return encoded_depth(nearest);
}
