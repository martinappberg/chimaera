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
2. **What's the intended behavior, and what's incidental?** (What's a promise you'll keep vs an
   implementation detail that could change without anyone minding.)
3. **What non-obvious decision or constraint shaped it, and what did you deliberately leave out?**
4. **Is there anything a future agent must NOT "helpfully" change because it's intentional?**

Four questions. If an answer makes another redundant, fold them — don't pad to four. If the human
volunteers something outside these, keep it; the questions are a floor, not a cage.

## Writing the answers

Append to the feature page's `## Intent` section (below the derived/intent divider — keep the split
obvious; this content is human ground truth, structurally separate from the regenerable half). Use a
sub-heading naming the specific capability and a capture date, then the answers. For example:

```markdown
### <Feature> — why it exists
_Captured 2026-07-09 (from <who>, or "the maintainer")._

- **Problem it solves:** …their answer…
- **Intended vs incidental:** …
- **Deliberate decisions / left out:** …
- **Do not change:** …
```

Leave the "_No intent captured yet — pending…_" placeholder in place until you actually have
answers; replace it only with real ones.

## If the human isn't available

**Never fabricate intent.** Ship the page with the Intent section reading *pending* (the default
placeholder), and note in the PR that intent capture is outstanding. A missing Intent section is
honest; a guessed one is a landmine — a future agent will treat it as a constraint.
