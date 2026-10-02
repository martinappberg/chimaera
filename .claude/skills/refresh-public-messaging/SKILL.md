---
name: refresh-public-messaging
description: Refresh Chimaera's public face — the README and the GitHub Pages site (site/index.html + site/docs.html) — so it matches what the product actually does now, and brainstorm positioning with the maintainer. Use when the README or website has drifted behind shipped features, when revisiting the selling points / tagline, or on a periodic "is our public face still current and well-said?" pass. Positioning is the maintainer's call — never rewrite it unilaterally.
---

# Refreshing Chimaera's public messaging

The public face is **README.md** + the **GitHub Pages site** (`site/`). It drifts because
capabilities ship faster than copy — a flagship feature can land and never reach the homepage.
This skill closes that gap. It has two jobs, and the split is load-bearing:

- **Facts** — what the product does — are **derived**. You verify them against the code and the
  feature catalog and fix them freely. Old public copy is a *suspect, not a source*.
- **Positioning** — the tagline, the selling points, the audience the page leads with — is the
  **maintainer's ground truth**, exactly like a feature's `## Intent`. You never invent or
  "helpfully improve" it alone. You surface the tensions and *brainstorm*; the maintainer decides.

## The public surfaces (keep them in sync)

| File | What it is |
|---|---|
| [README.md](../../../README.md) | The GitHub repo landing page — headline, why, quickstart, feature list. |
| [site/index.html](../../../site/index.html) | The marketing homepage: hero, why, features, how-it-works, download, FAQ. Hand-written static HTML/CSS (**not** the web-ui). |
| [site/docs.html](../../../site/docs.html) | The install/usage docs page (sidebar + sections). |

All three must agree on the **headline**, the **feature list**, and the **billing story**. A
change to one is usually a change to all three. Describe implemented behavior only: future
plans and other projects do not belong in the shipped feature list.

## Sources of truth (read these first — derive, don't remember)

| Source | Gives you |
|---|---|
| [docs/features/](../../../docs/features/README.md) | What the app *does* now, feature by feature — the derived truth. **Read this before writing any factual claim.** Honor its `Status: partial` flags. |
| [Product story](../../../docs/product-story.md) | Maintainer-approved framing and the reasons behind the page structure. |
| [docs/design/README.md](../../../docs/design/README.md) | Current positioning and dated rationale. Founding comparisons and roadmap entries are historical context, not proof that a capability shipped. |
| [AGENTS.md](../../../AGENTS.md) | The one-paragraph "what Chimaera is" — the canonical framing to stay consistent with. |
| Current code + `git log` since the last touch | Verify catalog claims and find shipped changes the copy has missed (see step 1). |

## Step 1 — compute the delta

Find when the public files were last meaningfully updated, then diff reality against them:

```sh
git log --oneline -8 -- README.md site/     # when was the public face last touched?
git log --oneline <that-sha>..HEAD          # what shipped since — scan for feat: and user-facing work
```

Two kinds of drift to hunt:

- **Missing capabilities** — a feature in `docs/features/` that the public copy never mentions.
  (Standing example: structured **chat mode** shipped and became the *default* agent view, but the
  site kept selling only "the real TUIs.")
- **Now-false claims** — a public statement the code no longer supports. Read each against the
  feature catalog. (Standing example: a blanket "agents bill exactly like a terminal" promise
  does not establish current provider policy for every chat surface. Verify billing claims
  against current official provider guidance before publishing them.)

## Step 2 — brainstorm positioning with the maintainer (required, never unilateral)

Positioning is product ground truth. Lay out the picture, then let the maintainer choose:

- **Current selling points** — pulled from the live copy.
- **The substance** — implemented capabilities that help someone direct agents, inspect
  and refine outputs, and return with project context. Include coordination, document/file
  workflows, history, Knowledge, and extensions; show local/remote reach where relevant.
- **The current decision** — [product-story.md](../../../docs/product-story.md) records the
  maintainer's whole-workspace framing: “A workspace for everything you do with agents.”
  Keep the audience broad, lead with use and value, and place recovery details in operating
  docs. The homepage describes capabilities positively; it is not an absence checklist.
- **The tensions** — distinguish the concrete workspace people open from the extension
  platform that lets it grow. Revisit genuine new choices with the maintainer rather than
  reopening settled wording on every factual update.

Ask the maintainer about genuine positioning forks using the available question tool or
plain chat — headline/tagline, which features to foreground, audience emphasis. Give a
recommendation and let the maintainer decide. Reuse decisions already authorized in the
session; record them in the PR body so the next run knows what was deliberate.

## Step 3 — apply, consistently

- README + index.html + docs.html tell **one story**. Facts trace to a feature page; positioning
  traces to the maintainer's step-2 calls.
- **Keep the brand and voice:** lowercase `chimaera` (the binary/product), "Chimaera" in prose,
  the "**workbench**" noun, the hexmark, curated light *and* dark. The site is hand-written static
  HTML — reuse existing components/classes (`.feature`, `.qa`, `.showcase`, `.codecard`); don't
  invent new CSS unless the design genuinely needs it.
- **Don't overclaim.** Verify current capabilities in code: git provides review, history and
  worktree management, with no commit/push UI. Claude Code, Codex, Antigravity and Grok Build
  have structured chat adapters; Gemini is retained for saved history, not new launches.
  Only Claude/Codex support verified terminal/chat switching and the Mastermind role.
  Closing a view or losing its connection leaves daemon-owned work running while its
  host and daemon stay alive. Restart restoration resumes supported conversations under
  the saved identity; it starts new processes, and interrupted work follows the restart
  settings. Compute jobs remain bounded by their allocations. Never promise work continues
  through sleep of the host running it or that every process survives a restart.

## Step 4 — verify (the UI-quality bar applies to the public face too)

- `node scripts/check-doc-links.mjs` — every relative markdown link + `#anchor` in the README/docs.
- `node scripts/check-site.mjs` — public-site links, metadata, structured data, social images,
  and sitemap consistency. CI and the Pages deployment both run this guard.
- Keep page titles and descriptions specific to each page, use canonical URLs in internal
  links and `site/sitemap.xml`, and update the versioned social image when the message changes.
  Structured data describes real project facts; never add invented ratings. Sample workspace
  content stays inside a static `div` with `data-nosnippet` so it cannot supply search snippets.
- **Eyeball it:** serve the static site and look at the hero, a new feature card, and the changed
  FAQ in **both** light and dark. Use the existing `site` entry in `.claude/launch.json`
  (`preview_start` → `preview_screenshot`) when that tooling is available, or run
  `python3 -m http.server --directory site` and open it with the available browser tools.

## Step 5 — ship

It's a **`docs:`** change → requests no release (an earlier releasing merge may
still be pending; see [ship-pr](../ship-pr/SKILL.md)). Title it
`docs: refresh public messaging — <the gist>`. The body says **what shipped since last time** and
**what positioning calls the maintainer made**, so the trail is legible for the next pass.
