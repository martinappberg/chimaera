//! Color-vision-deficiency preflight — the check that turns "too many colors"
//! from a taste argument into an error message with a computed number.
//!
//! The pipeline is the published one, not an approximation of it: sRGB is
//! linearized, pushed through a Machado 2009 severity-1.0 matrix, and
//! delinearized back; distances are CIE76 ΔE over Lab (D65). CIE76 is chosen
//! deliberately — the palettes being judged are far apart or collapsed, never
//! in the near-threshold regime where CIEDE2000's corrections matter, and the
//! simpler metric keeps the numbers reproducible by hand.
//!
//! [`check_palette`] flags every pair closer than [`MIN_DELTA_E`] under any
//! simulated deficiency; [`safe_series_cap`] turns that into the largest ramp
//! prefix with no flagged pair — the number `lint --target` holds a chart's
//! series count against, and `board validate-theme` prints.

use crate::theme::{Rgb, Theme};

/// The all-pairs floor from the board plan (§9): a categorical pair whose
/// simulated ΔE falls below this is not reliably distinguishable.
pub const MIN_DELTA_E: f64 = 8.0;

/// The three dichromacies Machado 2009 models at severity 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CvdKind {
    Protanopia,
    Deuteranopia,
    Tritanopia,
}

impl CvdKind {
    pub const ALL: [CvdKind; 3] = [
        CvdKind::Protanopia,
        CvdKind::Deuteranopia,
        CvdKind::Tritanopia,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            CvdKind::Protanopia => "protanopia",
            CvdKind::Deuteranopia => "deuteranopia",
            CvdKind::Tritanopia => "tritanopia",
        }
    }
}

/// Machado, Oliveira & Fernandes 2009, "A Physiologically-based Model for
/// Simulation of Color Vision Deficiency" (IEEE TVCG 15(6)), severity-1.0
/// matrices as published in the paper's supplementary table — the same
/// constants colorspacious and DaltonLens embed. Applied in **linear** RGB;
/// applying them to gamma-encoded values is the classic implementation bug.
const PROTANOPIA: [[f64; 3]; 3] = [
    [0.152_286, 1.052_583, -0.204_868],
    [0.114_503, 0.786_281, 0.099_216],
    [-0.003_882, -0.048_116, 1.051_998],
];
const DEUTERANOPIA: [[f64; 3]; 3] = [
    [0.367_322, 0.860_646, -0.227_968],
    [0.280_085, 0.672_501, 0.047_413],
    [-0.011_820, 0.042_940, 0.968_881],
];
const TRITANOPIA: [[f64; 3]; 3] = [
    [1.255_528, -0.076_749, -0.178_779],
    [-0.078_411, 0.930_809, 0.147_602],
    [0.004_733, 0.691_367, 0.303_900],
];

fn matrix(kind: CvdKind) -> &'static [[f64; 3]; 3] {
    match kind {
        CvdKind::Protanopia => &PROTANOPIA,
        CvdKind::Deuteranopia => &DEUTERANOPIA,
        CvdKind::Tritanopia => &TRITANOPIA,
    }
}

fn srgb_to_linear(v: u8) -> f64 {
    let s = v as f64 / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f64) -> u8 {
    let c = v.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// What a dichromat sees: linearize sRGB → Machado matrix → delinearize.
pub fn simulate(rgb: Rgb, kind: CvdKind) -> Rgb {
    let (r, g, b) = (
        srgb_to_linear(rgb.r),
        srgb_to_linear(rgb.g),
        srgb_to_linear(rgb.b),
    );
    let m = matrix(kind);
    let out = [
        m[0][0] * r + m[0][1] * g + m[0][2] * b,
        m[1][0] * r + m[1][1] * g + m[1][2] * b,
        m[2][0] * r + m[2][1] * g + m[2][2] * b,
    ];
    Rgb {
        r: linear_to_srgb(out[0]),
        g: linear_to_srgb(out[1]),
        b: linear_to_srgb(out[2]),
    }
}

/// sRGB → CIE Lab (D65, 2° observer), through linear RGB and XYZ.
fn to_lab(rgb: Rgb) -> [f64; 3] {
    let (r, g, b) = (
        srgb_to_linear(rgb.r),
        srgb_to_linear(rgb.g),
        srgb_to_linear(rgb.b),
    );
    // The sRGB → XYZ matrix (IEC 61966-2-1, D65).
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;
    // D65 reference white.
    let (xn, yn, zn) = (0.950_47, 1.0, 1.088_83);
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x / xn), f(y / yn), f(z / zn));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIE76 ΔE — Euclidean distance in Lab.
pub fn delta_e(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (to_lab(a), to_lab(b));
    ((la[0] - lb[0]).powi(2) + (la[1] - lb[1]).powi(2) + (la[2] - lb[2]).powi(2)).sqrt()
}

/// One pair that collapses under one simulated deficiency: the palette
/// indices, the deficiency, and the measured distance.
#[derive(Debug, Clone, PartialEq)]
pub struct CvdFinding {
    pub kind: CvdKind,
    pub i: usize,
    pub j: usize,
    pub delta_e: f64,
}

