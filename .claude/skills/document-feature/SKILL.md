---
name: document-feature
description: "Add or update a Chimaera feature page under docs/features/ so the feature catalog stays true as capabilities ship. Use when a change adds or changes user-facing behavior — a new pane surface, route, agent capability, CLI command — or when you find a feature page that's stale or missing. Defines the page structure, the derived-vs-intent split, and the rule that a feat: PR must carry its doc update."
---

# Documenting a Chimaera feature

The [feature catalog](../../../docs/features/README.md) is what tells an agent (or a person)
what the app *does*, feature by feature, and where each capability is wired — so it can be
located and extended without re-exploring the tree. It only stays true if updating it is part
of shipping. This skill is how you do that.

## When this runs

- **Always for a `feat:`** — a genuinely new user-facing capability (the same bar as a minor
  version bump, defined once in [`scripts/version-bump.sh`](../../../scripts/version-bump.sh)).
  A `feat:` PR **must** carry its feature-page update; the [ship-pr](../ship-pr/SKILL.md) flow
  checks for it. A `feat:` also runs [capture-feature-intent](../capture-feature-intent/SKILL.md).
- **When you change existing user-facing behavior** in a way a page describes (a new route, a
  changed flow, a new constraint) — update the page in the same change. The
  [doc-drift hook](../../hooks/doc-drift.sh) warns (never blocks) when a feature's entry points
  changed but its page didn't.
- **Not for** pure internal refactors, chores, or fixes with no user-visible change — those
  touch the derived facts at most (update a path if it moved), never the Intent section.

## Which page

Map the feature to an existing page using the current
[index](../../../docs/features/README.md) table. Add a new page only
for a genuinely new capability area (a whole new subsystem) — then add its row to the index
table **and** a feature entry in `docs/features/registry.json` (paths, maps, rules, checks) — the
[doc-drift hook](../../hooks/doc-drift.sh) and CI's change-impact job read it. For an existing
page, add or extend its entry there. Then run `node scripts/check-feature-registry.mjs --write`.

## The page is a feature contract (derived vs intent — the split is load-bearing)

A page states a contract: what the feature promises a user, what it depends on, and
how to extend and check it. An agent derives everything above the `## Intent` divider from code and may
regenerate it. Keep it lean — this is on-demand reference, not
always-on context. For each feature:

- **What & when** — one or two lines: what it does and when you would use it.
- **How it's used** — the user behavior: the concrete flow through the Web UI /
  daemon / CLI, enough to operate or demo it.
- **Where it lives** — the code entry points **and** the daemon↔UI touch points (the
  routes / WS channels / IPC commands), plus the other features and services it
  depends on. This is what makes the page worth more than reading the code cold.
- **Key behaviors / constraints / edge cases** — the non-obvious rules (auth, caps,
  error states, resource discipline) a code reader would miss.
- **How to extend it** — the steps to add a variant (a setting, a preview type, a
  provider), as links to the owning area map instead of repeated procedure.
- **Acceptance checks** — the commands and live scenarios that prove the feature
  still works (a test name, a `just` target, a **verify-app** scenario).
- Flag honestly: **Status: partial** for a half-built capability; and where a *why* can't be
  derived from code, leave it for the Intent section rather than guessing.

Write in the internal style of the
[robustness plan, section 6](../../../docs/design/robustness-and-agent-experience-plan.md#writing-rules)
(ASD-STE100 profile): one instruction per step, one main idea per sentence, active voice,
conditions before the action. Use the stable terms workspace, session, window, pane,
host, job, and provider. Keep API identifiers, commands, and filenames exact. Keep
requirements, observed behavior, and history in separate places.

Below the divider is the **`## Intent`** section — human-authored ground truth, **never**
generated or inferred from code. You do not write it from your own reasoning; it is filled only
by [capture-feature-intent](../capture-feature-intent/SKILL.md) from the human's answers. A new
page ships with Intent reading *pending*.

Match an existing page's shape (e.g. [linked-terminals.md](../../../docs/features/linked-terminals.md)
is a compact two-feature page; [chat-mode.md](../../../docs/features/chat-mode.md) is a large one) —
same headings, same derived/intent divider, same footer block.

## Verify before you claim it

- Every path, route, and command you write must resolve against the code **now** — derive it,
  don't remember it. `node scripts/check-doc-links.mjs` catches broken markdown links + anchors.
- The page is reference material: it should be reached on demand, not duplicated into the index
  or a `AGENTS.md`. Only the [index](../../../docs/features/README.md) is pointed at from the root
  [AGENTS.md](../../../AGENTS.md).
- **Semantic review.** After the paths resolve, read each claim against the code and
  ask: does the page still describe what the code does? A link check cannot answer
  this. Check the user flow, the constraints, and the acceptance checks one by one.
  Fix every claim that no longer holds, in the same change.

## When no doc change is needed

If a change touches a feature's entry points but its behavior did not change (a pure
refactor, a rename that kept every route and flow, a test-only change), do not edit
the page to satisfy the [doc-drift hook](../../hooks/doc-drift.sh). Write one sentence
in the PR: which feature page you reviewed, and why its described behavior is unchanged.
A meaningless edit is worse than this explanation. An unchanged page with no explanation
reads as forgotten.
