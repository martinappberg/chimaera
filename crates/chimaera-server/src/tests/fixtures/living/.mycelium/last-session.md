<!-- BEGIN MYCELIUM LIFECYCLE STATUS -->
Lifecycle status: accepted by Stop at 2026-09-15T18:02:11Z.
<!-- END MYCELIUM LIFECYCLE STATUS -->

SESSION RESUME — Last session (2026-09-15 17:40):

## What was worked on
- Re-ran QC on batches 1-2 with MAD cutoffs
- Drafted the methods section

## Key decisions made
- Dropped S14: 38% mitochondrial reads (see .living/decisions.md for full context)

## Blockers & surprises
- Unresolved: batch 3 FASTQs still not delivered
- None

## Current state
- Branch: `qc-mad` | Tests: 42 passing
- Stop hook finalization pending.

## Next steps
1. Request batch 3 from the sequencing core
2. Regress out batch before mito QC
- [x] Push the branch
