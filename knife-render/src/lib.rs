//! wgpu renderer: textures, a quad batcher with blend-mode pipelines, render targets,
//! rule transitions and a letterboxed present.
//!
//! The game lays everything out on a 1920x1080 canvas (`CANVAS_W` x `CANVAS_H` units), rendered at the
//! render scale (1, as the Switch draws, or with `Renderer::set_follow_window` the window's size) and then
//! scaled to the window.

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

pub const CANVAS_W: u32 = 1920;
pub const CANVAS_H: u32 = 1080;
/// Every surface the exe draws into (sub_A5C10's backbuffer / temp buffer / thumbnail / saveicon surfaces, the
/// capture, movie and display surfaces) takes sub_CD5F8's format, 2822 = nn::gfx R8_G8_B8_A8_UnormSrgb: the
/// blend maths runs on the decoded levels as before, but a target stores them sRGB-encoded, finer in the dark
/// and coarser in the light than 8-bit levels.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// sub_A3180's altered textures (see `Renderer::upload_altered`)
const ALTERED_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    /// STRA haze (nexl_nxshader_haze): x and y each (amplitude in uv, waves per uv.y, phase, unused)
    pub haze: [[f32; 4]; 2],
    /// soft clip mask (see `SoftClip`): inner rect, (pivot, sin, cos), (radius, LUT row, texels, -);
    /// radius 0 = no mask
    pub soft: [[f32; 4]; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Blend {
    #[default]
    Alpha,
    Add,
    Sub,
    Multiply,
    Screen,
    /// Replace destination (no blending).
    Opaque,
    /// A blend computed in the shader from a copy of the target taken just before the quad is drawn (the
    /// exe's blend state 7, which captures the framebuffer per draw).
    Ps(PsOp),
    /// Normal (premultiplied) colour with the source alpha added to the target's: the exe's blend state 0 (colour
    /// SrcAlpha / OneMinusSrcAlpha, alpha One / One), which a draw into a cleared group surface needs so the
    /// surface keeps the drawn alpha (`Alpha` leaves the target's alpha alone).
    Over,
    /// Colour kept, source alpha added to the target's (the exe's blend state 0 alpha One / One, for the layer
    /// group mask sub_9FE00 / sub_A4BC0 accumulate).
    Footprint,
    /// Colour kept, the target's alpha multiplied by the source alpha (the group mask applied to the group
    /// pixels: sub_37EA0's exalpha draw).
    MaskAlpha,
}

/// Backdrop-reading blends of the exe's layer shaders (shader/nexl_nxshader_*.bnsh). With s = texture rgb *
/// colour rgb, a = texture alpha * colour alpha and d = the backdrop, each writes an opaque colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PsOp {
    /// ps_add: mix(d, min(d + s, 1), a)
    Add = 1,
    /// ps_darken: mix(d, min(d, s), a)
    Darken,
    /// ps_lighten: mix(d, max(d, s), a)
    Lighten,
    /// ps_sub: mix(d, max(d + s - 1, 0), a)
    LinearBurn,
    /// ps_burn: mix(d, s > 0 ? max(1 - (1 - d) / s, 0) : 0, a)
    Burn,
    /// ps_dodge: mix(d, 1 - s > 0 ? min(d / (1 - s), 1) : 1, a)
    Dodge,
    /// dodge / ps_dodge5: d > 0 ? min(d / max(1 - a s, 0), 1) : 0
    DodgeAlpha,
    /// differnce: mix(d, |d - s|, a)
    Difference,
    /// differnce5: |d - a s|
    DifferenceAlpha,
    /// exclusion: mix(d, d + s - 2 d s, a)
    Exclusion,
    /// hardlight: mix(d, s < 0.5 ? 2 d s : 1 - 2 (1 - s)(1 - d), a)
    HardLight,
    /// hardlight_ex: hardlight of min(s / texture alpha, 1)
    HardLightEx,
    /// overlay: mix(d, d < 0.5 ? 2 d s : 1 - 2 (1 - s)(1 - d), a)
    Overlay,
    /// softlight: mix(d, s < 0.5 ? d^(2 - 2 s) : d^(1 / (2 s)), a)
    SoftLight,
    /// sub: max(d - a (1 - s), 0)
    Sub,
    /// add (ignores texture alpha): min(d + s * colour alpha, 1)
    AddNoAlpha,
    /// sub_noalpha (ignores texture alpha): max(d - colour alpha (1 - s), 0)
    SubNoAlpha,
    /// noalpha: s, opaque
    NoAlpha,
    /// normal: mix(d * backdrop alpha, s, a), opaque (where a = 0 the backdrop, alpha included, stays)
    Normal,
    /// darken (Witch on the Holy Night's STBR 4): mix(d, min(d, s), colour alpha) where a > 0
    DarkenColorAlpha,
    /// lighten (STBR 6; texture alpha ignored): mix(d, max(d, s), colour alpha) while the colour alpha > 0
    LightenColorAlpha,
    /// screen (STBR 9; texture alpha ignored): mix(d, 1 - (1 - d)(1 - s ca), ca), ca the colour alpha
    ScreenColorAlpha,
    /// the default shader through blend state 3 (DstColor / 1-SrcAlpha, STBR 7): min(d s + (1 - a) d, 1)
    /// where a > 0
    MultiplyDst,
    /// the default shader with blending off (blend state 7, STBR 26 / 29): s where a > 0
    Copy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Shade {
    #[default]
    Sprite,
    Fill,
    Text,
    /// glyph coverage with its rim composited in one pass (fill over the rim taps), so a faded glyph blends
    /// once like nn::font's bordered glyph; `Quad::haze` = [uv clamp rect, (u, v per canvas px, rim alpha, rim
    /// colour 0xRRGGBB: 0 black)]
    RimText,
    Mono,
    Nega,
    /// The texture's rgb with alpha 1 (shader noalpha: the exe draws a clipped layer of a layer group this way)
    NoAlpha,
    /// The texture's rgb with the colour's alpha alone (shader 17 forcealpha)
    ForceAlpha,
}

/// How a `Renderer::blur` pass takes its taps (see fs_blur).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlurMode {
    /// Taps offset in uv, edges clamped, rgb only (overseas sub_9CCD0)
    Uv = 0,
    /// Shifted quads of shader 3 fulladd (sub_A0A90 on a picture)
    FullAdd = 1,
    /// Shifted quads of shader 51 whiteness (sub_A0A90 on a coverage texture)
    White = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PipeKey {
    Sprite(Shade, Blend),
    /// sprite masked by a screen-space rule image (per-layer rule fades)
    Rule(Blend),
    /// backdrop-reading blend (sprite, backdrop copy, rule mask)
    Ps,
    Transition,
    Blur,
    /// canvas -> window through the Switch's output curve (see `output_level`)
    Present,
}

/// The level the Switch displays for a frame level `v`: the final blit (sub_D7BD0) draws the frame target into
/// the swapchain (sub_D683C: format 2822, R8G8B8A8 sRGB) with shader 1, noalpha_srgb (rgb^2.2), and the sRGB
/// target encodes the result, so dark levels sink (12 -> 4, 21 -> 13, 63 -> 61) while 90..255 barely move.
pub fn output_level(v: u8) -> u8 {
    output(f32::from(v) / 255.0)
}

/// The level the Switch displays for a stored (sRGB-encoded) target byte, see `TARGET_FORMAT`.
pub fn target_level(e: u8) -> u8 {
    let e = f32::from(e) / 255.0;
    output(if e <= 0.040_45 { e / 12.92 } else { ((e + 0.055) / 1.055).powf(2.4) })
}

fn output(v: f32) -> u8 {
    let l = v.powf(2.2);
    let e = if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
    (e * 255.0).round().clamp(0.0, 255.0) as u8
}

/// The clip inset mask (sub_32E30): a rounded rectangle whose edge band of `radius` target pixels fades out
/// along the radial profile of an alphagradation texture (BORDER_PARTS, LUT `row`, `texels` = its radius).
/// `inner` (x0, y0, x1, y1) is relative to `pivot` before the rectangle is turned by `rot` (radians).
#[derive(Clone, Copy, Debug)]
pub struct SoftClip {
    pub inner: [f32; 4],
    pub pivot: [f32; 2],
    pub rot: f32,
    pub radius: f32,
    pub row: u32,
    pub texels: u32,
    /// alpha inside the inner rectangle
    pub fill: f32,
}

impl SoftClip {
    /// The shader's values for a target of `k` pixels per unit.
    fn vertex(&self, k: f32) -> [[f32; 4]; 3] {
        let (s, c) = self.rot.sin_cos();
        [
            self.inner.map(|v| v * k),
            [self.pivot[0] * k, self.pivot[1] * k, s, c],
            [self.radius * k, self.row as f32, self.texels as f32, 1.0 - self.fill],
        ]
    }
}

/// Rows of the soft clip LUT: alpha along the middle row of each alphagradation texture, edge first.
pub const SOFT_LUT_ROWS: u32 = 18;
pub const SOFT_LUT_W: u32 = 257;

/// Screen-space rule mask for a quad (nexl_nxshader_transition): visible where the rule value is below the
/// progress, over a smoothstep 128 / 255 wide. `flip` is STTR's b (bit 0 mirrors the rule in x, bit 1 in y);
/// `invert` reads 1 - rule (the sprite draws of sub_9FE00, whose c3[0x38] is +1).
#[derive(Clone)]
pub struct RuleMask {
    pub tex: TexRef,
    pub progress: f32,
    pub flip: u8,
    pub invert: bool,
    /// The edge width c3[0x34] in rule units at 1920 wide: Tsukihime's 128, Witch on the Holy Night's
    /// dword_612C88 (FADS / STTR)
    pub soft: f32,
}

impl RuleMask {
    pub fn new(tex: TexRef, progress: f32) -> Self {
        Self { tex, progress, flip: 0, invert: false, soft: 128.0 }
    }

    /// The flag word the shaders read: flip bits, invert at bit 2.
    fn flags(&self) -> f32 {
        rule_flags(self.flip, self.invert, self.soft)
    }
}

/// Flip bits, invert at bit 2, the edge width (1..255) from bit 3.
fn rule_flags(flip: u8, invert: bool, soft: f32) -> f32 {
    ((flip & 3) as u32 | (invert as u32) << 2 | (soft.clamp(1.0, 255.0) as u32) << 3) as f32
}

pub struct GpuTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    pub width: u32,
    pub height: u32,
}

