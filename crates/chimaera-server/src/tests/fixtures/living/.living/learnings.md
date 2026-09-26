# Learnings

Append-only log of gotchas, surprises, and insights.

An entry looks like this (an example, not a learning):

```markdown
### [2026-01-01] Example learning inside a fence
**Category**: gotcha
**What happened**: This must never be read as knowledge.
```

### [2026-08-02] Scanpy drops genes with zero counts

**Category**: gotcha

**What happened**: `sc.pp.filter_genes` silently removed 1,204 genes before HVG selection.

**Why it matters**: Marker panels lose genes without any warning.

**Resolution**: Filter after subsetting the marker panel.

**Tags**: [scanpy, filtering]

**mitigation_type**: ambient-awareness

<!-- mitigation_type guidance:
  structural — a test has SHIPPED that enforces this class of error.
-->

**structural_mitigation_candidate**: assert the panel survives filtering in test_markers.py

source: atlas

### [2026-08-04] Slurm array OOM at 64G

**Category**: failure

**What happened**: The doublet step ran out of memory on the 200k-cell object.
It died building the kNN graph.

**Why it matters**: Re-runs cost a day of queue time.

**Resolution**: Request 128G for arrays above 150k cells.

**Tags**: slurm, memory

### [2026-08-11] Empty droplets in lane 3

**Category**: edge-case

**What happened**: Lane 3 had 40% empty droplets that passed the UMI floor.

**Why it matters**: They cluster together and look like a novel population.

**Resolution**: Run EmptyDrops per lane.

**Tags**: [droplets, qc]

### Cache the kNN graph between sweeps

**Category**: tip

**What happened**: Rebuilding the graph took 40 minutes per run.

**Why it matters**: Parameter sweeps were dominated by it.

**Resolution**: Persist `obsp` between sweeps.

**Tags**: performance
