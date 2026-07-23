//! The `equation` object's picture arm: LaTeX math → glyph-outline SVG.
//!
//! Gated behind the `math` cargo feature (on by default — the plan treats
//! equation as core vocabulary; docs/board-plan.md §"equation"). The engine is
//! mathtex, a pure-Rust translation of the real xetex core, driven *formatless*:
//! ini-xetex plus a curated math prelude and one bundled OpenType MATH font,
//! so no TeX installation, no format file, no network — the daemon stays one
//! static binary. The full LaTeX format (and the native OMML export arm) is
//! deliberately not built in v1; unknown macros surface as TeX errors, which
//! render as the standard placeholder-with-reason treatment, never a blank.
//!
//! The ceilings and the missing-feature refusal compile in every build so the
//! callers and their error text cannot drift between builds.

/// Refusal a build without the feature returns. Callers surface it verbatim.
pub const MISSING_FEATURE: &str =
    "this build lacks the math feature (rebuild with default features or --features math)";

/// TeX source over this length is refused before the engine runs — an
/// equation is notation, not a document, and the ceiling bounds engine work
/// on shared login nodes.
pub const MAX_TEX_BYTES: usize = 8 * 1024;

/// The em-size clamp in points. Outside it a stated `emSize` is a mistake;
/// the caller reports and clamps rather than typesetting unusable output.
pub const MIN_EM_PT: f64 = 4.0;
pub const MAX_EM_PT: f64 = 128.0;

/// One typeset equation: the SVG *body* (defs + glyph `<use>`s + rules,
/// no outer `<svg>` element) and its natural size in points at the typeset
/// em. Fills inherit from whatever element the caller wraps the body in;
/// black rules arrive as `fill="currentColor"` for the caller to recolor.
#[derive(Debug, Clone)]
pub struct EquationSvg {
    pub body: String,
    pub width_pt: f64,
    pub height_pt: f64,
}

