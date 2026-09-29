---
topic: depth
description: How deep libraries need to be sequenced.
last_updated: 2026-09-18
---

# Depth

## F-012 — Doublet rate does not scale with loading past 8k cells per lane (2026-09-18)

**Setup.** `scripts/doublets.py` over the pilot lanes (job 51973032), from `data/lanes.tsv`.
**Result.** Doublets plateau past 8k cells per lane. **This CORRECTS F-004.**
**Consequence.** Put to the user: load lanes at 10k and save a lane per batch? See D-3.

### F-012 RESOLVED (2026-09-20)

The next run loaded 10k per lane as decided.
