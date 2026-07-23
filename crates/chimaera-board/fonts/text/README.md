# Bundled text fonts — provenance & licenses

These faces are baked into the `chimaera-board` binary with `include_bytes!`
(see `src/layout.rs`, the `bundled` module) and registered into the render
`fontdb` on every `FontStack::new`. That is the fix for headless determinism:
a board renders in the same face on a laptop and on a fontless HPC login node,
and the app carries a deterministic typographic identity instead of falling back
to whatever `sans-serif` a compute node happens to have. This mirrors how the
math font (`../STIXTwoMath-Regular.otf`) is committed for the `equation` feature.

The **default** is a standard, **Helvetica/Arial-class** sans (**Arimo**), not a
tech-brand face. Board is a figure/slide tool: scientific venues expect
Arial/Helvetica-class metrics (a strict PLOS figure requires *Arial*, not
Helvetica — the trap that bounces submissions), and since exports keep editable
text a user can restyle downstream — so the safe default is the expected,
metric-standard one. The other faces stay bundled as **selectable alternates**:
Geist (a brand/slides sans), IBM Plex Sans (a neutral alternate), and JetBrains
Mono (the `code` role). How to switch is below.

Every family here is licensed under the **SIL Open Font License, Version 1.1**
(OFL), which permits bundling and redistribution inside a binary; the full
license text ships beside the fonts (the OFL requires the license travel with
the Font Software). Only static weight instances are committed — never variable
fonts — so weight selection is exact and reproducible across hosts.

| Family | Role in the themes | Weights bundled | Version | Source | License |
|---|---|---|---|---|---|
| **Arimo** | **default sans** — the text face every bundled theme leads with; metric-compatible with Arial (the PLOS/Cell figure standard) | Regular 400, Bold 700 | 1.33 (repo `4a6255f`) | [googlefonts/Arimo](https://github.com/googlefonts/Arimo) `fonts/otf/` | `Arimo-OFL.txt` |
| **Geist** | selectable **brand/slides** alternate | Regular 400, SemiBold 600, Bold 700 | 1.800 (repo v1.7.2) | [vercel/geist-font](https://github.com/vercel/geist-font) `fonts/Geist/otf/` | `Geist-OFL.txt` |
| **IBM Plex Sans** | selectable clean **neutral** alternate | Regular 400, SemiBold 600, Bold 700 | 3.005 (repo v1.1.0) | [IBM/plex](https://github.com/IBM/plex) `packages/plex-sans/fonts/complete/otf/` | `IBMPlexSans-LICENSE.txt` |
| **JetBrains Mono** | monospace for the `code` role (data/code) — the app's terminal face | Regular 400 | 2.305 (release v2.304) | [JetBrains/JetBrainsMono](https://github.com/JetBrains/JetBrainsMono) `fonts/ttf/` | `JetBrainsMono-OFL.txt` |

Arimo, like Arial itself, is a four-style family — it ships **Regular (400) and
Bold (700)** only. The talk themes ask for weight 600 on titles/headings; fontdb
resolves that to Arimo's 700 Bold *within the Arimo family* (never a foreign
fallback), which is exactly how a bold Arial heading is set. The alternates
(Geist, IBM Plex Sans) carry a true 600 SemiBold for the slides look, so a user
who switches a theme to one of them gets exact weights. `code` is monospace-only
and never headed, so JetBrains Mono ships Regular only.

## sha256 of the committed files

```
ee818d9e4f9b1ee9d12ed6839a28545270359d98bb63dd0ce02fd5430e22e69b  Arimo-Regular.otf
b269dcdb7abe31ff49aaa4b4d85f25663777af7650b7baf14cf70f5fe108598b  Arimo-Bold.otf
63eed3b8f533234e2ae120fae23e79c92d8dda96bccce4147480c62a2fbddba5  Geist-Regular.otf
0416da9be298af36716be61292eb930ad5bcced2dfe60c1bbca3af838eea34ef  Geist-SemiBold.otf
b23edd02fa88c86701214cd0aa90d43f63798d4eb4b1bc1c52fbf834ff30d113  Geist-Bold.otf
6b17a35a31ded2e81b3ed19e5eb532d22b9a0b5a76833b0d757a5c71ab5e0f6c  IBMPlexSans-Regular.otf
1aff1f99f0f415632e71a4b9d43804d093e85b8954489a973f0cf1e2e24b9b04  IBMPlexSans-SemiBold.otf
19de5aec74215119b3f8f7d1b1f0e0eba867bee2d2c65c5761b287d67581c316  IBMPlexSans-Bold.otf
e6fd0d7e91550b3ed2b735d4312474362c4716edc4fc0577a0f61ed782d5aed1  JetBrainsMono-Regular.ttf
```

## Changing the font

Fonts are a **theme** property, not a per-board schema field. The default is
Arimo (Arial-class); to pick a bundled alternate for a board — say the Geist
brand look for slides — export a bundled theme, edit its `type.*.family` stacks,
and reference the edited theme:

```sh
chimaera board theme-export talk-light --format json > .chimaera/board/themes/mytalk.theme.json
# edit each role's "family": ["Geist", "Arimo", …]  (first that resolves wins)
# then in the board:  "theme": "mytalk"
```

A workspace can also vendor any additional OFL/licensed face into
`.chimaera/board/fonts/` — vendored fonts win over these bundled ones — and
name it first in a theme's family stack.
