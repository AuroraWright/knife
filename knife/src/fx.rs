//! Menu screen effects: the title logo particles (sub_905B0 / sub_907E0 and sub_90D00 / sub_90E80) and
//! the shared submenu backdrop (sub_4B880 with its sparkles, sub_4C190 / sub_4BD10).
//!
//! The exe steps these once per 60 Hz frame in a 960x540 space; here positions are kept in that space and
//! scaled by 2 when drawn on the 1920x1080 canvas.

/// Small xorshift generator (the exe uses its own rand(); only the distributions matter).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// Uniform in [0, 1).
    pub fn f(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
}

/// Fade-in / fade-out over 60 frames at both ends of a particle's life; the flicker dims odd frames of the
/// fade-out to 70%.
fn phase(age: f32, life: f32) -> (f32, f32) {
    let ai = age as i32;
    if ai <= 59 {
        return (age / 60.0, 1.0);
    }
    let left = life - age;
    if left > 59.0 {
        return (1.0, 1.0);
    }
    (left.max(0.0) / 60.0, if ai & 2 == 0 { 0.7 } else { 1.0 })
}

/// A sprite to draw: texture source rect (texels), canvas rect, alpha.
pub struct Draw {
    pub src: [f32; 4],
    pub dst: [f32; 4],
    pub alpha: f32,
    /// mirror horizontally (the shard's spin turned it around)
    pub flip: bool,
}

struct Shard {
    kind: usize,
    scale: f32,
    life: f32,
    age: f32,
    vx: f32,
    vy: f32,
    alpha: f32,
    angle: f32,
    spin: f32,
    x: f32,
    y: f32,
}

struct Dot {
    kind: usize,
    alpha: f32,
    life: f32,
    age: f32,
    x: f32,
    y: f32,
}

/// logoparticle_all source rects (x0, y0, x1, y1): shards (unk_352AC4) and their one-texel glow dots
/// (unk_352ADC).
const SHARDS: [[f32; 4]; 3] = [[0.0, 0.0, 33.0, 44.0], [64.0, 0.0, 96.0, 55.0], [128.0, 0.0, 157.0, 45.0]];
const DOTS: [[f32; 4]; 3] = [[15.0, 20.0, 16.0, 21.0], [78.0, 26.0, 79.0, 27.0], [141.0, 21.0, 142.0, 22.0]];
/// Both logo particle systems draw with a scissor of 680 px from the top (under the logo, above the menu).
pub const LOGO_CLIP_Y: f32 = 680.0;

/// Rising, spinning crystal shards and twinkling dots under the title logo.
pub struct LogoFx {
    rng: Rng,
    shards: Vec<Shard>,
    dots: Vec<Dot>,
    acc: f32,
}

impl LogoFx {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: Rng::new(seed),
            shards: Vec::new(),
            dots: Vec::new(),
            acc: 0.0,
        }
    }

    /// Advance by `frames` 60 Hz frames.
    pub fn update(&mut self, frames: f32) {
        let r = &mut self.rng;
        // sub_905B0: one shard every 2 frames (120 slots)
        self.acc += frames;
        while self.acc >= 2.0 {
            self.acc -= 2.0;
            if self.shards.len() >= 120 {
                continue;
            }
            let vy = r.f() * 0.45 + 0.13333;
            let dir = ((r.f() * 20.0 + 80.0) * std::f32::consts::PI / 180.0).cos();
            self.shards.push(Shard {
                kind: (r.f() * 3.0) as usize % 3,
                scale: r.range(0.3, 0.75),
                life: (r.f() * 150.0 + 120.0).floor(),
                age: 0.0,
                vx: vy * dir,
                vy,
                alpha: r.f() * 0.5 + 0.5,
                angle: (r.f() * 640.0).to_radians(),
                spin: (r.f() * 2.5 + 0.5).to_radians(),
                x: r.f() * 135.0 + 412.5,
                y: r.f() * 75.0 + 303.0,
            });
        }
        // sub_90D00: four dots per frame (200 slots)
        for _ in 0..(4.0 * frames).round() as usize {
            if self.dots.len() >= 200 {
                break;
            }
            self.dots.push(Dot {
                kind: (r.f() * 3.0) as usize % 3,
                alpha: r.range(0.25, 1.0),
                life: (r.f() * 42.0 + 18.0).floor(),
                age: 0.0,
                x: r.f() * 150.0 + 405.0,
                y: r.f() * -215.25 + 399.38,
            });
        }
        for s in &mut self.shards {
            s.x += s.vx * frames;
            s.y -= s.vy * frames;
            s.angle += s.spin * frames;
            s.age += frames;
        }
        self.shards.retain(|s| s.age <= s.life);
        for d in &mut self.dots {
            d.age += frames;
        }
        self.dots.retain(|d| d.age <= d.life);
    }

    /// Sprites from `logoparticle_all` at overall alpha `a`.
    pub fn draws(&self, a: f32) -> Vec<Draw> {
        let mut v = Vec::new();
        for d in &self.dots {
            // brightness rises and falls over the dot's life; a single screen pixel
            let t = 2.0 * d.age / d.life;
            let t = if t > 1.0 { 2.0 - t } else { t };
            let [x0, y0, x1, y1] = DOTS[d.kind];
            v.push(Draw {
                src: [x0, y0, x1 - x0, y1 - y0],
                dst: [d.x * 2.0 - 0.5, d.y * 2.0 - 0.5, 1.0, 1.0],
                alpha: a * d.alpha * t,
                flip: false,
            });
        }
        for s in &self.shards {
            let (k, dim) = phase(s.age, s.life);
            let [x0, y0, x1, y1] = SHARDS[s.kind];
            let c = s.angle.cos();
            let (w, h) = ((x1 - x0) * s.scale * k * c, (y1 - y0) * s.scale * k);
            v.push(Draw {
                src: [x0, y0, x1 - x0, y1 - y0],
                dst: [s.x * 2.0 - w.abs() / 2.0, s.y * 2.0 - h / 2.0, w.abs(), h],
                alpha: a * s.alpha * dim,
                flip: w < 0.0,
            });
        }
        v
    }
}