impl GpuTexture {
    /// UV coordinates of the bottom-right corner of the logical image (block-compressed
    /// textures are padded to whole blocks).
    pub fn uv_max(&self) -> [f32; 2] {
        let s = self.texture.size();
        [self.width as f32 / s.width as f32, self.height as f32 / s.height as f32]
    }
}

pub type TexRef = Arc<GpuTexture>;

/// An offscreen render target that can also be sampled.
#[derive(Clone)]
pub struct RenderTarget {
    pub tex: TexRef,
    /// The size quads drawn into it are laid out in (`Renderer::draw`): its pixels, or for a canvas target
    /// (`Renderer::create_canvas_target`) its units, which the render scale maps to more or fewer pixels
    pub units: [f32; 2],
}

impl RenderTarget {
    /// A target of `tex` laid out in its own pixels.
    pub fn new(tex: TexRef) -> Self {
        let units = [tex.width as f32, tex.height as f32];
        Self { tex, units }
    }

    /// A target of `tex` laid out in `units`.
    pub fn with_units(tex: TexRef, units: [f32; 2]) -> Self {
        Self { tex, units }
    }

    /// Its pixels per unit.
    pub fn scale(&self) -> f32 {
        self.tex.width as f32 / self.units[0]
    }
}

#[derive(Clone)]
pub struct Quad {
    pub tex: TexRef,
    /// Corner positions in canvas pixels: top-left, top-right, bottom-right, bottom-left.
    pub pos: [[f32; 2]; 4],
    /// Texture coordinates for the same corners.
    pub uv: [[f32; 2]; 4],
    pub color: [f32; 4],
    pub blend: Blend,
    pub shade: Shade,
    /// Optional convex clip polygon (canvas pixels, any winding).
    pub clip: Option<[[f32; 2]; 4]>,
    pub rule: Option<RuleMask>,
    /// STRA haze parameters (see `Vertex::haze`); zero amplitude leaves the texture unshifted.
    pub haze: [[f32; 4]; 2],
    pub soft: Option<SoftClip>,
}

