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
change to one is usually a change to all three.

## Sources of truth (read these first — derive, don't remember)

| Source | Gives you |
|---|---|
| [docs/features/](../../../docs/features/README.md) | What the app *does* now, feature by feature — the derived truth. **Read this before writing any factual claim.** Honor its `Status: partial` flags. |
| [DESIGN.md](../../../DESIGN.md) | The positioning spine: the problem, the "four-way intersection nobody ships," non-goals, and the dated decisions log (e.g. chat became the default view 2026-07-07). |
| [CLAUDE.md](../../../CLAUDE.md) | The one-paragraph "what Chimaera is" — the canonical framing to stay consistent with. |
| `git log` since the last touch | What shipped that the copy hasn't caught up with (see step 1). |

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
  feature catalog. (Standing example: the FAQ "agents bill exactly like a terminal" went stale the
  moment chat mode — the `-p stream-json` billing class — became the default.)

## Step 2 — brainstorm positioning with the maintainer (required, never unilateral)

Positioning is product ground truth. Lay out the picture, then let the maintainer choose:

- **Current selling points** — pulled from the live copy.
- **The substance** — the real differentiators from DESIGN.md: a no-root single-static-binary
  server *on* the host that owns the work · survive-disconnect / nothing-dies · attention-aware
  multi-agent · rich (scientific) previews · workspace-first (vs. chat-first).
- **The tensions** — has the product outgrown its framing? Is a negation-lead ("*not an IDE*")
  still earning its place at the top, or should it demote to a clarifier? Is the page leading with
  the right audience (general vs. HPC/science)?

Use **AskUserQuestion** for the genuine forks — headline/tagline, which features to foreground,
audience emphasis. Give a recommendation per fork *and* let the maintainer decide. Record the
decisions in the PR body so the next run knows what was deliberate.

## Step 3 — apply, consistently

- README + index.html + docs.html tell **one story**. Facts trace to a feature page; positioning
  traces to the maintainer's step-2 calls.
- **Keep the brand and voice:** lowercase `chimaera` (the binary/product), "Chimaera" in prose,
  the "**workbench**" noun, the hexmark, curated light *and* dark. The site is hand-written static
  HTML — reuse existing components/classes (`.feature`, `.qa`, `.showcase`, `.codecard`); don't
  invent new CSS unless the design genuinely needs it.
- **Don't overclaim.** Match `docs/features` honesty: git is *review-only* (status/diff + worktree
  create/remove — no commit/push); Gemini/Antigravity are detected but not first-class; chat
  sessions survive a disconnect but **not yet a daemon restart**.

## Step 4 — verify (the UI-quality bar applies to the public face too)

- `node scripts/check-doc-links.mjs` — every relative markdown link + `#anchor` in the README/docs.
- **Site anchors:** every sidebar `href="#x"` in `docs.html` has a matching `id="x"` (grep both).
- **Eyeball it:** serve the static site and look at the hero, a new feature card, and the changed
  FAQ in **both** light and dark. Either add a temporary `site` entry to `.claude/launch.json`
  (`python3 -m http.server`, then `preview_start` → `preview_screenshot`, and revert the entry
  before committing), or just `python3 -m http.server --directory site` and open it.

## Step 5 — ship

It's a **`docs:`** change → ships no release (see [ship-pr](../ship-pr/SKILL.md)). Title it
`docs: refresh public messaging — <the gist>`. The body says **what shipped since last time** and
**what positioning calls the maintainer made**, so the trail is legible for the next pass.
