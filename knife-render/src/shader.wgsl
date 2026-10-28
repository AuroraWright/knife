// Sprite / text / transition shaders for the knife renderer.
// Colors are blended in gamma space (textures are sampled as UNORM), like the original.
// Sprite fragment shaders output premultiplied alpha.

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) hx: vec4<f32>,
    @location(4) hy: vec4<f32>,
    @location(5) s0: vec4<f32>,
    @location(6) s1: vec4<f32>,
    @location(7) s2: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    // per-quad parameters (`Quad::haze`), the same at every vertex: flat, so packed values stay exact
    @location(2) @interpolate(flat) hx: vec4<f32>,
    @location(3) @interpolate(flat) hy: vec4<f32>,
    @location(4) s0: vec4<f32>,
    @location(5) s1: vec4<f32>,
    @location(6) s2: vec4<f32>,
};

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var o: VsOut;
    o.pos = vec4<f32>(v.pos, 0.0, 1.0);
    o.uv = v.uv;
    o.color = v.color;
    o.hx = v.hx;
    o.hy = v.hy;
    o.s0 = v.s0;
    o.s1 = v.s1;
    o.s2 = v.s2;
    return o;
}

// STRA haze (nexl_nxshader_haze): both offsets are waves along the texture's v axis.
// h = (amplitude in uv, waves per uv.y, phase in cycles, -); zero amplitude is a plain lookup.
fn haze_uv(i: VsOut) -> vec2<f32> {
    let tau = 6.2831853;
    let dx = sin(tau * (fract(i.uv.y * i.hx.y) + fract(i.hx.z))) * i.hx.x;
    let dy = cos(tau * (fract(i.uv.y * i.hy.y) + fract(i.hy.z))) * i.hy.x;
    return i.uv + vec2<f32>(dx, dy);
}

@group(0) @binding(0) var tex0: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@group(1) @binding(0) var soft_lut: texture_2d<f32>;

// Clip inset mask (sub_32E30): 1 - s2.w inside the inner rectangle s0 (relative to the pivot s1.xy, turned by
// asin(s1.z)), fading over s2.x target pixels past it along an alphagradation texture's radial profile (LUT row
// s2.y of s2.z + 1 texels, edge first), sampled bilinearly as the scaled 9-slice draws it.
fn soft_mask(i: VsOut) -> f32 {
    if (i.s2.x <= 0.0) {
        return 1.0;
    }
    let p = i.pos.xy - i.s1.xy;
    let q = vec2<f32>(p.x * i.s1.w + p.y * i.s1.z, p.y * i.s1.w - p.x * i.s1.z);
    let d = max(max(i.s0.xy - q, q - i.s0.zw), vec2<f32>(0.0));
    if (d.x <= 0.0 && d.y <= 0.0) {
        return 1.0 - i.s2.w;
    }
    let n = i.s2.z;
    let t = clamp((1.0 - length(d) / i.s2.x) * n - 0.5, 0.0, n);
    let row = i32(i.s2.y);
    let k = i32(floor(t));
    let a = textureLoad(soft_lut, vec2<i32>(k, row), 0).r;
    let b = textureLoad(soft_lut, vec2<i32>(min(k + 1, i32(n)), row), 0).r;
    return mix(a, b, t - f32(k));
}

// A layer image texel. Haze layers (hx.w = 1) are first drawn into an intermediate with blend state 0 (colour
// SrcAlpha / OneMinusSrcAlpha, alpha One / One) on transparent black, so the layer's blend then reads
// (rgb * a, a); other layers go through blend state 4 (a straight copy).
fn sample_img(i: VsOut) -> vec4<f32> {
    let t = textureSample(tex0, samp, haze_uv(i));
    let a = t.a * soft_mask(i);
    if (i.hx.w > 0.5) {
        return vec4<f32>(t.rgb * a, a);
    }
    return vec4<f32>(t.rgb, a);
}

// Straight-alpha sprite tinted by vertex color (rgb multiply, a = opacity).
@fragment
fn fs_sprite(i: VsOut) -> @location(0) vec4<f32> {
    let t = sample_img(i);
    let a = t.a * i.color.a;
    return vec4<f32>(t.rgb * i.color.rgb * a, a);
}

