# Optional mirrors and workspace ownership

This module owns daemon-side background mirrors and handoff. Parent:
[server map](../../AGENTS.md). It is inert until the native app provides a scoped,
revocable delegation over the authenticated local API.

| File | Responsibility |
| --- | --- |
| `mod.rs` | Bounded, credential-free persistent state, ownership/import fences, session pins and deferred-command policy. |
| `routes.rs` | Authenticated configure/status/privacy/profile/power/hydration HTTP handlers. |
| `engine.rs` | Independent lease renewal, mirror coordinator, transactional hydration, profile execution and lazy return. |
| `protocol.rs` | Additive account contract subset; intentionally no link/TLS dependency in the daemon. |
| `transport.rs` | Bounded external curl/git children; credentials only in memory, never argv or Git config. |
| `policy.rs` | Mirrored-path policy, credential filtering, size budgets and cloud-profile classification. |
| `mirror.rs` | Separate shadow and repository Git directories, incremental transfer and conservative hand-back. |
| `repository.rs` | Portable remote/tracking allowlist; bounded ref import, compare-and-swap adoption and index/ref-lock cancellation cleanup. |
| `config.rs` | Agent configuration export with credential fields removed and missing names recorded. |

One recorded holder and epoch controls shared writes. Failure to reach the service
is not evidence that ownership moved; keep local work available until a newer
owner is verified. Expired remote takeovers fork native conversations. Clean
handoff stops agents before final export and releases only after the mirror and
bundles are durable. A failed flush retains local ownership.

No account refresh token or agent credential enters this module. Delegations and
short-lived Git passwords are memory-only. Never log remote response bodies,
credential helpers, or secret-bearing structs. Filesystem work runs off the
reactor. Every directory walk, child output, transfer, queue and state map is
bounded. Shadow commits never touch the user's index or branch. Hand-back never
resets a dirty worktree or rewrites a divergent branch.

Hand-back fetches never overwrite `FETCH_HEAD`. Active-branch fast-forward holds
the real index reservation and a prepared Git ref transaction before touching
the working tree. Its bounded finalizer survives caller cancellation and installs
the matching index after a committed ref; ambiguous failures retain the prepared
index. Prepared transactions serialize so their helper cannot deadlock on the
two-child transport budget. Another worktree's branch is retained separately. Unsupported
transaction support preserves a cloud ref instead. Network Git has a finite
16-minute deadline; ordinary helpers retain short deadlines. Repository and
shadow histories are quota-bound and retained, never silently rewritten/pruned.
