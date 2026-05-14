// Solid-color quad shader for the scene-graph renderer.
//
// Vertices arrive in logical-pixel coordinates (origin top-left, y down).
// The vertex stage converts them to clip space using the viewport size.

struct Viewport {
    // x = width, y = height, in logical pixels.
    size: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> viewport: Viewport;

struct VsIn {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    // pixel -> normalized device coords: x [0,w] -> [-1,1], y [0,h] -> [1,-1]
    let ndc = vec2<f32>(
        in.position.x / viewport.size.x * 2.0 - 1.0,
        1.0 - in.position.y / viewport.size.y * 2.0,
    );
    out.clip_position = vec4<f32>(ndc, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