// shader noalpha: the texture's rgb times the colour, alpha 1
@fragment
fn fs_noalpha(i: VsOut) -> @location(0) vec4<f32> {
    let t = sample_img(i);
    return vec4<f32>(t.rgb * i.color.rgb, 1.0);
}

// shader forcealpha: the texture's rgb times the colour, alpha the colour's alone
@fragment
fn fs_forcealpha(i: VsOut) -> @location(0) vec4<f32> {
    let t = sample_img(i);
    return vec4<f32>(t.rgb * i.color.rgb * i.color.a, i.color.a);
}

// Sprite whose rgb is replaced by a solid color, keeping the texture alpha
// (used for silhouettes / color fills such as STEC mode 1).
@fragment
fn fs_fill(i: VsOut) -> @location(0) vec4<f32> {
    let t = sample_img(i);
    let a = t.a * i.color.a;
    // the fill is baked into the texture's rgb, so the haze intermediate premultiplies it too
    let k = select(1.0, t.a, i.hx.w > 0.5);
    return vec4<f32>(i.color.rgb * k * a, a);
}

// Single-channel coverage texture (glyph atlas) tinted by vertex color. The Switch text writer renders its
// glyphs into an offscreen texture and composites that (sub_C6A58 -> sub_D4A38 / sub_D56F8): the colour keeps
// the coverage c while the alpha builds up to 1 - (1 - c)^2, so dark text on a light ground comes out heavier
// and light text on a dark ground stays as is (Switch: flowchart panel / chapter name / music list).
@fragment
fn fs_text(i: VsOut) -> @location(0) vec4<f32> {
    // exfont glyphs too: plain bilinear at any scale (Switch, nearest-scaled captures: the edged dash at 45.6 px,
    // a run stretched 2.4 x and a ruby's dash at 0.52 x pixel-exact)
    let c = textureSample(tex0, samp, i.uv).r;
    // hy.z = 1 (an edged glyph cache's glyphs, the cell the filled outline less half the outline alone, and
    // exfont glyphs with their own border: the edged dash and box, the inline glyph tags' edged set):
    // half coverage is the border, drawn opaque in the edge colour (0xRRGGBB in hx.x, black by default), full
    // coverage the fill in the vertex colour. Switch, the edged dash over a 93 grey: rows 14 166 255 155 19 and
    // a column of 64 28 13 30 65 at each end (the padded quad's half texel of the sheet's empty gap). nn::font's
    // border program (font_BuildinShader, main.elf's embedded BNSH, fragment variant 2) only starts the fill at
    // 0.54 (its c1[0x0]): below that it is all border, so a fill fringe under ~8% coverage stays edge colour
    if i.hy.z > 0.5 {
        let a = clamp(2.0 * c, 0.0, 1.0);
        let f = select(0.0, clamp(2.0 * c - 1.0, 0.0, 1.0), c >= 0.54);
        let e = u32(i.hx.x);
        let edge = vec3<f32>(f32((e >> 16u) & 255u), f32((e >> 8u) & 255u), f32(e & 255u)) / 255.0;
        return vec4<f32>((i.color.rgb * f + edge * (a - f)) * i.color.a, a * i.color.a);
    }
    return vec4<f32>(i.color.rgb * c * i.color.a, c * (2.0 - c) * i.color.a);
}

// Monochrome (grayscale) sprite.
@fragment
fn fs_mono(i: VsOut) -> @location(0) vec4<f32> {
    let t = sample_img(i);
    let l = dot(t.rgb, vec3<f32>(0.299, 0.587, 0.114));
    let a = t.a * i.color.a;
    return vec4<f32>(vec3<f32>(l) * i.color.rgb * a, a);
}

// Negative sprite.
@fragment
fn fs_nega(i: VsOut) -> @location(0) vec4<f32> {
    let t = sample_img(i);
    let a = t.a * i.color.a;
    return vec4<f32>((vec3<f32>(1.0) - t.rgb) * i.color.rgb * a, a);
}

// ---- transitions: old frame (tex0) -> new frame (tex1) through a rule image (tex2)

struct TransUniform {
    // x: progress 0..1, y: rule flags (`rule_at`), z: use rule (0/1)
    params: vec4<f32>,
    // backdrop blends: rule progress (< 0: no rule), rule flags
    params2: vec4<f32>,
};

