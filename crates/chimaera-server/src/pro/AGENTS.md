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