/// Namespace the body's glyph-def ids (`id="gN"` / `href="#gN"`) by an
/// object-derived prefix. SVG ids are document-global, so two equations
/// inlined on one page would otherwise capture each other's glyph defs —
/// the same hazard inlined figures solve with `sanitize_svg`'s prefix.
/// String surgery is sound here because the body is machine-generated: def
/// ids are the only `id="g…"` attributes (node/glyph/font ids are numeric).
pub fn namespace_glyph_ids(body: &str, prefix: &str) -> String {
    body.replace(r#"id="g"#, &format!(r#"id="{prefix}g"#))
        .replace(r##"href="#g"##, &format!(r##"href="#{prefix}g"##))
}

/// Typeset `tex` (math mode, display style) at `em_pt` and return outline
/// SVG. Errors are complete sentences naming the TeX failure — they land in
/// diagnostics and lint findings verbatim.
#[cfg(feature = "math")]
pub fn render_tex_svg(tex: &str, em_pt: f64) -> Result<EquationSvg, String> {
    imp::render_tex_svg(tex, em_pt)
}

/// The no-feature stub: same signature, the one clear refusal.
#[cfg(not(feature = "math"))]
pub fn render_tex_svg(_tex: &str, _em_pt: f64) -> Result<EquationSvg, String> {
    Err(MISSING_FEATURE.to_string())
}

#[cfg(feature = "math")]
mod imp {
    use super::{EquationSvg, MAX_EM_PT, MAX_TEX_BYTES, MIN_EM_PT};

    use mathtex::engine::generated::generated_node_to_fragment;
    use mathtex::engine::portable_engine as pe;
    use mathtex::engine::{
        GeneratedFontSystemAdapter, GeneratedFormatCache, GeneratedResourceProvider,
        ProviderResourceRequest, Resource, ResourceError, ResourceFontSystem, ResourceKind,
        ResourceProvider,
    };
    use mathtex::font::{
        FontData, FontError, FontQuery, FontSystem, RustybuzzFontSystem, ShapeRequest, ShapedText,
    };
    use mathtex::ir::{FontId, FontRef, FragmentMetadata, GlyphId, GlyphOutline};
    use mathtex::render::GlyphOutlineSource;

    /// STIX Two Math (SIL OFL 1.1; license text alongside the font file).
    /// Fetched from the canonical upstream release tag:
    /// https://raw.githubusercontent.com/stipub/stixfonts/v2.13/fonts/static_otf/STIXTwoMath-Regular.otf
    /// sha256 f2076b9f1676438439dd41e23676f5ab99056e83d6b8f8c27841591ef2ccfa72
    /// (v2.14 and later publish font sources only, no compiled OTFs).
    static MATH_FONT: &[u8] = include_bytes!("../fonts/STIXTwoMath-Regular.otf");

    /// The name the prelude loads the font under; every font request resolves
    /// to the embedded bytes regardless, so the name is cosmetic but stable —
    /// it appears in the SVG's `data-font` attributes.
    const FONT_NAME: &str = "STIXTwoMath-Regular.otf";

    /// Serves the one embedded math font for every Font request. TeX-input
    /// requests are refused: the prelude is the whole format.
    #[derive(Clone)]
    struct OneFontProvider;

    impl ResourceProvider for OneFontProvider {
        fn read_request(
            &self,
            request: &ProviderResourceRequest,
        ) -> Result<Resource, ResourceError> {
            if request.kind == ResourceKind::Font {
                return Ok(Resource {
                    canonical_name: FONT_NAME.to_string(),
                    kind: ResourceKind::Font,
                    bytes: MATH_FONT.to_vec(),
                });
            }
            Err(ResourceError::NotFound {
                name: request.canonical_name(),
                kind: request.kind,
            })
        }
    }

    /// rustybuzz shaping over the embedded font — the same shaper pinned for
    /// the board's own text stack.
    struct Fonts {
        inner: RustybuzzFontSystem<ResourceFontSystem<OneFontProvider>>,
    }

    impl FontSystem for Fonts {
        fn load_font(&self, query: &FontQuery) -> Result<FontData, FontError> {
            self.inner.load_font(query)
        }
        fn shape_text(&self, request: &ShapeRequest<'_>) -> Result<ShapedText, FontError> {
            self.inner.shape_text(request)
        }
    }

    /// Every glyph run resolves outlines against the one embedded font.
    struct SingleFontOutlines {
        font: FontData,
    }

    impl GlyphOutlineSource for SingleFontOutlines {
        fn glyph_run_outlines(
            &self,
            _font: &FontRef,
            glyphs: &[GlyphId],
        ) -> Vec<Option<GlyphOutline>> {
            self.font
                .glyph_outlines(glyphs)
                .unwrap_or_else(|_| vec![None; glyphs.len()])
        }
    }

    /// The initialized ini-xetex format state and the parsed embedded font,
    /// built once per thread — neither type is `Sync` (the translated engine
    /// core carries raw pointers), so a process-wide `OnceLock` is out. The
    /// cache is immutable after init; every render instantiates a fresh
    /// engine from it, so renders stay pure functions of their input.
    fn with_cached<R>(f: impl FnOnce(&GeneratedFormatCache, &FontData) -> R) -> R {
        thread_local! {
            static CACHE: (GeneratedFormatCache, FontData) = (
                GeneratedFormatCache::initialized(pe::EngineProfile::xetex()),
                FontData::new(FontId(1), FONT_NAME, MATH_FONT),
            );
        }
        CACHE.with(|(cache, font)| f(cache, font))
    }

    /// The formatless prelude: ini-xetex gives bare primitives, so everything
    /// plain.tex would establish must be stated — catcodes, the MATH font on
    /// families 0–3 at text/script/scriptscript sizes, the mu glues, math
    /// classes for ASCII operators (with `-` mapped to a real minus), and a
    /// curated macro set. Curated is the point: every name here is tested,
    /// and an unknown macro is a loud TeX error, not a silent gap.
    fn document(tex: &str, em_pt: f64) -> String {
        let em = em_pt.clamp(MIN_EM_PT, MAX_EM_PT);
        let sizes = format!(
            concat!(
                "\\font\\mf=\"[{f}]:script=math\" at {main:.2}pt ",
                "\\font\\mfs=\"[{f}]:script=math\" at {script:.2}pt ",
                "\\font\\mfss=\"[{f}]:script=math\" at {sscript:.2}pt ",
            ),
            f = FONT_NAME,
            main = em,
            // Plain TeX's 10/7/5 ratios.
            script = em * 0.7,
            sscript = em * 0.5,
        );
        // The newline before the closing `$` keeps a trailing `%` comment in
        // the user's TeX from consuming the close and the `\end`.
        format!("{sizes}{PRELUDE}\\hbox{{$\\displaystyle {tex}\n\\relax$}}\\end")
    }

    const PRELUDE: &str = concat!(
        r"\catcode`\{=1 \catcode`\}=2 \catcode`\$=3 ",
        r"\catcode`\^=7 \catcode`\_=8 \catcode`\#=6 ",
        r"\mf ",
        r"\textfont0=\mf \scriptfont0=\mfs \scriptscriptfont0=\mfss ",
        r"\textfont1=\mf \scriptfont1=\mfs \scriptscriptfont1=\mfss ",
        r"\textfont2=\mf \scriptfont2=\mfs \scriptscriptfont2=\mfss ",
        r"\textfont3=\mf \scriptfont3=\mfs \scriptscriptfont3=\mfss ",
        // Ini-TeX zeroes the mu glues; these are plain.tex's values.
        r"\thinmuskip=3mu \medmuskip=4mu plus 2mu minus 4mu ",
        r"\thickmuskip=5mu plus 5mu ",
        // Delimiter codes so \left/\right and \sqrt-style radicals resolve.
        "\\Udelcode`\\(=\"0 \"0028 \\Udelcode`\\)=\"0 \"0029 ",
        "\\Udelcode`\\[=\"0 \"005B \\Udelcode`\\]=\"0 \"005D ",
        "\\Udelcode`\\/=\"0 \"002F \\Udelcode`\\|=\"0 \"007C ",
        // Ini-TeX gives every character math class 0 (ord); restore the
        // plain-TeX classes, and map ASCII hyphen to a real minus sign.
        "\\Umathcode`\\==\"3 \"0 \"003D ",
        "\\Umathcode`\\+=\"2 \"0 \"002B ",
        "\\Umathcode`\\-=\"2 \"0 \"2212 ",
        "\\Umathcode`\\*=\"2 \"0 \"2217 ",
        "\\Umathcode`\\(=\"4 \"0 \"0028 ",
        "\\Umathcode`\\)=\"5 \"0 \"0029 ",
        "\\Umathcode`\\[=\"4 \"0 \"005B ",
        "\\Umathcode`\\]=\"5 \"0 \"005D ",
        "\\Umathcode`\\<=\"3 \"0 \"003C ",
        "\\Umathcode`\\>=\"3 \"0 \"003E ",
        "\\Umathcode`\\,=\"6 \"0 \"002C ",
        "\\Umathcode`\\;=\"6 \"0 \"003B ",
        "\\Umathcode`\\!=\"5 \"0 \"0021 ",
        "\\Umathcode`\\?=\"5 \"0 \"003F ",
        // Structures.
        r"\def\frac#1#2{{#1\over#2}} ",
        "\\def\\sqrt{\\Uradical\"0 \"221A } ",
        "\\def\\hat#1{\\Umathaccent\"0 \"0 \"0302 {#1}} ",
        "\\def\\bar#1{\\Umathaccent\"0 \"0 \"0304 {#1}} ",
        "\\def\\tilde#1{\\Umathaccent\"0 \"0 \"0303 {#1}} ",
        "\\def\\vec#1{\\Umathaccent\"0 \"0 \"20D7 {#1}} ",
        "\\def\\dot#1{\\Umathaccent\"0 \"0 \"0307 {#1}} ",
        "\\def\\ddot#1{\\Umathaccent\"0 \"0 \"0308 {#1}} ",
        // Big operators (class 1: limits above/below in display style).
        "\\Umathchardef\\sum=\"1 \"0 \"2211 ",
        "\\Umathchardef\\prod=\"1 \"0 \"220F ",
        "\\Umathchardef\\bigcup=\"1 \"0 \"22C3 ",
        "\\Umathchardef\\bigcap=\"1 \"0 \"22C2 ",
        "\\def\\int{\\Umathchar\"1 \"0 \"222B \\nolimits} ",
        "\\def\\oint{\\Umathchar\"1 \"0 \"222E \\nolimits} ",
        // Binary operators (class 2).
        "\\Umathchardef\\pm=\"2 \"0 \"00B1 ",
        "\\Umathchardef\\mp=\"2 \"0 \"2213 ",
        "\\Umathchardef\\cdot=\"2 \"0 \"22C5 ",
        "\\Umathchardef\\times=\"2 \"0 \"00D7 ",
        "\\Umathchardef\\div=\"2 \"0 \"00F7 ",
        "\\Umathchardef\\circ=\"2 \"0 \"2218 ",
        "\\Umathchardef\\bullet=\"2 \"0 \"2219 ",
        "\\Umathchardef\\oplus=\"2 \"0 \"2295 ",
        "\\Umathchardef\\otimes=\"2 \"0 \"2297 ",
        "\\Umathchardef\\cup=\"2 \"0 \"222A ",
        "\\Umathchardef\\cap=\"2 \"0 \"2229 ",
        "\\Umathchardef\\setminus=\"2 \"0 \"2216 ",
        "\\Umathchardef\\wedge=\"2 \"0 \"2227 ",
        "\\Umathchardef\\vee=\"2 \"0 \"2228 ",
        // Relations (class 3).
        "\\Umathchardef\\le=\"3 \"0 \"2264 ",
        "\\Umathchardef\\leq=\"3 \"0 \"2264 ",
        "\\Umathchardef\\ge=\"3 \"0 \"2265 ",
        "\\Umathchardef\\geq=\"3 \"0 \"2265 ",
        "\\Umathchardef\\ne=\"3 \"0 \"2260 ",
        "\\Umathchardef\\neq=\"3 \"0 \"2260 ",
        "\\Umathchardef\\approx=\"3 \"0 \"2248 ",
        "\\Umathchardef\\sim=\"3 \"0 \"223C ",
        "\\Umathchardef\\simeq=\"3 \"0 \"2243 ",
        "\\Umathchardef\\equiv=\"3 \"0 \"2261 ",
        "\\Umathchardef\\propto=\"3 \"0 \"221D ",
        "\\Umathchardef\\in=\"3 \"0 \"2208 ",
        "\\Umathchardef\\notin=\"3 \"0 \"2209 ",
        "\\Umathchardef\\subset=\"3 \"0 \"2282 ",
        "\\Umathchardef\\subseteq=\"3 \"0 \"2286 ",
        "\\Umathchardef\\supset=\"3 \"0 \"2283 ",
        "\\Umathchardef\\supseteq=\"3 \"0 \"2287 ",
        "\\Umathchardef\\to=\"3 \"0 \"2192 ",
        "\\Umathchardef\\rightarrow=\"3 \"0 \"2192 ",
        "\\Umathchardef\\leftarrow=\"3 \"0 \"2190 ",
        "\\Umathchardef\\Rightarrow=\"3 \"0 \"21D2 ",
        "\\Umathchardef\\Leftarrow=\"3 \"0 \"21D0 ",
        "\\Umathchardef\\mapsto=\"3 \"0 \"21A6 ",
        "\\Umathchardef\\perp=\"3 \"0 \"22A5 ",
        "\\Umathchardef\\mid=\"3 \"0 \"2223 ",
        "\\Umathchardef\\ll=\"3 \"0 \"226A ",
        "\\Umathchardef\\gg=\"3 \"0 \"226B ",
        // Ordinary symbols (class 0).
        "\\Umathchardef\\infty=\"0 \"0 \"221E ",
        "\\Umathchardef\\partial=\"0 \"0 \"2202 ",
        "\\Umathchardef\\nabla=\"0 \"0 \"2207 ",
        "\\Umathchardef\\forall=\"0 \"0 \"2200 ",
        "\\Umathchardef\\exists=\"0 \"0 \"2203 ",
        "\\Umathchardef\\emptyset=\"0 \"0 \"2205 ",
        "\\Umathchardef\\hbar=\"0 \"0 \"210F ",
        "\\Umathchardef\\ell=\"0 \"0 \"2113 ",
        "\\Umathchardef\\prime=\"0 \"0 \"2032 ",
        "\\Umathchardef\\ldots=\"0 \"0 \"2026 ",
        "\\Umathchardef\\cdots=\"0 \"0 \"22EF ",
        "\\Umathchardef\\lbrace=\"4 \"0 \"007B ",
        "\\Umathchardef\\rbrace=\"5 \"0 \"007D ",
        r"\def\{{\lbrace} \def\}{\rbrace} ",
        // Greek, lowercase then the uppercase set with distinct glyphs.
        "\\Umathchardef\\alpha=\"0 \"0 \"03B1 ",
        "\\Umathchardef\\beta=\"0 \"0 \"03B2 ",
        "\\Umathchardef\\gamma=\"0 \"0 \"03B3 ",
        "\\Umathchardef\\delta=\"0 \"0 \"03B4 ",
        "\\Umathchardef\\epsilon=\"0 \"0 \"03F5 ",
        "\\Umathchardef\\varepsilon=\"0 \"0 \"03B5 ",
        "\\Umathchardef\\zeta=\"0 \"0 \"03B6 ",
        "\\Umathchardef\\eta=\"0 \"0 \"03B7 ",
        "\\Umathchardef\\theta=\"0 \"0 \"03B8 ",
        "\\Umathchardef\\vartheta=\"0 \"0 \"03D1 ",
        "\\Umathchardef\\iota=\"0 \"0 \"03B9 ",
        "\\Umathchardef\\kappa=\"0 \"0 \"03BA ",
        "\\Umathchardef\\lambda=\"0 \"0 \"03BB ",
        "\\Umathchardef\\mu=\"0 \"0 \"03BC ",
        "\\Umathchardef\\nu=\"0 \"0 \"03BD ",
        "\\Umathchardef\\xi=\"0 \"0 \"03BE ",
        "\\Umathchardef\\pi=\"0 \"0 \"03C0 ",
        "\\Umathchardef\\rho=\"0 \"0 \"03C1 ",
        "\\Umathchardef\\sigma=\"0 \"0 \"03C3 ",
        "\\Umathchardef\\varsigma=\"0 \"0 \"03C2 ",
        "\\Umathchardef\\tau=\"0 \"0 \"03C4 ",
        "\\Umathchardef\\upsilon=\"0 \"0 \"03C5 ",
        "\\Umathchardef\\phi=\"0 \"0 \"03D5 ",
        "\\Umathchardef\\varphi=\"0 \"0 \"03C6 ",
        "\\Umathchardef\\chi=\"0 \"0 \"03C7 ",
        "\\Umathchardef\\psi=\"0 \"0 \"03C8 ",
        "\\Umathchardef\\omega=\"0 \"0 \"03C9 ",
        "\\Umathchardef\\Gamma=\"0 \"0 \"0393 ",
        "\\Umathchardef\\Delta=\"0 \"0 \"0394 ",
        "\\Umathchardef\\Theta=\"0 \"0 \"0398 ",
        "\\Umathchardef\\Lambda=\"0 \"0 \"039B ",
        "\\Umathchardef\\Xi=\"0 \"0 \"039E ",
        "\\Umathchardef\\Pi=\"0 \"0 \"03A0 ",
        "\\Umathchardef\\Sigma=\"0 \"0 \"03A3 ",
        "\\Umathchardef\\Upsilon=\"0 \"0 \"03A5 ",
        "\\Umathchardef\\Phi=\"0 \"0 \"03A6 ",
        "\\Umathchardef\\Psi=\"0 \"0 \"03A8 ",
        "\\Umathchardef\\Omega=\"0 \"0 \"03A9 ",
        // Spacing.
        r"\def\,{\mskip\thinmuskip} \def\;{\mskip\thickmuskip} ",
        r"\def\quad{\hskip 1em } \def\qquad{\hskip 2em } ",
        // Operator names, the plain-TeX set. Plain TeX spells these
        // \mathop{\rm log}: upright roman letters in an Op atom. This
        // formatless prelude has no \rm — and needs none: v1 leaves letters
        // at their literal codepoints (upright in STIX Two Math, matching
        // how variables render), so bare letters inside \mathop already give
        // the upright operator look with Op spacing. Following plain.tex,
        // the \lim class omits \nolimits and so takes limits above/below in
        // display style; the \log class pins \nolimits.
        r"\def\log{\mathop{log}\nolimits} ",
        r"\def\ln{\mathop{ln}\nolimits} ",
        r"\def\lg{\mathop{lg}\nolimits} ",
        r"\def\exp{\mathop{exp}\nolimits} ",
        r"\def\sin{\mathop{sin}\nolimits} ",
        r"\def\cos{\mathop{cos}\nolimits} ",
        r"\def\tan{\mathop{tan}\nolimits} ",
        r"\def\arcsin{\mathop{arcsin}\nolimits} ",
        r"\def\arccos{\mathop{arccos}\nolimits} ",
        r"\def\arctan{\mathop{arctan}\nolimits} ",
        r"\def\sinh{\mathop{sinh}\nolimits} ",
        r"\def\cosh{\mathop{cosh}\nolimits} ",
        r"\def\tanh{\mathop{tanh}\nolimits} ",
        r"\def\arg{\mathop{arg}\nolimits} ",
        r"\def\dim{\mathop{dim}\nolimits} ",
        r"\def\min{\mathop{min}} ",
        r"\def\max{\mathop{max}} ",
        r"\def\det{\mathop{det}} ",
        r"\def\gcd{\mathop{gcd}} ",
        r"\def\inf{\mathop{inf}} ",
        r"\def\sup{\mathop{sup}} ",
        r"\def\Pr{\mathop{Pr}} ",
        r"\def\lim{\mathop{lim}} ",
        r"\def\liminf{\mathop{lim\,inf}} ",
        r"\def\limsup{\mathop{lim\,sup}} ",
    );

    /// First transcript line that signals a TeX error (`! ...`), if any.
    fn tex_error(transcript: &[u8]) -> Option<String> {
        let text = String::from_utf8_lossy(transcript);
        text.lines()
            .find(|l| l.trim_start().starts_with("! "))
            .map(|l| l.trim().to_string())
    }

    pub(super) fn render_tex_svg(tex: &str, em_pt: f64) -> Result<EquationSvg, String> {
        if tex.trim().is_empty() {
            return Err("tex is empty; nothing to typeset".to_string());
        }
        if tex.len() > MAX_TEX_BYTES {
            return Err(format!(
                "tex is {} bytes, over the {MAX_TEX_BYTES}-byte ceiling",
                tex.len()
            ));
        }

        let (fragment, svg) = with_cached(|cache, font| {
            let provider = OneFontProvider;
            let mut engine = cache
                .instantiate(
                    pe::EngineProfile::xetex(),
                    GeneratedResourceProvider::new(provider.clone()),
                )
                .with_font_platform(GeneratedFontSystemAdapter::new(Fonts {
                    inner: RustybuzzFontSystem::new(ResourceFontSystem::new(provider)),
                }));

            // Sandbox mode rejects job control and runaway loops as errors —
            // the TeX is agent-written input on a shared login node.
            engine.set_sandbox(true);
            engine.begin_fragment_capture();
            if !engine.begin_primary_input("equation.tex", document(tex, em_pt).into_bytes()) {
                return Err("the TeX engine refused the input".to_string());
            }
            let ran = engine.run_main_control();
            engine.end_fragment_capture();
            if let Some(err) = tex_error(engine.transcript_bytes()) {
                return Err(format!("TeX error: {err}"));
            }
            if !ran {
                return Err(match engine.last_error_message() {
                    Some(m) => format!("TeX error: {m}"),
                    None => "the TeX engine did not run to completion".to_string(),
                });
            }

            let root = engine
                .captured_fragment_root()
                .ok_or_else(|| "the TeX engine produced no output box".to_string())?;
            let fragment = generated_node_to_fragment(
                &engine,
                root,
                FragmentMetadata {
                    engine_profile: "xetex".into(),
                    format_id: "chimaera-board-equation".into(),
                    fragment_kind: Default::default(),
                },
            )
            .ok_or_else(|| "the typeset box could not be lowered to layout IR".to_string())?;

            let outlines = SingleFontOutlines { font: font.clone() };
            let svg = mathtex::svg::render_with_outlines(&fragment, &outlines)
                .map_err(|e| format!("SVG emission failed: {e:?}"))?;
            Ok((fragment, svg))
        })?;

        // Scaled points → TeX points. The 0.4% TeX-pt/PostScript-pt gap is
        // irrelevant: the placed frame is the truth and the aspect survives.
        let width_pt = fragment.surface.width.0 as f64 / 65_536.0;
        let height_pt = fragment.surface.height.0 as f64 / 65_536.0;
        if !(width_pt > 0.0 && height_pt > 0.0) {
            return Err("the equation typeset to an empty box".to_string());
        }

        // Strip mathtex's root element; the caller owns placement and fill.
        let body = svg
            .find('>')
            .map(|open| &svg[open + 1..])
            .and_then(|rest| rest.strip_suffix("</svg>"))
            .ok_or_else(|| "SVG emission produced an unexpected shape".to_string())?
            .to_string();
        if !body.contains("<use") {
            return Err("the equation produced no glyphs".to_string());
        }
        Ok(EquationSvg {
            body,
            width_pt,
            height_pt,
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(feature = "math"))]
    #[test]
    fn without_the_feature_the_refusal_names_the_flag() {
        let err = super::render_tex_svg("E = mc^2", 12.0).unwrap_err();
        assert_eq!(err, super::MISSING_FEATURE);
    }

    #[cfg(feature = "math")]
    mod with_feature {
        use super::super::*;

        /// Wrap a body in a standalone document, as embedders do.
        fn document(eq: &EquationSvg) -> String {
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {} {}" fill="#000000">{}</svg>"##,
                eq.width_pt,
                eq.height_pt,
                eq.body.replace("currentColor", "#000000")
            )
        }

        /// Rasterized ink coverage — proves visible output, not just markup.
        fn ink_pixels(eq: &EquationSvg) -> usize {
            let doc = document(eq);
            let tree = usvg::Tree::from_str(&doc, &usvg::Options::default()).expect("usvg parses");
            let scale = 4.0f32;
            let (w, h) = (
                (eq.width_pt as f32 * scale).ceil() as u32,
                (eq.height_pt as f32 * scale).ceil() as u32,
            );
            let mut pixmap = tiny_skia::Pixmap::new(w.max(1), h.max(1)).unwrap();
            resvg::render(
                &tree,
                tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            pixmap.pixels().iter().filter(|p| p.alpha() > 128).count()
        }

        #[test]
        fn the_reference_expressions_typeset_with_real_outlines() {
            for tex in [
                r"E = mc^2",
                r"\frac{-b \pm \sqrt{b^2-4ac}}{2a}",
                r"\sum_{i=1}^{n} x_i",
                r"\hat{\beta}",
                // The live-drive regression: operator names are staple math.
                r"T(n) = \frac{n \log n}{k} + c",
                r"\lim_{n \to \infty} \frac{\log n}{n} = 0",
            ] {
                let eq = render_tex_svg(tex, 12.0).unwrap_or_else(|e| panic!("{tex}: {e}"));
                assert!(eq.width_pt > 1.0 && eq.height_pt > 1.0, "{tex}: empty box");
                assert!(
                    eq.body.contains("<path") && eq.body.contains("<use"),
                    "{tex}: no glyph outline paths"
                );
                assert!(ink_pixels(&eq) > 40, "{tex}: rendered blank");
            }
        }

        #[test]
        fn the_curated_prelude_names_all_resolve() {
            // One expression exercising every macro family the prelude
            // defines; a TeX error here means the curated set drifted.
            let tex = r"\left( \int_0^\infty \alpha\beta\gamma\delta\epsilon\varepsilon\zeta\eta\theta\vartheta\iota\kappa\lambda\mu\nu\xi\pi\rho\sigma\varsigma\tau\upsilon\phi\varphi\chi\psi\omega \, \Gamma\Delta\Theta\Lambda\Xi\Pi\Sigma\Upsilon\Phi\Psi\Omega \right] \quad \{ x \mid x \in A \cup B, x \notin A \cap B \} \; \prod_k \oint \nabla\partial\infty\forall\exists\emptyset\hbar\ell\prime\ldots\cdots \qquad a \pm b \mp c \cdot d \times e \div f \circ g \bullet h \oplus i \otimes j \setminus k \wedge l \vee m \le n \geq o \ne p \approx q \sim r \simeq s \equiv t \propto u \subset v \subseteq w \supset x \supseteq y \to z \rightarrow \leftarrow \Rightarrow \Leftarrow \mapsto \perp \ll \gg \bar{a}\tilde{b}\vec{c}\dot{d}\ddot{e} \bigcup_j \bigcap_j";
            let eq = render_tex_svg(tex, 12.0).expect("the whole curated set typesets");
            assert!(eq.body.contains("<use"));

            // The full operator-name set, \log-class then \lim-class (which
            // takes limits above/below in display style, per plain.tex).
            let tex = r"\log x + \ln x + \lg x + \exp x + \sin x + \cos x + \tan x + \arcsin x + \arccos x + \arctan x + \sinh x + \cosh x + \tanh x + \arg z + \dim V \quad \min_i x_i + \max_i x_i + \det A + \gcd(a,b) + \inf_n a_n + \sup_n a_n + \Pr(X) + \lim_{n \to \infty} a_n + \liminf_{n} a_n + \limsup_{n} a_n";
            let eq = render_tex_svg(tex, 12.0).expect("every operator name typesets");
            assert!(eq.body.contains("<use"));
        }

        #[test]
        fn lim_takes_display_limits_and_log_does_not() {
            // \lim_{n} sets its subscript under the operator (display
            // limits), so the box grows taller than wide relative to the
            // same content on \log_{n}, whose \nolimits keeps the subscript
            // beside it — pinning the plain-TeX limits split.
            let lim = render_tex_svg(r"\lim_{n \to \infty}", 12.0).unwrap();
            let log = render_tex_svg(r"\log_{n \to \infty}", 12.0).unwrap();
            assert!(
                lim.height_pt > log.height_pt + 1.0,
                "lim {}x{} vs log {}x{}",
                lim.width_pt,
                lim.height_pt,
                log.width_pt,
                log.height_pt
            );
            assert!(
                log.width_pt > lim.width_pt + 1.0,
                "lim {}x{} vs log {}x{}",
                lim.width_pt,
                lim.height_pt,
                log.width_pt,
                log.height_pt
            );
        }

        #[test]
        fn rendering_is_deterministic() {
            let a = render_tex_svg(r"\frac{1}{2} + \sqrt{x}", 12.0).unwrap();
            let b = render_tex_svg(r"\frac{1}{2} + \sqrt{x}", 12.0).unwrap();
            assert_eq!(a.body, b.body, "same TeX, same bytes");
            assert_eq!((a.width_pt, a.height_pt), (b.width_pt, b.height_pt));
        }

        #[test]
        fn em_size_scales_the_natural_box() {
            let small = render_tex_svg("x + y", 10.0).unwrap();
            let large = render_tex_svg("x + y", 20.0).unwrap();
            assert!(
                large.width_pt > small.width_pt * 1.8,
                "{} vs {}",
                large.width_pt,
                small.width_pt
            );
        }

        #[test]
        fn a_tex_error_is_a_named_refusal_not_a_blank() {
            // An \right with no \left is a definite TeX error.
            let err = render_tex_svg(r"x \right)", 12.0).unwrap_err();
            assert!(err.contains("TeX error"), "{err}");
            let err = render_tex_svg(r"\notamacro", 12.0).unwrap_err();
            assert!(err.contains("TeX error"), "{err}");
        }

        #[test]
        fn the_ceilings_refuse_before_the_engine_runs() {
            let err = render_tex_svg("", 12.0).unwrap_err();
            assert!(err.contains("empty"), "{err}");
            let long = "x+".repeat(MAX_TEX_BYTES);
            let err = render_tex_svg(&long, 12.0).unwrap_err();
            assert!(err.contains("ceiling"), "{err}");
        }
    }
}
