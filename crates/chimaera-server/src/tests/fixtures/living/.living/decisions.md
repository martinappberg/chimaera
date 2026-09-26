# Decision Log

Append-only log of non-obvious decisions and their rationale.

**Entry template:** copy from `skills/core/templates/decision-log-entry.md` (includes Context, Decision, Alternatives considered, Rationale, Consequences, Tags fields).

## 2026-07-28: Keep the atlas objects on scratch

**Context**: Home quota is 15G; the atlas object alone is 11G.

**Decision**: Write `.h5ad` files under `$SCRATCH/atlas/`.

**Tags**: storage

### [2026-08-02] Use scran size factors over CPM

**Context**: Library sizes vary ten-fold across the two sequencing batches.

**Decision**: Normalise with scran pooled size factors.

**Alternatives considered**:
- CPM — ignores composition bias
- SCTransform — too slow on 200k cells

**Rationale**: scran corrects composition bias and scales to the atlas.

**Consequences**: Normalisation now needs a quick clustering pass first.

**Tags**: [normalisation, scran]

### [2026-09-12] Drop sample S14 from the atlas

**Context**: S14 failed QC in both batches.

**Decision**: Exclude S14 from all downstream analyses.

**Alternatives considered**: Keep S14 with a batch covariate — it still dominated PC1

**Rationale**: 38% mitochondrial reads; no rescue possible.

**Consequences**: The cohort is n=23.

**Tags**: qc, exclusion

### Keep raw counts in their own layer

**Context**: Normalised matrices overwrote `adata.X` twice.

**Decision**: Store raw counts in `adata.layers["counts"]` before any transform.

**Alternatives considered**: none

**Rationale**: Every downstream tool finds the raw matrix by name.

**Tags**: #anndata #provenance
