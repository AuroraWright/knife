// Sprite / text / transition shaders for the knife renderer.
// Colors are blended in gamma space (textures are sampled as UNORM), like the original.
// Sprite fragment shaders output premultiplied alpha.

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var o: VsOut;
    o.pos = vec4<f32>(v.pos, 0.0, 1.0);
    o.uv = v.uv;
    o.color = v.color;
    return o;
}

@group(0) @binding(0) var tex0: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

// Straight-alpha sprite tinted by vertex color (rgb multiply, a = opacity).
@fragment
fn fs_sprite(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex0, samp, i.uv);
    let a = t.a * i.color.a;
    return vec4<f32>(t.rgb * i.color.rgb * a, a);
}

// Sprite whose rgb is replaced by a solid color, keeping the texture alpha
// (used for silhouettes / color fills such as STEC mode 1).
@fragment
fn fs_fill(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex0, samp, i.uv);
    let a = t.a * i.color.a;
    return vec4<f32>(i.color.rgb * a, a);
}

// Single-channel coverage texture (glyph atlas) tinted by vertex color.
@fragment
fn fs_text(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex0, samp, i.uv);
    let a = t.r * i.color.a;
    return vec4<f32>(i.color.rgb * a, a);
}

// Monochrome (grayscale) sprite.
@fragment
fn fs_mono(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex0, samp, i.uv);
    let l = dot(t.rgb, vec3<f32>(0.299, 0.587, 0.114));
    let a = t.a * i.color.a;
    return vec4<f32>(vec3<f32>(l) * i.color.rgb * a, a);
}

// Negative sprite.
@fragment
fn fs_nega(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex0, samp, i.uv);
    let a = t.a * i.color.a;
    return vec4<f32>((vec3<f32>(1.0) - t.rgb) * i.color.rgb * a, a);
}

// ---- transitions: old frame (tex0) -> new frame (tex1) through a rule image (tex2)

struct TransUniform {
    // x: progress 0..1, y: vagueness (0..1), z: use rule (0/1), w: invert rule
    params: vec4<f32>,
};

@group(0) @binding(2) var tex1: texture_2d<f32>;
@group(0) @binding(3) var tex2: texture_2d<f32>;
@group(0) @binding(4) var<uniform> tu: TransUniform;

@fragment
fn fs_transition(i: VsOut) -> @location(0) vec4<f32> {
    let a = textureSample(tex0, samp, i.uv);
    let b = textureSample(tex1, samp, i.uv);
    var t = tu.params.x;
    if (tu.params.z > 0.5) {
        var r = textureSample(tex2, samp, i.uv).r;
        if (tu.params.w > 0.5) { r = 1.0 - r; }
        let v = max(tu.params.y, 0.001);
        t = clamp((tu.params.x * (1.0 + v) - r) / v, 0.0, 1.0);
    }
    return vec4<f32>(mix(a.rgb, b.rgb, t), 1.0);
}

// Sprite masked by a screen-space rule image: params = (progress, vague, target w, target h).
@fragment
fn fs_rule(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex0, samp, i.uv);
    let r = textureSample(tex2, samp, i.pos.xy / tu.params.zw).r;
    let v = max(tu.params.y, 0.001);
    let m = clamp((tu.params.x * (1.0 + v) - r) / v, 0.0, 1.0);
    let a = t.a * i.color.a * m;
    return vec4<f32>(t.rgb * i.color.rgb * a, a);
}

// One pass of a separable gaussian blur: params = (dir x, dir y, sigma, -).
@fragment
fn fs_blur(i: VsOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let stp = tu.params.xy / dims;
    let sigma = max(tu.params.z, 0.01);
    var acc = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var k = -12; k <= 12; k++) {
        let x = f32(k);
        let w = exp(-x * x / (2.0 * sigma * sigma));
        acc += textureSampleLevel(tex0, samp, i.uv + stp * x, 0.0).rgb * w;
        wsum += w;
    }
    return vec4<f32>(acc / wsum, 1.0);
}

// Plain copy (canvas -> surface).
@fragment
fn fs_copy(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex0, samp, i.uv);
    return vec4<f32>(t.rgb * i.color.rgb, 1.0);
}
