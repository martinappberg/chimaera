---
topic: exhaustion
description: T-cell exhaustion signatures in tumour infiltrates.
created: 2026-07-01
last_updated: 2026-08-30
status: active
---

# Exhaustion

## F-001: TOX marks terminal exhaustion
**Status:** robust
**Claim:** A TOX+ PD1-high cluster appears in every tumour cohort.
**Implications:** Call terminal exhaustion on TOX, not PD1 alone.
**Tags:** [tcell, exhaustion]

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-07-02 | run-3 | melanoma | tumour | TOX+ PD1hi cluster | supports |
| 2026-07-20 | run-8 | nsclc | tumour | same cluster | supports |
| 2026-08-05 | run-12 | crc | colon | only in MSI-high | refines |

### Open Questions
- Does the cluster persist after checkpoint therapy?
- [x] Is it present in nsclc?

## F-002: TCF7 predicts response
**Status:** contradicted
**Claim:** TCF7+ CD8 fraction predicts checkpoint response.

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-07-05 | run-4 | melanoma | tumour | TCF7+ enriched in responders | supports |
| 2026-08-30 | run-15 | nsclc | tumour | no association | contradicts |

### Open Questions
- Is the melanoma association a cohort artefact?
- Is batch 2 confounded with tissue source?