/// Every pair `(i, j)` with simulated ΔE below [`MIN_DELTA_E`] under any of
/// the three deficiencies. Deterministic: pairs in index order, kinds in
/// [`CvdKind::ALL`] order.
pub fn check_palette(colors: &[Rgb]) -> Vec<CvdFinding> {
    let mut findings = Vec::new();
    for i in 0..colors.len() {
        for j in (i + 1)..colors.len() {
            for kind in CvdKind::ALL {
                let d = delta_e(simulate(colors[i], kind), simulate(colors[j], kind));
                if d < MIN_DELTA_E {
                    findings.push(CvdFinding {
                        kind,
                        i,
                        j,
                        delta_e: d,
                    });
                }
            }
        }
    }
    findings
}

/// The largest prefix of the ramp with no flagged pair — the computed series
/// cap. Series are assigned ramp colors in order, so the first prefix that
/// contains a collapsed pair is where a chart stops being readable.
pub fn safe_series_cap(colors: &[Rgb]) -> usize {
    for n in 2..=colors.len() {
        let has_flag = (0..n).any(|i| {
            ((i + 1)..n).any(|j| {
                CvdKind::ALL
                    .iter()
                    .any(|&k| delta_e(simulate(colors[i], k), simulate(colors[j], k)) < MIN_DELTA_E)
            })
        });
        if has_flag {
            return n - 1;
        }
    }
    colors.len()
}

// --- OKLab, for the lightness band and chroma floor -------------------------

/// sRGB → OKLab (Björn Ottosson 2020) — `(L, C)`: perceptual lightness and
/// chroma. Used only for the band checks; CVD distances stay in CIE Lab.
fn oklab_lc(rgb: Rgb) -> (f64, f64) {
    let (r, g, b) = (
        srgb_to_linear(rgb.r),
        srgb_to_linear(rgb.g),
        srgb_to_linear(rgb.b),
    );
    let l = 0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b;
    let m = 0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b;
    let s = 0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    let big_l = 0.210_454_255_3 * l_ + 0.793_617_785_0 * m_ - 0.004_072_046_8 * s_;
    let a = 1.977_998_495_1 * l_ - 2.428_592_205_0 * m_ + 0.450_593_709_9 * s_;
    let b2 = 0.025_904_037_1 * l_ + 0.782_771_766_2 * m_ - 0.808_675_766_0 * s_;
    (big_l, (a * a + b2 * b2).sqrt())
}

/// The OKLCH lightness band a categorical color must sit in. Outside it a
/// swatch reads as near-black or near-white rather than as a hue; the band is
/// wide enough that every Okabe–Ito color clears it (L 0.53–0.90).
pub const LIGHTNESS_BAND: (f64, f64) = (0.35, 0.95);

/// The chroma floor: below this a "categorical" color is effectively a gray,
/// and grayscale robustness collapses. Okabe–Ito sits at C 0.117–0.172.
pub const CHROMA_FLOOR: f64 = 0.06;