struct Spark {
    scale: f32,
    life: f32,
    age: f32,
    vx: f32,
    vy: f32,
    alpha: f32,
    x: f32,
    y: f32,
}

/// The submenu backdrop (sub_4B880): menuBG01 over the whole screen at alpha `a`, eight cloud / foliage
/// strips easing in from the top and bottom with `t`, and kirakira sparkles drifting upwards.
pub struct MenuBg {
    /// backdrop alpha and its per-frame speed (sub_49630)
    pub a: f32,
    pub speed: f32,
    /// strip animation 0..1 and its per-frame speed (+1/60 when opening, the closing speed otherwise)
    pub t: f32,
    pub t_speed: f32,
    /// sub_1B20: a fade in pauses the title movie, a fade out resumes it (off_AE0210)
    pub movie_paused: bool,
    rng: Rng,
    sparks: Vec<Spark>,
    acc: f32,
}

/// (DISP texture, alpha / 255, x centre in px, y centre at full slide in px, slide distance in px):
/// menuBG04_0..3 rise from the bottom edge, menuBG05_0..3 drop from the top edge (anchor: centre).
pub const STRIPS: [(&str, f32, f32, f32, f32); 8] = [
    ("menuBG04_0_upscale", 64.0, 960.0, 1080.0, -198.0),
    ("menuBG04_1_upscale", 96.0, 960.0, 1080.0, -160.5),
    ("menuBG04_2_upscale", 160.0, 960.0, 1080.0, -127.5),
    ("menuBG04_3_upscale", 224.0, 960.0, 1080.0, -82.5),
    ("menuBG05_0_upscale", 64.0, 960.0, 0.0, 155.25),
    ("menuBG05_1_upscale", 96.0, 849.75, 0.0, 117.75),
    ("menuBG05_2_upscale", 128.0, 960.0, 0.0, 84.75),
    ("menuBG05_3_upscale", 160.0, 829.5, 0.0, 39.75),
];

impl MenuBg {
    pub fn new(seed: u64) -> Self {
        Self {
            a: 0.0,
            speed: 0.0,
            t: 0.0,
            t_speed: 0.0,
            movie_paused: false,
            rng: Rng::new(seed),
            sparks: Vec::new(),
            acc: 0.0,
        }
    }

    /// sub_49630(_, speed): start fading in (speed > 0) or out (speed < 0).
    pub fn fade(&mut self, speed: f32) {
        if speed == 0.0 {
            self.a = if self.speed < 0.0 { 0.0 } else if self.speed > 0.0 { 1.0 } else { self.a };
        }
        self.speed = speed;
        if speed > 0.0 {
            self.t_speed = 1.0 / 60.0;
            self.movie_paused = true;
        } else if speed < 0.0 {
            self.t_speed = speed;
            self.movie_paused = false;
        }
    }

    pub fn visible(&self) -> bool {
        self.a > 0.0
    }

    pub fn update(&mut self, frames: f32) {
        self.a = (self.a + self.speed * frames).clamp(0.0, 1.0);
        self.t = (self.t + self.t_speed * frames).clamp(0.0, 1.0);
        if self.a <= 0.0 {
            self.sparks.clear();
            return;
        }
        // sub_4C190: one sparkle per frame (100 slots)
        let r = &mut self.rng;
        self.acc += frames;
        while self.acc >= 1.0 {
            self.acc -= 1.0;
            if self.sparks.len() >= 100 {
                continue;
            }
            let scale = r.f() * 0.2 + 0.1;
            let life = (r.f() * 150.0 + 120.0).floor();
            let alpha = r.range(0.25, 0.5);
            let x = r.f() * 960.0;
            let y = r.f() * 540.0;
            let vy = r.f() * 0.36667 + 0.13333;
            self.sparks.push(Spark { scale, life, age: 0.0, vx: vy * (480.0 - x) / (y + 75.0), vy, alpha, x, y });
        }
        for s in &mut self.sparks {
            s.x += s.vx * frames;
            s.y -= s.vy * frames;
            s.age += frames;
        }
        self.sparks.retain(|s| s.age <= s.life);
    }

    /// Strip offset factor: eased in while opening, held at full slide while closing.
    pub fn slide(&self) -> f32 {
        if self.t_speed >= 0.0 { 1.0 - (1.0 - self.t) * (1.0 - self.t) } else { 1.0 }
    }

    /// Strip alpha factor (eased with the animation in both directions).
    pub fn strip_alpha(&self) -> f32 {
        1.0 - (1.0 - self.t) * (1.0 - self.t)
    }

    /// kirakira sprites (82x82 texture, the sprite uses its 80x80 top-left area).
    pub fn sparks(&self) -> Vec<Draw> {
        self.sparks
            .iter()
            .map(|s| {
                let (k, dim) = phase(s.age, s.life);
                let size = s.scale * k * 80.0;
                Draw {
                    src: [0.0, 0.0, 80.0, 80.0],
                    dst: [s.x * 2.0, s.y * 2.0, size, size],
                    alpha: s.alpha * dim * self.a,
                    flip: false,
                }
            })
            .collect()
    }
}