@group(0) @binding(2) var tex1: texture_2d<f32>;
@group(0) @binding(3) var tex2: texture_2d<f32>;
@group(0) @binding(4) var<uniform> tu: TransUniform;

// The rule value at screen uv: flags bit 0 / 1 mirror it in x / y (STTR's b, c3[0x3c]), bit 2 reads 1 - rule.
fn rule_at(uv: vec2<f32>, flags: f32) -> f32 {
    let f = u32(flags + 0.5);
    var q = uv;
    if ((f & 1u) != 0u) { q.x = 1.0 - q.x; }
    if ((f & 2u) != 0u) { q.y = 1.0 - q.y; }
    let r = textureSample(tex2, samp, q).r;
    return select(r, 1.0 - r, (f & 4u) != 0u);
}

// nexl_nxshader_transition (c3[0x30] progress, c3[0x34] = soft * width / 1920 in 0..255 rule units, soft from
// the flag word's bit 3 up: Tsukihime's 128, Witch on the Holy Night's FADS / STTR value): the edge sits at
// (255 + 2v) p - v and fades over v with a smoothstep; visible where the rule is below it.
fn rule_mask(r: f32, p: f32, flags: f32) -> f32 {
    let v = f32(u32(flags + 0.5) >> 3u) / 255.0;
    let t = clamp((r - ((1.0 + 2.0 * v) * p - v) + 0.5 * v) / v, 0.0, 1.0);
    return 1.0 - t * t * (3.0 - 2.0 * t);
}

@fragment
fn fs_transition(i: VsOut) -> @location(0) vec4<f32> {
    let a = textureSample(tex0, samp, i.uv);
    let b = textureSample(tex1, samp, i.uv);
    var t = tu.params.x;
    if (tu.params.z > 0.5) {
        t = rule_mask(rule_at(i.uv, tu.params.y), tu.params.x, tu.params.y);
    }
    return vec4<f32>(mix(a.rgb, b.rgb, t), 1.0);
}

// Sprite masked by a screen-space rule image: params = (progress, rule flags, target w, target h).
@fragment
fn fs_rule(i: VsOut) -> @location(0) vec4<f32> {
    let t = sample_img(i);
    let m = rule_mask(rule_at(i.pos.xy / tu.params.zw, tu.params.y), tu.params.x, tu.params.y);
    let a = t.a * i.color.a * m;
    return vec4<f32>(t.rgb * i.color.rgb * a, a);
}

// One pass of a separable gaussian blur: params = (step x, step y in px, sigma, half tap count), params2.x = mode.
// As sub_A0A90 / sub_9CCD0 build it: taps -n..n (n = ceil(3 sigma)), each weighted exp(-k^2 / 2 sigma^2) /
// sqrt(2 pi sigma^2) and summed additively, not renormalised.
// Mode 0 (overseas sub_9CCD0's meshes): the taps offset the uv, edge texels clamp, alpha 1.
// Modes 1 / 2 (sub_A0A90): each tap is a full-target quad shifted by k steps into a cleared target, colour alpha w
// (sub_BACF8(.., 3, w)), blend state 1 (colour SrcAlpha / One, alpha One / One), so pixels a shifted quad leaves
// uncovered take nothing from it. Mode 1 is shader 3 fulladd (texture * colour: rgb += rgb a w, a += a w); mode 2
// is shader 51 whiteness (white, texture alpha * colour alpha: rgb = a += a w), its coverage read from the red
// channel (knife's text targets hold white premultiplied by coverage and no alpha; a pass's output has r = a).
@fragment
fn fs_blur(i: VsOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let sigma = max(tu.params.z, 0.01);
    let n = i32(tu.params.w);
    let mode = i32(tu.params2.x + 0.5);
    let norm = sqrt(2.0 * sigma * sigma * 3.1416);
    var acc = vec4<f32>(0.0);
    for (var k = -n; k <= n; k++) {
        let x = f32(k);
        let w = exp(-x * x / (2.0 * sigma * sigma)) / norm;
        if (mode == 0) {
            acc += vec4<f32>(textureSampleLevel(tex0, samp, i.uv + tu.params.xy / dims * x, 0.0).rgb * w, 0.0);
            continue;
        }
        let p = i.pos.xy - tu.params.xy * x;
        if (any(p < vec2<f32>(0.0)) || any(p > dims)) {
            continue;
        }
        let t = textureSampleLevel(tex0, samp, p / dims, 0.0);
        if (mode == 1) {
            acc += vec4<f32>(t.rgb * t.a, t.a) * w;
        } else {
            acc += vec4<f32>(t.r * w);
        }
    }
    if (mode == 0) {
        return vec4<f32>(acc.rgb, 1.0);
    }
    return clamp(acc, vec4<f32>(0.0), vec4<f32>(1.0));
}

