---
name: capture-feature-intent
description: Run a short questionnaire with the human to capture WHY a newly-shipped feature exists and what about it is intentional, then write their answers into that feature's Intent section in docs/features/. Use only when a feat: ships new user-facing capability. Never for fix/refactor/chore/docs. Never fabricate intent — if the human isn't available, the page ships with Intent marked pending.
---

# Capturing feature intent

The derived half of a [feature page](../../../docs/features/README.md) (what / how / where) comes
from code. The **why** — why the feature exists, what behavior is a promise vs an incidental
implementation detail, what a future agent must not "helpfully" change — cannot be derived from
code. It is human ground truth, and it is the single most valuable thing in the catalog, because
it stops a later agent from "fixing" something that was deliberate. This skill captures it.

## The gate — this is the whole point

Run this **only** when the change is a `feat:` — a genuinely new user-facing capability. "Feature"
is defined in exactly one place: the release mapping in
[`scripts/version-bump.sh`](../../../scripts/version-bump.sh) (a `feat:` subject → a minor bump).
This skill points at that definition rather than restating it, so the two can never drift.

- **Triggers:** `feat:` (and `feat!:`). The same bar as a minor/major version bump.
- **Does NOT trigger:** `fix:` · `perf:` · `revert:` · `refactor:` · `chore:` · `docs:` · `test:` ·
  `ci:` · `build:` · `style:`. Patches, cleanups, and tweaks get **no** intent questionnaire, ever —
  that gate is what keeps the Intent sections free of patch-level noise.

If you're unsure whether a change is a `feat:`, resolve *that* first (the
[ship-pr](../ship-pr/SKILL.md) skill owns the version-prefix decision) — don't run this on a maybe.

## The questionnaire (ask the human, verbatim answers)

Ask these directly, in the session, of the person who built or is shipping the feature. Keep it to
these — short is the point. Use their words; do not answer on their behalf or infer from code.

1. **Why does this feature exist — what problem does it solve for the user?**
2. **How settled is it — what's a real promise you intend to keep, versus "this is just how it works
   for now" and free to change?**
3. **What non-obvious decision or constraint shaped it, and what did you deliberately leave out or
   leave open for later?**
4. **Is there anything a future agent must NOT change because it's intentional — or is this area
   open to change?**

Four questions. If an answer makes another redundant, fold them — don't pad to four. If the human
volunteers something outside these (e.g. where the feature might go next), keep it; the questions are
a floor, not a cage.

**Don't assume the feature is mature.** "It's provisional — most of this could change; only the
*why* is settled" is a valid and valuable answer, not a non-answer. Capture it plainly: a future
agent needs to know what is **not** a locked contract just as much as what is. Questions 2–4 are
worded to welcome that — never push the human to invent firm promises or must-not-touch rules that
don't exist yet.

**Grade the intent: core bet vs addition.** A handful of decisions are genuine **core bets**
(don't-change). Most features are **additions to the core** — deliberate today but improvable, and
the maintainer's standing rule is *"don't be too strict about additions; they can change if
improved."* When you write the Intent, say which grade each point is, and reserve "do not change"
for the core. Over-freezing an addition is as harmful as under-recording a core bet.

(This wording is the result of pressure-testing the questions live on real features; the original
phrasing assumed more certainty than early features have, and lacked the core-vs-addition grade. See
the worked examples — [linked-terminals.md](../../../docs/features/linked-terminals.md) and the other
[feature pages](../../../docs/features/README.md) — and the "how to read Intent" note in the catalog
index.)

## Writing the answers

Append to the feature page's `## Intent` section (below the derived/intent divider — keep the split
obvious; this content is human ground truth, structurally separate from the regenerable half). Use a
sub-heading naming the specific capability and a capture date, then the answers. For example:

```markdown
### <Feature> — why it exists
_Captured <YYYY-MM-DD> (from <who>, or "the maintainer")._

- **Problem it solves:** …their answer…
- **How settled it is (intended vs provisional):** …what's a promise vs "for now"…
- **Deliberately open / where it may go:** …left out, left open, future direction…
- **Do not change (or: open to change):** …what's load-bearing, or plainly that it's open…
```

For a filled-in example, see the first entry in
[docs/features/linked-terminals.md](../../../docs/features/linked-terminals.md).

Leave the "_No intent captured yet — pending…_" placeholder in place until you actually have
answers; replace it only with real ones.

## If the human isn't available

**Never fabricate intent.** Ship the page with the Intent section reading *pending* (the default
placeholder), and note in the PR that intent capture is outstanding. A missing Intent section is
honest; a guessed one is a landmine — a future agent will treat it as a constraint.
