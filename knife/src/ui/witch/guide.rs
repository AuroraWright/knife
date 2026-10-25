//! The button guide (jp_4A8F0) and its back button (jp_4A790 shows it, jp_4A410 runs it, jp_4A7A0 draws
//! it: texture 84 at half scale in 960x544, sliding from x 960 to 823 at y 482 (dword_3CD654 == 1: the bottom
//! right), source (28, 176, 302, 301), pressed (28, 24, 302, 149); docked (jp_FBED4 == 1) at alpha 0, so only
//! its tap zone shows here, as in Tsukihime).

use knife_render::Quad;

use super::super::Ui;

/// jd_34FD48 (0x260DC8), 128 bytes a guide: (kind, id) pairs right to left until a negative kind; kind 0 an
/// icon of help_key (a0080h), else a word of help_txt (CONTHELP_TXT_UV).
pub const GUIDES: [&[(u8, u8)]; 11] = [
    // 0 the save / load screen: B 戻る, X 削除, Y コピー, A 決定, L R ページ変更, 移動
    &[(1, 0), (0, 10), (1, 16), (0, 9), (1, 19), (0, 8), (1, 1), (0, 7), (1, 13), (0, 14), (0, 13), (1, 2), (0, 0)],
    &[(1, 0), (0, 10), (1, 13), (0, 14), (0, 13)],
    &[(1, 0), (0, 10), (1, 1), (0, 7), (1, 5), (0, 1), (1, 12), (0, 2), (1, 13), (0, 14), (0, 13)],
    &[(1, 0), (0, 10), (1, 16), (0, 9), (1, 9), (0, 8), (1, 4), (0, 7), (1, 15), (0, 22), (1, 13), (0, 14), (0, 13), (1, 2), (0, 0)],
    &[(1, 0), (0, 10), (1, 16), (0, 9), (1, 4), (0, 7), (1, 15), (0, 22), (1, 13), (0, 14), (0, 13), (1, 2), (0, 0)],
    &[(1, 0), (0, 10), (1, 4), (0, 7), (1, 11), (0, 6), (1, 2), (0, 2)],
    &[(1, 0), (0, 10), (1, 1), (0, 7), (1, 2), (0, 0)],
    &[(1, 0), (0, 10), (1, 1), (0, 7), (1, 2), (0, 0)],
    &[(1, 0), (0, 10), (1, 1), (0, 7), (1, 21), (0, 8), (1, 22), (0, 9), (1, 13), (0, 17), (0, 16), (1, 2), (0, 0)],
    &[(1, 0), (0, 10), (1, 1), (0, 7), (1, 13), (0, 14), (0, 13), (1, 2), (0, 0)],
    // 10 the save screen's copy mode: B 戻る, A 決定, L R ページ変更, 移動
    &[(1, 0), (0, 10), (1, 1), (0, 7), (1, 13), (0, 14), (0, 13), (1, 2), (0, 0)],
];

/// a0080h (0x3CD3EC): help_key cells (x0, y0, x1, y1)
const ICONS: [[i16; 4]; 23] = [
    [8, 8, 48, 48],
    [56, 8, 96, 48],
    [104, 8, 144, 48],
    [152, 8, 192, 48],
    [200, 8, 240, 48],
    [248, 8, 288, 48],
    [296, 8, 336, 48],
    [8, 56, 48, 96],
    [56, 56, 96, 96],
    [104, 56, 144, 96],
    [152, 56, 192, 96],
    [200, 56, 240, 96],
    [248, 56, 288, 96],
    [288, 50, 328, 90],
    [16, 98, 56, 138],
    [56, 104, 96, 144],
    [8, 152, 52, 192],
    [60, 152, 104, 192],
    [152, 104, 192, 144],
    [112, 152, 156, 192],
    [164, 152, 208, 192],
    [104, 104, 148, 144],
    [200, 104, 240, 144],
];

/// The back button's fade (jd_B34B38, 0.04 a frame) and state (jd_B34B2C: 1 rising, 2 up, 3 falling).
#[derive(Default)]
pub struct WGuide {
    /// jd_B34B30 (jp_4A790)
    pub on: bool,
    a: f32,
    st: u8,
    /// jd_B34AFC (jp_4A780): its corner was tapped
    pub tapped: bool,
    /// jd_B34B34 / jd_B34B00: frames the button shows pressed
    press: i32,
}

impl Ui {
    /// jp_4A410, every frame: the back button fades with `on`; while it is up a tap that started in its corner
    /// (x 548.., y 396.. of 640x448: dword_3CD654 == 1, the bottom right) without a dialog open counts as B
    /// (jp_4A780), and the pointer goes to y 398.
    pub(in crate::ui) fn wg_step(&mut self) {
        let g = &mut self.w.guide;
        if g.on {
            if g.a < 1.0 {
                g.a += 0.04;
                if g.a < 1.0 {
                    g.st = 1;
                } else {
                    g.a = 1.0;
                    g.st = 2;
                }
            }
        } else if g.a > 0.0 {
            g.a -= 0.04;
            if g.a > 0.0 {
                g.st = 3;
            } else {
                g.a = 0.0;
                g.tapped = false;
                g.st = 0;
            }
        }
        if g.press >= 1 {
            g.press -= 1;
        }
        if g.st != 2 || self.w.dlg.busy() || self.w.inp.touch & 2 == 0 {
            return;
        }
        let [x, y] = self.w.inp.start;
        if x < 548.0 || y <= 395.0 {
            return;
        }
        g.tapped = true;
        g.press = 10;
        self.w.inp.pos[1] = 398.0;
        self.touch.at[1] = 398.0;
    }

    /// jp_4A8F0(guide, x, y): right to left from x 1920 - 10 px, icons (help_key, tex 76) at y and words
    /// (help_txt, tex 77) 3.3185 lower, 4 px apart and 10 px after each icon, at the pen's scale and alpha.
    pub(in crate::ui) fn wg_draw(&self, out: &mut Vec<Quad>, pen: &super::Pen, guide: usize, x: f32, y: f32) {
        let mut v = 10.0;
        for &(kind, id) in GUIDES.get(guide).copied().unwrap_or(&[]) {
            if kind != 0 {
                let Some(&[x0, y0, x1, y1]) = self.guide_words.1.get(usize::from(id)) else { continue };
                let w = v + f32::from(x1) - f32::from(x0);
                if let Some(t) = self.w.tex.get(77) {
                    pen.spr(out, &t, 16, Some([x0, y0, x1, y1].map(f32::from)), (1920.0 - w) * 0.33333 + x, y + 3.3185);
                }
                v = w + 4.0;
            } else {
                let Some(&[x0, y0, x1, y1]) = ICONS.get(usize::from(id)) else { continue };
                let w = v + f32::from(x1 - x0);
                if let Some(t) = self.w.tex.get(76) {
                    pen.spr(out, &t, 16, Some([x0, y0, x1, y1].map(f32::from)), (1920.0 - w) * 0.33333 + x, y);
                }
                v = w + 6.0 + 4.0;
            }
        }
    }
}
