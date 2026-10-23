//! The OpenType / TrueType header fields nn::fontll's font metrics come from (the game's font manager sizes
//! its lines with them, see `set_metrics`).

/// A font's design metrics, in font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    pub units_per_em: u16,
    /// `OS/2` usWinAscent / usWinDescent
    pub win_ascent: u16,
    pub win_descent: u16,
    /// `head` bounding box y range
    pub y_min: i16,
    pub y_max: i16,
}

/// Read the metrics of an sfnt (TrueType / OpenType, the first font of a collection).
pub fn metrics(data: &[u8]) -> Option<Metrics> {
    let u16_at = |o: usize| Some(u16::from_be_bytes(data.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_be_bytes(data.get(o..o + 4)?.try_into().ok()?));
    let base = if data.get(..4)? == b"ttcf" { u32_at(12)? as usize } else { 0 };
    let n = u16_at(base + 4)? as usize;
    let table = |tag: &[u8; 4]| {
        (0..n).map(|i| base + 12 + 16 * i).find(|&r| data.get(r..r + 4) == Some(tag)).and_then(|r| u32_at(r + 8)).map(|o| o as usize)
    };
    let head = table(b"head")?;
    let os2 = table(b"OS/2")?;
    Some(Metrics {
        units_per_em: u16_at(head + 18)?,
        y_min: u16_at(head + 38)? as i16,
        y_max: u16_at(head + 42)? as i16,
        win_ascent: u16_at(os2 + 74)?,
        win_descent: u16_at(os2 + 76)?,
    })
}

/// nn::font's ScalableFont metrics of a font set at `size` px (font manager sub_CAE88 -> sub_1402A4):
/// sub_14193C fills a record per font from `nn::fontll::ScalableFontEngine::GetFontMetrics` (FSS_font_metrics:
/// `OS/2` usWinAscent / usWinDescent, the `head` bounding box) as ratios of the em, ascent / em, (ascent +
/// descent) / em, yMax / em and (yMax - yMin) / em, and gives every font of the set the record with the largest
/// bbox height (the first on a tie). The object rounds them at its size: height (+28) = (int)(f1 * size +
/// 0.5), the line feed (+40) the same, ascent (+32) = (int)(ascent * size + 0.5). Switch line feeds at 40 px:
/// Japanese 50 (FOT-Seurat, ja3: 1029 + 217 of 1000), English 53 (DemosNext, en2: 1076 + 255; it ties Seurat's
/// bbox and comes first), Simplified Chinese 49 (MYingHei, zc2: 983 + 253), the shared standard font's 60 (1158 +
/// 378 of 1024; the title's version text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetMetrics {
    pub height: i32,
    pub ascent: i32,
    pub line_feed: i32,
    /// GetBaselinePos (+36): (int)(yMax / em * size + 0.5)
    pub baseline: i32,
}

pub fn set_metrics(fonts: &[Metrics], size: f32) -> Option<SetMetrics> {
    let mut best: Option<(f32, &Metrics)> = None;
    for m in fonts {
        let em = f32::from(m.units_per_em.max(1));
        let f3 = (f32::from(m.y_max) - f32::from(m.y_min)) / em;
        if best.is_none_or(|(b, _)| b < f3) {
            best = Some((f3, m));
        }
    }
    best.map(|(_, m)| {
        let em = f32::from(m.units_per_em.max(1));
        let f1 = (f32::from(m.win_ascent) + f32::from(m.win_descent)) / em;
        let height = (f1 * size + 0.5) as i32;
        let round = |v: f32| (v / em * size + 0.5) as i32;
        SetMetrics { height, ascent: round(f32::from(m.win_ascent)), line_feed: height, baseline: round(f32::from(m.y_max)) }
    })
}

/// The metrics of the pair font a text slot draws with (sub_CAE88: nn::font PairFont of the exfont ResFont and
/// the set's ScalableFont): the line feed is the larger (sub_13D56C), the ascent and baseline position the
/// taller font's by height, the exfont's on a tie (sub_13D340 / sub_13D3F8). `ex` is the exfont as a set (FINF
/// height, ascent and line feed 47 / 39 / 47, TGLP baseline 37): only Traditional Chinese's set (SungASC,
/// height 45) is shorter, so its lines take the exfont's ascent 39 and line feed 47 (Switch: page lines 77.7 px
/// apart, 2.4 px further than the set's 45 gives) and its ruby the exfont's baseline.
pub fn pair_metrics(set: SetMetrics, ex: Option<SetMetrics>) -> SetMetrics {
    match ex {
        Some(ex) => {
            let taller = if ex.height >= set.height { ex } else { set };
            SetMetrics { line_feed: ex.line_feed.max(set.line_feed), ..taller }
        }
        None => set,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(units_per_em: u16, win_ascent: u16, win_descent: u16, y_min: i16, y_max: i16) -> Metrics {
        Metrics { units_per_em, win_ascent, win_descent, y_min, y_max }
    }

    #[test]
    fn message_pair_metrics() {
        let ex = SetMetrics { height: 47, ascent: 39, line_feed: 47, baseline: 37 };
        let pair = |fonts: &[Metrics]| pair_metrics(set_metrics(fonts, 40.0).unwrap(), Some(ex));
        // Japanese: FOT-Seurat (ja3) has the tallest bbox
        let ja = pair(&[font(1000, 880, 120, -120, 880), font(1000, 1029, 217, -217, 1029)]);
        assert_eq!(ja, SetMetrics { height: 50, ascent: 41, line_feed: 50, baseline: 41 });
        // English: Demos Next (en2) ties Seurat (en3) and comes first
        let en = pair(&[font(1000, 973, 234, -214, 973), font(1000, 1076, 255, -242, 1004), font(1000, 1029, 217, -217, 1029)]);
        assert_eq!(en, SetMetrics { height: 53, ascent: 43, line_feed: 53, baseline: 40 });
        // Traditional Chinese: SungASC is shorter than the exfont, which gives the ascent and baseline
        let zt = pair(&[font(2048, 1837, 457, -512, 2181), font(2048, 1837, 457, -430, 2241)]);
        assert_eq!(zt, SetMetrics { height: 47, ascent: 39, line_feed: 47, baseline: 37 });
    }
}
