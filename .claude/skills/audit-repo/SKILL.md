---
name: audit-repo
description: Audit Chimaera end to end for correctness, security, performance, UX, developer experience, and agent-context drift, then land bounded high-confidence fixes with proportional verification. Use for broad repository health checks or modernization passes, especially when parallel subagents are requested.
---

# Audit the repository

Run a breadth-first audit without turning it into an unbounded rewrite. Preserve
wire compatibility, daemon resource ceilings, and the user's unrelated changes.
Keep four outputs apart: the inventory, the evidence, the fixes, and the accepted
risks. Do not report a suspicion as a finding until it has evidence.

## 1. Inventory

1. Read the root `AGENTS.md`, snapshot `git status --short`, and list the
   repository, workflows, rules, skills, and nested `AGENTS.md` files.
2. Read every nested map and `.claude/rules/*.md` file that governs files you
   will touch. Verify documentation claims against the current tree.
3. Build the feature/platform matrix. Take the features from the
   [feature catalog](../../../docs/features/README.md). Take the surfaces (local,
   remote, native app, cloud session) and the baseline counts from the inventory
   in [the robustness plan](../../../docs/design/robustness-and-agent-experience-plan.md).
   Mark each cell as covered, uncovered, or not applicable.
4. Assign each cell to one owner. When parallel agents are available and the user
   requested fan-out, give each agent a non-overlapping file-ownership lane. A
   useful split is:
   - Rust daemon, PTY, remote orchestration, and server
   - structured chat drivers, journal, protocol, and chat UI
   - non-chat web UI and native shell
   - root agent owns cross-cutting automation, dependencies, and integration
5. Tell every editing agent:
   - the worktree is shared;
   - preserve others' edits;
   - edit only inside your lane;
   - make only scoped, high-confidence fixes.

## 2. Collect evidence

Prioritize evidence over cleanup taste. For each finding, record the file, the
invariant it breaks, and how you observed it (command output, log line, test, or
reproduction). Audit these invariants:

- correctness and security: authentication boundaries, path/process handling,
  untrusted agent output, races, cancellation, reconnects, and bounded storage
- performance: busy loops, unbounded buffers, avoidable cloning or I/O, UI
  waterfalls, oversized bundles, and work repeated on hot paths
- UX and accessibility: keyboard/focus behavior, loading/error/empty states,
  responsiveness, theme consistency, and destructive-action affordances
- architecture: stable daemon-to-UI wire shapes, clear ownership boundaries,
  duplicated policy, dead code, and abstractions that remove real repetition
- agent experience: accurate maps, progressive-disclosure skills, matching
  Claude/Codex bridges, safe hooks, reproducible commands, and fast feedback
- supply chain: locked installs, minimal workflow permissions, immutable action
  pins, automated dependency updates, and dependency-diff review

When the user asks for current best practices, browse official primary sources
and distinguish verified guidance from inference. Do not mass-upgrade or refactor
solely because a newer version exists.

## 3. Fix and verify

1. Fix only findings that have evidence and a bounded change. Review the combined
   diff for conflicting assumptions and accidental wire or public-behavior
   changes. Update affected maps, rules, or feature docs.
2. Run the smallest relevant checks first, then the full applicable gates:

   ```sh
   node scripts/check-agent-assets.mjs
   node scripts/check-workflow-security.mjs
   node scripts/check-doc-links.mjs
   npm --prefix web-ui run check
   npm --prefix web-ui run test
   npm --prefix web-ui run build
   just check
   just app-check        # when native-shell code or automation changed
   ```

3. Use `/verify-app` for runtime-observable changes. Run `just chat-smoke` for
   Claude/Codex or `just chat-smoke-acp` for ACP when a pinned agent protocol or
   CLI driver changed; these use live credentials,
   network access, and billable turns.

## 4. Report

Write four separate lists:

- **Inventory:** what you covered, and which matrix cells you did not reach.
- **Fixes:** each fix with its evidence and the exact verification you ran.
- **Accepted risks:** each finding you did not fix, with the reason and the
  boundary of the follow-up. Prefer several small, defensible fixes over
  speculative cross-repo churn. Record larger redesigns here.
- **Not run:** each check you could not run, and why.