/// The full `board validate-theme` pass: WCAG text contrast (reused from the
/// theme), the OKLCH lightness band, the chroma floor, and all-pairs CVD over
/// the categorical ramp. Returns human-readable findings; empty means clean.
pub fn validate_theme(theme: &Theme) -> Vec<String> {
    let mut out = theme.contrast_findings();

    let mut ramp: Vec<(String, Rgb)> = Vec::new();
    for reference in &theme.chart.categorical {
        match theme.color(reference) {
            Some(c) => ramp.push((reference.clone(), c)),
            None => out.push(format!(
                "chart.categorical entry {reference:?} does not resolve in theme {:?}",
                theme.id
            )),
        }
    }

    for (name, c) in &ramp {
        let (l, chroma) = oklab_lc(*c);
        let (lo, hi) = LIGHTNESS_BAND;
        if l < lo || l > hi {
            out.push(format!(
                "categorical {name} ({}) has OKLCH lightness {l:.2}, outside the {lo}–{hi} band",
                c.hex()
            ));
        }
        if chroma < CHROMA_FLOOR {
            out.push(format!(
                "categorical {name} ({}) has OKLCH chroma {chroma:.3}, under the {CHROMA_FLOOR} \
                 floor — it reads as a gray, not a category",
                c.hex()
            ));
        }
    }

    let colors: Vec<Rgb> = ramp.iter().map(|(_, c)| *c).collect();
    for f in check_palette(&colors) {
        out.push(format!(
            "categorical {} ({}) vs {} ({}) is ΔE {:.1} under {} — below the {MIN_DELTA_E} floor",
            ramp[f.i].0,
            colors[f.i].hex(),
            ramp[f.j].0,
            colors[f.j].hex(),
            f.delta_e,
            f.kind.label()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// The Okabe–Ito ramp in its canonical order.
    fn okabe_ito() -> Vec<Rgb> {
        [
            "#e69f00", "#56b4e9", "#009e73", "#f0e442", "#0072b2", "#d55e00", "#cc79a7",
        ]
        .iter()
        .map(|h| crate::theme::parse_hex(h).unwrap())
        .collect()
    }

    #[test]
    fn protanopia_collapses_red_green() {
        // Directional: red vs green is enormous under normal vision and an
        // order of magnitude smaller once the L cone is gone.
        let (red, green) = (rgb(255, 0, 0), rgb(0, 128, 0));
        let normal = delta_e(red, green);
        let simulated = delta_e(
            simulate(red, CvdKind::Protanopia),
            simulate(green, CvdKind::Protanopia),
        );
        assert!(
            simulated < normal / 5.0,
            "protanopia must collapse red/green: {simulated:.1} vs normal {normal:.1}"
        );
    }

    #[test]
    fn check_palette_flags_a_red_green_pair() {
        // Firebrick vs forest green: ΔE ≈ 5.9 under deuteranopia (verified
        // against an independent Python implementation of the same pipeline).
        let pair = [rgb(178, 34, 34), rgb(34, 139, 34)];
        let findings = check_palette(&pair);
        let f = findings
            .iter()
            .find(|f| f.kind == CvdKind::Deuteranopia)
            .expect("deuteranopia must flag firebrick vs forest green");
        assert_eq!((f.i, f.j), (0, 1));
        assert!(f.delta_e < MIN_DELTA_E, "{}", f.delta_e);
        assert!(f.delta_e > 4.0, "sanity: not degenerate: {}", f.delta_e);
    }

    #[test]
    fn okabe_ito_passes_all_pairs_and_caps_at_seven() {
        // Real numbers (Machado 1.0 + CIE76): the worst Okabe–Ito pair under
        // any simulation is sky-blue vs green at ΔE ≈ 16.2 under tritanopia —
        // every pair clears the 8.0 floor, so the full 7-color ramp is safe.
        let ramp = okabe_ito();
        let findings = check_palette(&ramp);
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(safe_series_cap(&ramp), 7);
    }

    #[test]
    fn okabe_ito_adjacent_pairs_clear_the_floor_under_deuteranopia() {
        // Adjacent ramp entries are the colors that end up side by side in a
        // chart. Smallest adjacent deuteranopia distance is sky-blue vs green
        // at ΔE ≈ 48.8 — comfortably over the floor.
        let ramp = okabe_ito();
        for w in ramp.windows(2) {
            let d = delta_e(
                simulate(w[0], CvdKind::Deuteranopia),
                simulate(w[1], CvdKind::Deuteranopia),
            );
            assert!(
                d >= MIN_DELTA_E,
                "{} vs {} is ΔE {d:.1} under deuteranopia",
                w[0].hex(),
                w[1].hex()
            );
        }
    }

    #[test]
    fn safe_series_cap_stops_at_the_first_collapsed_prefix() {
        // Okabe–Ito orange, sky blue, then firebrick + forest green — the
        // red/green pair collapses under deuteranopia, so the cap is the
        // clean 3-color prefix (firebrick vs orange stays distinct).
        let ramp = vec![
            rgb(0xe6, 0x9f, 0x00),
            rgb(0x56, 0xb4, 0xe9),
            rgb(178, 34, 34),
            rgb(34, 139, 34),
        ];
        assert_eq!(safe_series_cap(&ramp), 3);
        // Degenerate ramps: a duplicate pair caps at 1.
        assert_eq!(safe_series_cap(&[rgb(10, 10, 10), rgb(10, 10, 10)]), 1);
        assert_eq!(safe_series_cap(&[]), 0);
    }

    #[test]
    fn simulation_is_idempotent_on_grays() {
        // All three matrices preserve the achromatic axis (rows sum to ~1),
        // so a gray survives simulation essentially unchanged.
        for kind in CvdKind::ALL {
            let g = rgb(128, 128, 128);
            let s = simulate(g, kind);
            assert!(
                delta_e(g, s) < 2.5,
                "{} moved gray to {}",
                kind.label(),
                s.hex()
            );
        }
    }

    #[test]
    fn bundled_themes_validate_clean() {
        // The bundled ramps are Okabe–Ito, which clears the band, the floor
        // and all-pairs CVD; the acceptance bar is that our own defaults pass
        // our own preflight.
        for id in crate::theme::BUNDLED_IDS {
            let theme = crate::theme::bundled(id).unwrap();
            let findings = validate_theme(&theme);
            assert!(findings.is_empty(), "{id}: {findings:?}");
        }
    }

    #[test]
    fn validate_theme_names_a_collapsed_pair_with_the_numbers() {
        let mut theme = crate::theme::bundled("talk-light").unwrap();
        theme.chart.categorical = vec!["#b22222".to_string(), "#228b22".to_string()];
        let findings = validate_theme(&theme);
        let f = findings
            .iter()
            .find(|f| f.contains("deuteranopia"))
            .expect("a CVD finding");
        assert!(f.contains("#b22222"), "{f}");
        assert!(f.contains("#228b22"), "{f}");
        assert!(f.contains("ΔE"), "{f}");
    }
}
