---
topic: batch-effects
description: How sequencing batch shifts expression estimates across runs.
created: 2026-08-01
last_updated: 2026-09-10
status: active
---

# Batch Effects

## F-003: Batch 2 inflates the mitochondrial fraction
**Status:** supported
**Claim:** Libraries sequenced in batch 2 show a ~4% higher mitochondrial read fraction than batch 1 after QC.
**Implications:** Regress out batch before comparing mito-based QC across runs.
**Tags:** qc, batch, mitochondria

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|------|-------------|---------|---------|--------|-----------|
| 2026-08-14 | run-17 | pbmc-10k | atlas | +4.1% mito in batch 2 | supports |
| 2026-09-10 | [run-22](../log/2026-09-10-001-qc.md) | pbmc-20k | atlas | +3.8% mito in batch 2 | supports |

### Open Questions
- Is batch 2 confounded with tissue source?
- {Gap in evidence that needs more data}

## F-004: Doublet rate scales with loading density
**Status:** preliminary
**Claim:** Doublets rise roughly linearly with cells loaded per lane.
**Tags:** #doublets #qc

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|------|-------------|---------|---------|--------|-----------|
| YYYY-MM-DD | {session-id} | {dataset} | {project} | {what was observed} | supports |

### Open Questions
- None — the loading curve is the next run.