/// Clip a convex polygon against a convex clip polygon (Sutherland-Hodgman).
/// Each vertex carries (x, y, u, v).
fn clip_poly(subject: Vec<[f32; 4]>, clip: &[[f32; 2]; 4]) -> Vec<[f32; 4]> {
    // orientation of the clip polygon
    let area: f32 = (0..4)
        .map(|i| {
            let (a, b) = (clip[i], clip[(i + 1) % 4]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum();
    let sign = if area >= 0.0 { 1.0 } else { -1.0 };
    let mut out = subject;
    for i in 0..4 {
        if out.is_empty() {
            break;
        }
        let (a, b) = (clip[i], clip[(i + 1) % 4]);
        let side = |p: &[f32; 4]| sign * ((b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]));
        let input = std::mem::take(&mut out);
        for k in 0..input.len() {
            let cur = input[k];
            let prev = input[(k + input.len() - 1) % input.len()];
            let (dc, dp) = (side(&cur), side(&prev));
            if dc >= 0.0 {
                if dp < 0.0 {
                    let t = dp / (dp - dc);
                    out.push(std::array::from_fn(|j| prev[j] + (cur[j] - prev[j]) * t));
                }
                out.push(cur);
            } else if dp >= 0.0 {
                let t = dp / (dp - dc);
                out.push(std::array::from_fn(|j| prev[j] + (cur[j] - prev[j]) * t));
            }
        }
    }
    out
}

impl Quad {
    /// The same quad drawn into a target `s` times the canvas size: positions, clip and soft clip scaled.
    pub fn scaled(mut self, s: f32) -> Self {
        let sc = |p: &mut [f32; 2]| *p = [p[0] * s, p[1] * s];
        self.pos.iter_mut().for_each(sc);
        if let Some(c) = &mut self.clip {
            c.iter_mut().for_each(sc);
        }
        if let Some(m) = &mut self.soft {
            m.inner = m.inner.map(|v| v * s);
            sc(&mut m.pivot);
            m.radius *= s;
        }
        self
    }

    /// Axis-aligned quad covering `rect` (x, y, w, h) in canvas pixels with the whole texture.
    pub fn rect(tex: TexRef, rect: [f32; 4], color: [f32; 4]) -> Self {
        let [x, y, w, h] = rect;
        Self {
            tex,
            pos: [[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
            uv: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            color,
            blend: Blend::Alpha,
            shade: Shade::Sprite,
            clip: None,
            rule: None,
            haze: [[0.0; 4]; 2],
            soft: None,
        }
    }

    pub fn with_uv_rect(mut self, u0: f32, v0: f32, u1: f32, v1: f32) -> Self {
        self.uv = [[u0, v0], [u1, v0], [u1, v1], [u0, v1]];
        self
    }
}

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    /// None while the window has none (`suspend`: Android takes the surface away in the background)
    surface: Option<wgpu::Surface<'static>>,
    config: wgpu::SurfaceConfiguration,
    pub bc_supported: bool,
    sampler: wgpu::Sampler,
    sprite_layout: wgpu::BindGroupLayout,
    trans_layout: wgpu::BindGroupLayout,
    module: wgpu::ShaderModule,
    sprite_pl: wgpu::PipelineLayout,
    trans_pl: wgpu::PipelineLayout,
    pipelines: HashMap<(PipeKey, wgpu::TextureFormat), wgpu::RenderPipeline>,
    /// per target size: the copy of the target that backdrop blends read
    backdrops: HashMap<(u32, u32), RenderTarget>,
    pub white: TexRef,
    soft_lut: wgpu::Texture,
    lut_group: wgpu::BindGroup,
    /// pre_alter without a grade, into an sRGB texture (`upload_altered`)
    alter: wgpu::RenderPipeline,
    /// The window is fully covered or minimised: `present` submits the frame's work but takes no surface
    /// texture (a hidden macOS window hands drawables back at once or after a second's stall)
    pub hidden: bool,
    /// Pixels per canvas unit of the canvas targets (`create_canvas_target`)
    scale: f32,
    /// The scale follows the window (`set_follow_window`)
    follow_window: bool,
}

pub enum Upload<'a> {
    Rgba8(&'a [u8]),
    Bc7(&'a [u8]),
    Bc1(&'a [u8]),
    Bc3(&'a [u8]),
    R8(&'a [u8]),
}

impl Renderer {
    pub fn new(window: Arc<impl wgpu::DisplayAndWindowHandle + 'static>, width: u32, height: u32) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            flags: wgpu::InstanceFlags::default(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance.create_surface(window).context("creating surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .context("no suitable GPU adapter")?;
        tracing::info!("adapter: {:?}", adapter.get_info());
        let bc_supported = adapter.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("knife"),
            required_features: if bc_supported {
                wgpu::Features::TEXTURE_COMPRESSION_BC
            } else {
                wgpu::Features::empty()
            },
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .context("creating device")?;

        let format = surface_format(&surface.get_capabilities(&adapter));
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            color_space: Default::default(),
        };
        surface.configure(&device, &config);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let samp_entry = wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let sprite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite"),
            entries: &[tex_entry(0), samp_entry],
        });
        let trans_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transition"),
            entries: &[
                tex_entry(0),
                samp_entry,
                tex_entry(2),
                tex_entry(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let lut_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("soft lut"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let soft_lut = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("soft lut"),
            size: wgpu::Extent3d { width: SOFT_LUT_W, height: SOFT_LUT_ROWS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let lut_view = soft_lut.create_view(&Default::default());
        let lut_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("soft lut"),
            layout: &lut_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&lut_view) }],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("knife shaders"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let sprite_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprite"),
            bind_group_layouts: &[Some(&sprite_layout), Some(&lut_layout)],
            immediate_size: 0,
        });
        let trans_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transition"),
            bind_group_layouts: &[Some(&trans_layout), Some(&lut_layout)],
            immediate_size: 0,
        });

        let white = upload_texture(&device, &queue, &sprite_layout, &sampler, 1, 1, Upload::Rgba8(&[255, 255, 255, 255]), false, "white");
        let alter = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fs_alter"),
            layout: Some(&sprite_pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4],
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_alter"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: ALTERED_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Ok(Self {
            device,
            queue,
            instance,
            adapter,
            surface: Some(surface),
            config,
            bc_supported,
            sampler,
            sprite_layout,
            trans_layout,
            module,
            sprite_pl,
            trans_pl,
            pipelines: HashMap::new(),
            backdrops: HashMap::new(),
            white,
            soft_lut,
            lut_group,
            alter,
            hidden: false,
            scale: 1.0,
            follow_window: false,
        })
    }

    /// Render the canvas at the window's resolution (the largest 16:9 fit, never under 1080p) rather than at
    /// 1920x1080 as the Switch does. The canvas targets made from now on take the scale (`create_canvas_target`;
    /// `canvas_fits` tells the ones made before a change).
    pub fn set_follow_window(&mut self, on: bool) {
        self.follow_window = on;
        self.fit_scale();
    }

    fn fit_scale(&mut self) {
        self.scale = if self.follow_window {
            let fit = (self.config.width as f32 / CANVAS_W as f32).min(self.config.height as f32 / CANVAS_H as f32);
            let max = self.device.limits().max_texture_dimension_2d as f32 / CANVAS_W as f32;
            fit.min(max).max(1.0)
        } else {
            1.0
        };
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The pixel size of a canvas target of `w` x `h` units.
    fn canvas_px(&self, w: u32, h: u32) -> (u32, u32) {
        ((w as f32 * self.scale).round().max(1.0) as u32, (h as f32 * self.scale).round().max(1.0) as u32)
    }

    /// A render target laid out in `w` x `h` canvas units, `scale` pixels to the unit.
    pub fn create_canvas_target(&self, w: u32, h: u32, label: &str) -> RenderTarget {
        let (pw, ph) = self.canvas_px(w, h);
        RenderTarget::with_units(self.create_target(pw, ph, label).tex, [w as f32, h as f32])
    }

    /// Whether `tex` has the pixels of a `w` x `h` unit canvas target at the current scale.
    pub fn canvas_fits(&self, tex: &GpuTexture, w: u32, h: u32) -> bool {
        (tex.width, tex.height) == self.canvas_px(w, h)
    }

    /// Make canvas target `rt` again when the scale has changed since, its picture scaled into the new one.
    pub fn refit(&mut self, encoder: &mut wgpu::CommandEncoder, rt: &mut RenderTarget, label: &str) {
        let [w, h] = rt.units;
        if self.canvas_fits(&rt.tex, w as u32, h as u32) {
            return;
        }
        let new = self.create_canvas_target(w as u32, h as u32, label);
        let q = Quad { blend: Blend::Opaque, ..Quad::rect(rt.tex.clone(), [0.0, 0.0, w, h], [1.0; 4]) };
        self.draw(encoder, &new, Some([0.0; 4]), &[q]);
        *rt = new;
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.config);
        }
        self.fit_scale();
    }

    /// Let go of the window's surface (Android: the app is going to the background, where its native window is
    /// destroyed); `present` draws nothing until `resume`.
    pub fn suspend(&mut self) {
        self.surface = None;
    }

    /// A new surface for the window after `suspend`, `width` x `height` pixels.
    pub fn resume(&mut self, window: Arc<impl wgpu::DisplayAndWindowHandle + 'static>, width: u32, height: u32) -> Result<()> {
        // (two surfaces for one native window do not coexist)
        self.surface = None;
        let surface = self.instance.create_surface(window).context("creating surface")?;
        self.config.format = surface_format(&surface.get_capabilities(&self.adapter));
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        surface.configure(&self.device, &self.config);
        self.surface = Some(surface);
        self.fit_scale();
        Ok(())
    }

    fn bind_group_for(&self, view: &wgpu::TextureView) -> wgpu::BindGroup {
        make_bind_group(&self.device, &self.sprite_layout, &self.sampler, view)
    }

    /// Fill the soft clip LUT: per row, the alpha of an alphagradation texture's middle row from the edge to
    /// the centre (radius + 1 texels, at most `SOFT_LUT_W`).
    pub fn set_soft_lut(&self, rows: &[Vec<u8>]) {
        let mut data = vec![0u8; (SOFT_LUT_W * SOFT_LUT_ROWS) as usize];
        for (i, row) in rows.iter().take(SOFT_LUT_ROWS as usize).enumerate() {
            let n = row.len().min(SOFT_LUT_W as usize);
            data[i * SOFT_LUT_W as usize..][..n].copy_from_slice(&row[..n]);
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.soft_lut,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(SOFT_LUT_W), rows_per_image: None },
            wgpu::Extent3d { width: SOFT_LUT_W, height: SOFT_LUT_ROWS, depth_or_array_layers: 1 },
        );
    }

    /// Upload a texture. `width`/`height` are the logical size; block data must cover
    /// whole 4x4 blocks.
    pub fn upload(&self, width: u32, height: u32, data: Upload, label: &str) -> TexRef {
        upload_texture(&self.device, &self.queue, &self.sprite_layout, &self.sampler, width, height, data, false, label)
    }

    /// Upload RGBA8 pixels already baked the way pre_alter would (sRGB-encoded, see `upload_altered`).
    pub fn upload_baked(&self, width: u32, height: u32, data: &[u8], label: &str) -> TexRef {
        upload_texture(&self.device, &self.queue, &self.sprite_layout, &self.sampler, width, height, Upload::Rgba8(data), true, label)
    }

    /// Upload an image the way sub_A3180 builds every sprite texture: the source (sampled through an sRGB view
    /// when `srgb`, as the exe's "Jpeg Tex" / "Png Tex" and the BC7 sRGB art are) is drawn once through
    /// pre_alter, which starts with pow(x, 1 / 2.2), into an "altered texture" of the graphics config's format
    /// 0 (sub_CD5F8 -> 2822, R8G8B8A8 sRGB); draws sample that copy. The output curve then shows an opaque
    /// texel at its file value.
    pub fn upload_altered(&self, width: u32, height: u32, data: Upload, srgb: bool, label: &str) -> TexRef {
        let src = upload_texture(&self.device, &self.queue, &self.sprite_layout, &self.sampler, width, height, data, srgb, label);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: ALTERED_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let [u, v] = src.uv_max();
        let c = [1.0; 4];
        let vx = |pos: [f32; 2], uv: [f32; 2]| Vertex { pos, uv, color: c, haze: [[0.0; 4]; 2], soft: [[0.0; 4]; 3] };
        let verts = [
            vx([-1.0, 1.0], [0.0, 0.0]),
            vx([1.0, 1.0], [u, 0.0]),
            vx([1.0, -1.0], [u, v]),
            vx([-1.0, 1.0], [0.0, 0.0]),
            vx([1.0, -1.0], [u, v]),
            vx([-1.0, -1.0], [0.0, v]),
        ];
        let vbuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("alter"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let mut encoder = self.encoder();
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("alter"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.alter);
            pass.set_bind_group(0, &src.bind_group, &[]);
            pass.set_bind_group(1, &self.lut_group, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.draw(0..6, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        let bind_group = self.bind_group_for(&view);
        Arc::new(GpuTexture {
            texture,
            view,
            bind_group,
            width,
            height,
        })
    }

    /// Update a sub-rectangle of an RGBA8/R8 texture (glyph atlas, movie frames).
    pub fn write_region(&self, tex: &GpuTexture, [x, y, w, h]: [u32; 4], bpp: u32, data: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * bpp),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Empty writable texture (for atlases / movie frames).
    pub fn create_writable(&self, width: u32, height: u32, r8: bool, label: &str) -> TexRef {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: if r8 { wgpu::TextureFormat::R8Unorm } else { wgpu::TextureFormat::Rgba8Unorm },
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind_group = self.bind_group_for(&view);
        Arc::new(GpuTexture {
            texture,
            view,
            bind_group,
            width,
            height,
        })
    }

    pub fn create_target(&self, width: u32, height: u32, label: &str) -> RenderTarget {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind_group = self.bind_group_for(&view);
        RenderTarget::new(Arc::new(GpuTexture {
            texture,
            view,
            bind_group,
            width,
            height,
        }))
    }

    fn pipeline(&mut self, key: PipeKey, format: wgpu::TextureFormat) -> &wgpu::RenderPipeline {
        if !self.pipelines.contains_key(&(key, format)) {
            let (fs, blend, layout) = match key {
                PipeKey::Sprite(shade, blend) => (
                    match shade {
                        Shade::Sprite => "fs_sprite",
                        Shade::Fill => "fs_fill",
                        Shade::Text => "fs_text",
                        Shade::RimText => "fs_rim_text",
                        Shade::Mono => "fs_mono",
                        Shade::Nega => "fs_nega",
                        Shade::NoAlpha => "fs_noalpha",
                        Shade::ForceAlpha => "fs_forcealpha",
                    },
                    blend_state(blend),
                    &self.sprite_pl,
                ),
                PipeKey::Rule(blend) => ("fs_rule", blend_state(blend), &self.trans_pl),
                PipeKey::Ps => ("fs_ps", None, &self.trans_pl),
                PipeKey::Transition => ("fs_transition", None, &self.trans_pl),
                PipeKey::Blur => ("fs_blur", None, &self.trans_pl),
                // an sRGB surface encodes on write itself, like the Switch's swapchain
                PipeKey::Present if format.is_srgb() => ("fs_present_srgb", None, &self.sprite_pl),
                PipeKey::Present => ("fs_present", None, &self.sprite_pl),
            };
            let p = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(fs),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &self.module,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4],
                    })],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &self.module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
            self.pipelines.insert((key, format), p);
        }
        &self.pipelines[&(key, format)]
    }

    /// Draw quads into a render target, optionally clearing it first. A quad with a backdrop blend
    /// (`Blend::Ps`) ends the current pass: the target is copied, and the quad is drawn reading that copy.
    pub fn draw(&mut self, encoder: &mut wgpu::CommandEncoder, target: &RenderTarget, clear: Option<[f32; 4]>, quads: &[Quad]) {
        let (tw, th) = (target.tex.width as f32, target.tex.height as f32);
        let [uw, uh] = target.units;
        // the soft clip works in target pixels
        let k = tw / uw;
        let mut verts: Vec<Vertex> = Vec::with_capacity(quads.len() * 6);
        // vertex range of each quad
        let mut ranges: Vec<(u32, u32)> = Vec::with_capacity(quads.len());
        for q in quads {
            let start = verts.len() as u32;
            let mk = |p: [f32; 4]| Vertex {
                pos: [p[0] / uw * 2.0 - 1.0, 1.0 - p[1] / uh * 2.0],
                uv: [p[2], p[3]],
                color: q.color,
                haze: q.haze,
                soft: q.soft.map_or([[0.0; 4]; 3], |m| m.vertex(k)),
            };
            let corners: Vec<[f32; 4]> = (0..4).map(|i| [q.pos[i][0], q.pos[i][1], q.uv[i][0], q.uv[i][1]]).collect();
            let poly = match &q.clip {
                Some(c) => clip_poly(corners, c),
                None => corners,
            };
            for k in 1..poly.len().saturating_sub(1) {
                verts.extend_from_slice(&[mk(poly[0]), mk(poly[k]), mk(poly[k + 1])]);
            }
            ranges.push((start, verts.len() as u32));
        }
        // make sure all pipelines exist before borrowing them in the pass
        for q in quads {
            match (q.blend, &q.rule) {
                (Blend::Ps(_), _) => self.pipeline(PipeKey::Ps, TARGET_FORMAT),
                (_, Some(_)) => self.pipeline(PipeKey::Rule(q.blend), TARGET_FORMAT),
                (_, None) => self.pipeline(PipeKey::Sprite(q.shade, q.blend), TARGET_FORMAT),
            };
        }
        let has_ps = quads.iter().any(|q| matches!(q.blend, Blend::Ps(_)));
        if has_ps && !self.backdrops.contains_key(&(target.tex.width, target.tex.height)) {
            let bd = self.create_target(target.tex.width, target.tex.height, "backdrop");
            self.backdrops.insert((target.tex.width, target.tex.height), bd);
        }
        let backdrop = has_ps.then(|| self.backdrops[&(target.tex.width, target.tex.height)].tex.clone());
        // per-quad bind groups for rule-masked and backdrop quads
        let groups: Vec<Option<wgpu::BindGroup>> = quads
            .iter()
            .map(|q| match (q.blend, &q.rule) {
                (Blend::Ps(op), m) => {
                    let (rule, progress, flags) = match m {
                        Some(m) => (&m.tex.view, m.progress, m.flags()),
                        None => (&self.white.view, -1.0, 0.0),
                    };
                    let fill = if q.shade == Shade::Fill { 1.0 } else { 0.0 };
                    let noalpha = if q.shade == Shade::NoAlpha { 1.0 } else { 0.0 };
                    let bd = backdrop.as_ref().unwrap();
                    Some(self.effect_bind_group(&q.tex.view, &bd.view, rule, [op as u32 as f32, fill, tw, th, progress, flags, noalpha, 0.0]))
                }
                (_, Some(m)) => Some(self.trans_bind_group(&q.tex.view, &self.white.view, &m.tex.view, [m.progress, m.flags(), tw, th])),
                (_, None) => None,
            })
            .collect();
        let vbuf = (!verts.is_empty()).then(|| {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("quads"),
                contents: bytemuck::cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let mut i = 0;
        let mut clear = clear;
        loop {
            // quads up to the next backdrop blend share one pass
            let end = (i..quads.len()).find(|&k| matches!(quads[k].blend, Blend::Ps(_))).unwrap_or(quads.len());
            if end > i || clear.is_some() {
                let mut pass = begin_pass(encoder, target, clear.take());
                if let Some(vbuf) = &vbuf {
                    pass.set_vertex_buffer(0, vbuf.slice(..));
                    self.draw_run(&mut pass, quads, &ranges, &groups, i, end);
                }
            }
            if end == quads.len() {
                break;
            }
            let bd = backdrop.as_ref().unwrap();
            encoder.copy_texture_to_texture(
                target.tex.texture.as_image_copy(),
                bd.texture.as_image_copy(),
                wgpu::Extent3d {
                    width: target.tex.width,
                    height: target.tex.height,
                    depth_or_array_layers: 1,
                },
            );
            let (a, b) = ranges[end];
            if let (Some(vbuf), Some(bg)) = (&vbuf, &groups[end])
                && b > a
            {
                let mut pass = begin_pass(encoder, target, None);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.set_pipeline(&self.pipelines[&(PipeKey::Ps, TARGET_FORMAT)]);
                pass.set_bind_group(0, bg, &[]);
                pass.set_bind_group(1, &self.lut_group, &[]);
                pass.draw(a..b, 0..1);
            }
            i = end + 1;
        }
    }

    /// Draw quads `i..end` (no backdrop blends) into an open pass, batching runs that share texture and state.
    fn draw_run(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        quads: &[Quad],
        ranges: &[(u32, u32)],
        groups: &[Option<wgpu::BindGroup>],
        mut i: usize,
        end: usize,
    ) {
        while i < end {
            let q = &quads[i];
            if let Some(bg) = &groups[i] {
                pass.set_pipeline(&self.pipelines[&(PipeKey::Rule(q.blend), TARGET_FORMAT)]);
                pass.set_bind_group(0, bg, &[]);
                pass.set_bind_group(1, &self.lut_group, &[]);
                let (a, b) = ranges[i];
                if b > a {
                    pass.draw(a..b, 0..1);
                }
                i += 1;
                continue;
            }
            let mut j = i + 1;
            while j < end
                && Arc::ptr_eq(&quads[j].tex, &q.tex)
                && quads[j].blend == q.blend
                && quads[j].shade == q.shade
                && quads[j].rule.is_none()
            {
                j += 1;
            }
            let p = &self.pipelines[&(PipeKey::Sprite(q.shade, q.blend), TARGET_FORMAT)];
            pass.set_pipeline(p);
            pass.set_bind_group(0, &q.tex.bind_group, &[]);
            pass.set_bind_group(1, &self.lut_group, &[]);
            let (a, b) = (ranges[i].0, ranges[j - 1].1);
            if b > a {
                pass.draw(a..b, 0..1);
            }
            i = j;
        }
    }

    /// Full-target transition from `old` to `new`: a crossfade, or through a rule image as `RuleMask` reads it.
    #[allow(clippy::too_many_arguments)]
    pub fn transition(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        dst: &RenderTarget,
        old: &GpuTexture,
        new: &GpuTexture,
        rule: Option<&GpuTexture>,
        progress: f32,
        soft: f32,
    ) {
        self.pipeline(PipeKey::Transition, TARGET_FORMAT);
        let params: [f32; 4] = [progress, rule_flags(0, false, soft), rule.is_some() as u32 as f32, 0.0];
        let rule_view = rule.map(|r| &r.view).unwrap_or(&self.white.view);
        let bg = self.trans_bind_group(&old.view, &new.view, rule_view, params);
        let verts = fullscreen_verts([1.0; 4]);
        let vbuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fullscreen"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let p = &self.pipelines[&(PipeKey::Transition, TARGET_FORMAT)];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("transition"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &dst.tex.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(p);
        pass.set_bind_group(0, &bg, &[]);
        pass.set_bind_group(1, &self.lut_group, &[]);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.draw(0..6, 0..1);
    }

    fn trans_bind_group(
        &self,
        t0: &wgpu::TextureView,
        t1: &wgpu::TextureView,
        t2: &wgpu::TextureView,
        params: [f32; 4],
    ) -> wgpu::BindGroup {
        let [a, b, c, d] = params;
        self.effect_bind_group(t0, t1, t2, [a, b, c, d, 0.0, 0.0, 0.0, 0.0])
    }

    /// Bind group of the effect layout: three textures and two vec4 parameters.
    fn effect_bind_group(
        &self,
        t0: &wgpu::TextureView,
        t1: &wgpu::TextureView,
        t2: &wgpu::TextureView,
        params: [f32; 8],
    ) -> wgpu::BindGroup {
        let ubuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("effect params"),
            contents: bytemuck::cast_slice(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("effect"),
            layout: &self.trans_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(t0),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(t1),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(t2),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: ubuf.as_entire_binding(),
                },
            ],
        })
    }

    /// One separable gaussian blur pass of `src` into `dst` (overwritten) along `dir` (units of `dst` per tap,
    /// `src` having its scale): the Switch's 2 ceil(3 sigma) + 1 taps.
    #[allow(clippy::too_many_arguments)]
    pub fn blur(&mut self, encoder: &mut wgpu::CommandEncoder, src: &GpuTexture, dst: &RenderTarget, dir: [f32; 2], sigma: f32, mode: BlurMode) {
        self.pipeline(PipeKey::Blur, TARGET_FORMAT);
        let half = (sigma * 3.0).ceil();
        let k = dst.scale();
        let params = [dir[0] * k, dir[1] * k, sigma, half, mode as u32 as f32, 0.0, 0.0, 0.0];
        let bg = self.effect_bind_group(&src.view, &self.white.view, &self.white.view, params);
        let verts = fullscreen_verts([1.0; 4]);
        let vbuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fullscreen"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let p = &self.pipelines[&(PipeKey::Blur, TARGET_FORMAT)];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blur"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &dst.tex.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(p);
        pass.set_bind_group(0, &bg, &[]);
        pass.set_bind_group(1, &self.lut_group, &[]);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.draw(0..6, 0..1);
    }

    pub fn copy_target(&self, encoder: &mut wgpu::CommandEncoder, src: &RenderTarget, dst: &RenderTarget) {
        encoder.copy_texture_to_texture(
            src.tex.texture.as_image_copy(),
            dst.tex.texture.as_image_copy(),
            wgpu::Extent3d {
                width: src.tex.width,
                height: src.tex.height,
                depth_or_array_layers: 1,
            },
        );
    }

    pub fn encoder(&self) -> wgpu::CommandEncoder {
        self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None })
    }

    /// Scale `canvas` into the window (letterboxed) through the output curve, then submit and present.
    pub fn present(&mut self, mut encoder: wgpu::CommandEncoder, canvas: &RenderTarget) {
        let Some(surface) = self.surface.as_ref().filter(|_| !self.hidden) else {
            self.queue.submit([encoder.finish()]);
            return;
        };
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                surface.configure(&self.device, &self.config);
                self.queue.submit([encoder.finish()]);
                return;
            }
            _ => {
                self.queue.submit([encoder.finish()]);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let format = self.config.format;
        self.pipeline(PipeKey::Present, format);
        let (sw, sh) = (self.config.width as f32, self.config.height as f32);
        let scale = (sw / CANVAS_W as f32).min(sh / CANVAS_H as f32);
        let (w, h) = (CANVAS_W as f32 * scale, CANVAS_H as f32 * scale);
        let (x, y) = ((sw - w) / 2.0, (sh - h) / 2.0);
        let ndc = |px: f32, py: f32| [px / sw * 2.0 - 1.0, 1.0 - py / sh * 2.0];
        let c = [1.0; 4];
        let v = |p: [f32; 2], uv: [f32; 2]| Vertex { pos: p, uv, color: c, haze: [[0.0; 4]; 2], soft: [[0.0; 4]; 3] };
        let verts = [
            v(ndc(x, y), [0.0, 0.0]),
            v(ndc(x + w, y), [1.0, 0.0]),
            v(ndc(x + w, y + h), [1.0, 1.0]),
            v(ndc(x, y), [0.0, 0.0]),
            v(ndc(x + w, y + h), [1.0, 1.0]),
            v(ndc(x, y + h), [0.0, 1.0]),
        ];
        let vbuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("present"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        {
            let p = &self.pipelines[&(PipeKey::Present, format)];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(p);
            pass.set_bind_group(0, &canvas.tex.bind_group, &[]);
            pass.set_bind_group(1, &self.lut_group, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.draw(0..6, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }

    /// Read a render target's stored bytes back (sRGB-encoded rgb, see `TARGET_FORMAT`; for debugging).
    pub fn read_target(&self, target: &RenderTarget) -> Vec<u8> {
        let (w, h) = (target.tex.width, target.tex.height);
        let row = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.encoder();
        enc.copy_texture_to_buffer(
            target.tex.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range().expect("mapping readback buffer");
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            out.extend_from_slice(&data[(y * row) as usize..(y * row + w * 4) as usize]);
        }
        out
    }
}

/// The surface's format: plain 8-bit and never sRGB, as the Switch draws (compositors treat float, and some 10-bit,
/// surfaces as linear or HDR, which washes the colours out).
fn surface_format(caps: &wgpu::SurfaceCapabilities) -> wgpu::TextureFormat {
    [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm]
        .into_iter()
        .find(|f| caps.formats.contains(f))
        .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
        .unwrap_or(caps.formats[0])
}

fn fullscreen_verts(color: [f32; 4]) -> [Vertex; 6] {
    let v = |x: f32, y: f32, u: f32, w: f32| Vertex {
        pos: [x, y],
        uv: [u, w],
        color,
        haze: [[0.0; 4]; 2],
        soft: [[0.0; 4]; 3],
    };
    [
        v(-1.0, 1.0, 0.0, 0.0),
        v(1.0, 1.0, 1.0, 0.0),
        v(1.0, -1.0, 1.0, 1.0),
        v(-1.0, 1.0, 0.0, 0.0),
        v(1.0, -1.0, 1.0, 1.0),
        v(-1.0, -1.0, 0.0, 1.0),
    ]
}

/// A render pass on `target`, cleared to `clear` or loading what is there.
fn begin_pass<'a>(encoder: &'a mut wgpu::CommandEncoder, target: &'a RenderTarget, clear: Option<[f32; 4]>) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("draw"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &target.tex.view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: match clear {
                    Some([r, g, b, a]) => wgpu::LoadOp::Clear(wgpu::Color {
                        r: r as f64,
                        g: g as f64,
                        b: b as f64,
                        a: a as f64,
                    }),
                    None => wgpu::LoadOp::Load,
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

fn blend_state(b: Blend) -> Option<wgpu::BlendState> {
    use wgpu::{BlendComponent as C, BlendFactor as F, BlendOperation as O};
    let alpha = C {
        src_factor: F::One,
        dst_factor: F::OneMinusSrcAlpha,
        operation: O::Add,
    };
    let color = match b {
        Blend::Opaque | Blend::Ps(_) => return None,
        Blend::Footprint | Blend::MaskAlpha => {
            let keep = C {
                src_factor: F::Zero,
                dst_factor: F::One,
                operation: O::Add,
            };
            let (src_factor, dst_factor) = if b == Blend::Footprint { (F::One, F::One) } else { (F::Zero, F::SrcAlpha) };
            return Some(wgpu::BlendState {
                color: keep,
                alpha: C {
                    src_factor,
                    dst_factor,
                    operation: O::Add,
                },
            });
        }
        Blend::Alpha | Blend::Over => alpha,
        Blend::Add => C {
            src_factor: F::One,
            dst_factor: F::One,
            operation: O::Add,
        },
        Blend::Sub => C {
            src_factor: F::One,
            dst_factor: F::One,
            operation: O::ReverseSubtract,
        },
        Blend::Multiply => C {
            src_factor: F::Dst,
            dst_factor: F::OneMinusSrcAlpha,
            operation: O::Add,
        },
        Blend::Screen => C {
            src_factor: F::One,
            dst_factor: F::OneMinusSrc,
            operation: O::Add,
        },
    };
    let add = matches!(b, Blend::Over);
    Some(wgpu::BlendState {
        color,
        alpha: C {
            src_factor: if add { F::One } else { F::Zero },
            dst_factor: F::One,
            operation: O::Add,
        },
    })
}

fn make_bind_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, view: &wgpu::TextureView) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// Upload a texture. `width`/`height` are the logical size; block data must cover
/// whole 4x4 blocks.
#[allow(clippy::too_many_arguments)]
fn upload_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    width: u32,
    height: u32,
    data: Upload,
    srgb: bool,
    label: &str,
) -> TexRef {
    use wgpu::TextureFormat as F;
    let (format, bytes, block) = match data {
        Upload::Rgba8(d) => (if srgb { F::Rgba8UnormSrgb } else { F::Rgba8Unorm }, d, 1),
        Upload::Bc7(d) => (if srgb { F::Bc7RgbaUnormSrgb } else { F::Bc7RgbaUnorm }, d, 4),
        Upload::Bc1(d) => (if srgb { F::Bc1RgbaUnormSrgb } else { F::Bc1RgbaUnorm }, d, 4),
        Upload::Bc3(d) => (if srgb { F::Bc3RgbaUnormSrgb } else { F::Bc3RgbaUnorm }, d, 4),
        Upload::R8(d) => (F::R8Unorm, d, 1),
    };
    let (pw, ph) = (width.div_ceil(block) * block, height.div_ceil(block) * block);
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: pw,
                height: ph,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytes,
    );
    let view = texture.create_view(&Default::default());
    let bind_group = make_bind_group(device, layout, sampler, &view);
    Arc::new(GpuTexture {
        texture,
        view,
        bind_group,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn output_curve_matches_switch_captures() {
        // train scene 004 levels: knife frame -> Switch screen (Astris, sRGB-converted)
        let f = super::output_level;
        assert_eq!((f(0), f(8), f(12), f(21), f(63), f(255)), (0, 2, 4, 13, 61, 255));
    }
}
