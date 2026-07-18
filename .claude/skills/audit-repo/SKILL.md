---
name: audit-repo
description: Audit Chimaera end to end for correctness, security, performance, UX, developer experience, and agent-context drift, then land bounded high-confidence fixes with proportional verification. Use for broad repository health checks or modernization passes, especially when parallel subagents are requested.
---

# Audit the repository

Run a breadth-first audit without turning it into an unbounded rewrite. Preserve
wire compatibility, daemon resource ceilings, and the user's unrelated changes.

## 1. Orient and partition

1. Read the root `AGENTS.md`, snapshot `git status --short`, and inventory the
   repository, workflows, rules, skills, and nested `AGENTS.md` files.
2. Read every nested map and `.claude/rules/*.md` file that governs files you
   will touch. Verify documentation claims against the current tree.
3. When parallel agents are available and the user requested fan-out, give each
   a non-overlapping ownership lane. A useful split is:
   - Rust daemon, PTY, remote orchestration, and server
   - structured chat drivers, journal, protocol, and chat UI
   - non-chat web UI and native shell
   - root agent owns cross-cutting automation, dependencies, and integration
4. Tell every editing agent it shares the worktree, must preserve others' edits,
   and must implement only scoped, high-confidence fixes.

## 2. Audit by invariants

Prioritize evidence over cleanup taste:

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

## 3. Integrate and verify

1. Review the combined diff for conflicting assumptions and accidental wire or
   public-behavior changes. Update affected maps, rules, or feature docs.
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

3. Use `/verify-app` for runtime-observable changes. Run `just chat-smoke` only
   when a pinned agent protocol or CLI driver changed; it uses live credentials,
   network access, and billable turns.
4. Report fixes separately from residual risks. Include exact verification and
   explain anything that could not be run.

Prefer several small, defensible fixes over speculative cross-repo churn. Record
larger redesigns as follow-up findings with concrete evidence and boundaries.
