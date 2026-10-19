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
    // backdrop blends: rule progress (< 0: no rule), vagueness
    params2: vec4<f32>,
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

// Backdrop-reading layer blends (the exe's nexl_nxshader_ps_* family, blend state 7): tex0 = sprite,
// tex1 = copy of the target taken before this quad, tex2 = optional rule mask.
// params = (op, fill, target w, target h), params2 = (rule progress or -1, rule vagueness, -, -).
// Formulas follow the shader disassembly; s = texture rgb * colour rgb, a = texture alpha * colour alpha.
fn ps_mix(d: vec3<f32>, r: vec3<f32>, a: f32) -> vec3<f32> {
    return d + a * (r - d);
}

@fragment
fn fs_ps(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex0, samp, i.uv);
    let d = textureLoad(tex1, vec2<i32>(i.pos.xy), 0).rgb;
    var s = t.rgb * i.color.rgb;
    if (tu.params.y > 0.5) { s = i.color.rgb; }
    var ca = i.color.a;
    if (tu.params2.x >= 0.0) {
        let r = textureSample(tex2, samp, i.pos.xy / tu.params.zw).r;
        let v = max(tu.params2.y, 0.001);
        ca *= clamp((tu.params2.x * (1.0 + v) - r) / v, 0.0, 1.0);
    }
    let a = t.a * ca;
    let one = vec3<f32>(1.0);
    let zero = vec3<f32>(0.0);
    let op = u32(tu.params.x + 0.5);
    // the alpha-weighted shaders leave the backdrop alone where a = 0 (their `$p0 exit`)
    if (a <= 0.0 && op <= 15u) {
        return vec4<f32>(d, 1.0);
    }
    var o = d;
    switch (op) {
        case 1u: { o = ps_mix(d, min(d + s, one), a); }
        case 2u: { o = ps_mix(d, min(d, s), a); }
        case 3u: { o = ps_mix(d, max(d, s), a); }
        case 4u: { o = ps_mix(d, max(d + s - one, zero), a); }
        case 5u: {
            let r = select(zero, max(one - (one - d) / s, zero), s > zero);
            o = ps_mix(d, r, a);
        }
        case 6u: {
            let k = max(one - s, zero);
            o = ps_mix(d, select(one, min(d / k, one), k > zero), a);
        }
        case 7u: {
            let k = max(one - a * s, zero);
            o = select(zero, min(d / k, one), d > zero);
        }
        case 8u: { o = ps_mix(d, abs(d - s), a); }
        case 9u: { o = abs(d - a * s); }
        case 10u: { o = ps_mix(d, d + s - 2.0 * d * s, a); }
        case 11u, 12u: {
            var h = s;
            if (op == 12u) { h = min(s / t.a, one); }
            let r = select(one - 2.0 * (one - h) * (one - d), 2.0 * d * h, h < vec3<f32>(0.5));
            o = ps_mix(d, r, a);
        }
        case 13u: {
            let r = select(one - 2.0 * (one - s) * (one - d), 2.0 * d * s, d < vec3<f32>(0.5));
            o = ps_mix(d, r, a);
        }
        case 14u: {
            let r = select(pow(d, one / (2.0 * s)), pow(d, 2.0 - 2.0 * s), s < vec3<f32>(0.5));
            o = ps_mix(d, r, a);
        }
        case 15u: { o = max(d - a * (one - s), zero); }
        case 16u: { o = min(d + s * ca, one); }
        case 17u: { o = max(d - ca * (one - s), zero); }
        case 18u: { o = s; }
        default: {}
    }
    return vec4<f32>(o, 1.0);
}