// pre_alter (shader 44) as sub_A3180 builds a sprite texture without a grade: pow(x, 1 / 2.2) on the sample
// (an sRGB source decodes first), alpha kept; the sRGB target encodes the result. hx.xy: the source's last
// texel centre, where its sampling clamps (`Renderer::bake`).
@fragment
fn fs_alter(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex0, samp, min(i.uv, i.hx.xy));
    return vec4<f32>(pow(t.rgb, vec3<f32>(1.0 / 2.2)), t.a);
}

// The bake's resampling alone, for texels the CPU already took through pre_alter (`upload_baked`).
@fragment
fn fs_alter_copy(i: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex0, samp, min(i.uv, i.hx.xy));
}

// The final blit (sub_D7BD0): shader noalpha_srgb raises the frame to 2.2 and the sRGB swapchain encodes it.
fn srgb_encode(l: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(l, vec3<f32>(1.0 / 2.4)) - 0.055, l * 12.92, l <= vec3<f32>(0.0031308));
}

@fragment
fn fs_present(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex0, samp, i.uv);
    return vec4<f32>(srgb_encode(pow(t.rgb, vec3<f32>(2.2))), 1.0);
}

@fragment
fn fs_present_srgb(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex0, samp, i.uv);
    return vec4<f32>(pow(t.rgb, vec3<f32>(2.2)), 1.0);
}

// Backdrop-reading layer blends (the exe's nexl_nxshader_ps_* family, blend state 7): tex0 = sprite,
// tex1 = copy of the target taken before this quad, tex2 = optional rule mask.
// params = (op, fill, target w, target h), params2 = (rule progress or -1, rule flags, noalpha, -).
// Formulas follow the shader disassembly; s = texture rgb * colour rgb, a = texture alpha * colour alpha.
fn ps_mix(d: vec3<f32>, r: vec3<f32>, a: f32) -> vec3<f32> {
    return d + a * (r - d);
}

@fragment
fn fs_ps(i: VsOut) -> @location(0) vec4<f32> {
    var t = sample_img(i);
    // params2.z: the source drawn through noalpha first (alpha 1)
    if (tu.params2.z > 0.5) { t.a = 1.0; }
    let d4 = textureLoad(tex1, vec2<i32>(i.pos.xy), 0);
    let d = d4.rgb;
    var s = t.rgb * i.color.rgb;
    if (tu.params.y > 0.5) { s = i.color.rgb; }
    var ca = i.color.a;
    if (tu.params2.x >= 0.0) {
        ca *= rule_mask(rule_at(i.pos.xy / tu.params.zw, tu.params2.y), tu.params2.x, tu.params2.y);
    }
    let a = t.a * ca;
    let one = vec3<f32>(1.0);
    let zero = vec3<f32>(0.0);
    let op = u32(tu.params.x + 0.5);
    // the alpha-weighted shaders leave the backdrop, alpha included, alone where a = 0 (their `$p0 exit`)
    if (a <= 0.0 && (op <= 15u || op == 19u || op == 20u || op == 23u || op == 24u)) {
        return d4;
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
        // normal: the backdrop premultiplied by its own alpha
        case 19u: { o = ps_mix(d * d4.a, s, a); }
        case 20u: { o = ps_mix(d, min(d, s), ca); }
        case 21u: {
            if (ca <= 0.0) { return d4; }
            o = ps_mix(d, max(d, s), ca);
        }
        case 22u: { o = ps_mix(d, one - (one - d) * (one - s * ca), ca); }
        case 23u: { o = min(d * s + (1.0 - a) * d, one); }
        case 24u: { o = s; }
        default: {}
    }
    return vec4<f32>(o, 1.0);
}
