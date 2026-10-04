# chimaera-link — public account and keeper contracts

This crate defines serializable protocol types and their bounded validators. It does not contain an account client, token refresh, sockets, background tasks, OAuth implementation or a fake service. The optional application extension supplies those implementations separately; the free app and daemon resolve without that extension.

| File | Responsibility |
| --- | --- |
| [PROTOCOL.md](PROTOCOL.md), `src/protocol.rs` | Versioned account/keeper wire, additive service decoding and resource ceilings |
| [HANDOFF.md](HANDOFF.md), `src/handoff.rs` | Ownership, credential and exact daemon-acknowledgment bodies |
| [VIEWING.md](VIEWING.md), `src/placement.rs` | Passive placement, exact identity validation and installation DTOs |
| [CLUSTER.md](CLUSTER.md), `src/cluster.rs` | Capability-gated cluster control and exact operation/history identities |
| [SSH_AUTH.md](SSH_AUTH.md), `src/ssh_auth.rs`, `src/ssh_auth/route.rs` | Bounded destination/route-bound grants and frame validation; no cryptographic authority |
| [PROVIDERS.md](PROVIDERS.md), `src/providers.rs` | Closed provider contract reexports from core |
| [PROJECT_SECRETS.md](PROJECT_SECRETS.md), `src/project_secrets.rs` | Write-only bounded secret commands and fixed outcomes |
| `src/continuity.rs` | Managed execution/checkpoint receipts and exact recovery acknowledgments |
| `src/error.rs` | Typed revoked, unsupported and already-subscribed outcomes |

- Preserve serialized wire shape through implementation extraction. Public types are reexported by consumers, not cloned into a second incompatible type family.
- Service responses evolve additively; unknown enum values never grant authority. Daemon acknowledgments and mutation request envelopes remain exact.
- A DTO, parsed frame, matching identifier or package receipt is not authorization. Runtime owners still enforce original account, operation, device, workspace and connection identity.
- Secrets have no printable Debug representation. Parse errors must not retain submitted credential values in error chains. Selected-project values are bounded, write-only and zeroized.
- Validation and constructors perform no account/network/keychain/process work. Passive placement never requests wake or execution.
- The optional client must enforce the documented TLS, deadline, quota, token rotation, no-replay and ownership rules. Contract publication does not enable any optional service capability.

Verify with `cargo +1.96.0 test -p chimaera-link` and `cargo +1.96.0 clippy -p chimaera-link --all-targets -- -D warnings`. Contract unit tests remain here. Client lifecycle, real-socket tests and executable service conformance travel with the optional implementation, outside the default public dependency graph.
