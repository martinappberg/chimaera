# chimaera-link — optional device transport

| File | Responsibility |
| --- | --- |
| [PROTOCOL.md](PROTOCOL.md) | Versioned account/keeper contract; change it with code |
| [HANDOFF.md](HANDOFF.md) | Additive baton, mirror credential and scoped daemon delegation contracts |
| `src/continuity.rs` | Negotiated managed execution, immutable checkpoint receipts and exact recovery acknowledgments; recovery secrets have no Debug representation |
| [VIEWING.md](VIEWING.md) | Passive logical project routes, target acknowledgment, path aliases and installation binding |
| `src/placement.rs` | Exact passive placement/capability DTOs and native-only installation identity |
| `src/handoff.rs` | Typed ownership and credential bodies, immutable workspace binding and bounded exact daemon acknowledgment; secrets redact Debug |
| `src/fake_handoff.rs` | Bounded baton and credential-fencing fixture |
| `src/protocol.rs` | Serializable wire types and resource ceilings |
| `src/client.rs` | Account REST, refresh serialization, events reconnect, loopback tunnels, reverse serve |
| `src/error.rs` | Typed outcomes callers must tell apart from network failures: `AuthorizationRevoked`, `ServiceUnsupported` |
| `src/oauth.rs` | PKCE and state validation; caller owns system browser and keychain |
| `src/bridge.rs` | Bounded bidirectional TCP/WebSocket pump and heartbeat |
| `src/transport.rs` | Origin policy, encoded path building, socket type |
| `src/fake.rs` | Loopback fixture; `fixtures` feature only |
| `src/conformance.rs` | Executable checks shared by fixture and real services |
| `tests/` | Security, lifecycle and real-socket regression coverage |

Invariants:

- Workspace-bound daemon configuration uses a distinct endpoint and exact
  version/binding/root acknowledgment. Never fall back to legacy configure on
  failure; an old daemon can silently ignore an additive delegation field.
  Omitted workspace binding retains legacy account-wide semantics. Consumer
  acceptance alone does not prove remote authorization or project isolation.
  Today neither the account service nor the worker supervisor uses this
  surface (the supervisor configures `configure/execution` with an unbound
  worker grant); treat it as a dormant contract, not the worker path.
- Refresh: every 4xx from `/v1/oauth/refresh` except 404/408/429 is final
  (`AuthorizationRevoked`, credentials cleared, `None` published); the service
  answers `400 invalid_grant` and treats reuse of a rotated token as theft.
  Only a connection never established (`reqwest::Error::is_connect`) retries
  once with the same token; a timeout, a reply lost after sending, 404, 408,
  429 and 5xx keep the session without presenting the token again in that
  operation (the account may already have rotated it). A keeper 401 rotates
  only after the account itself rejects the access token.
- Service responses evolve additively: no `deny_unknown_fields` on
  service-originated types, `#[serde(other)] Unknown` on service enums, unknown
  host kinds/events/serve messages ignored. Daemon acknowledgments stay exact.
  A missing v2 route or another protocol major is `ServiceUnsupported`.
- `Client::plans` reads the public catalog (`GET /v1/plans`) with no bearer even
  when the client holds tokens, so it works signed out and never touches token or
  keeper state; a 404 (older service) is `Ok(None)`, other failures are ordinary
  errors, never a sign-in problem. Prices are presentation only. So are a
  `PlanPrice`'s optional `cloud_time_multiple` / `storage_multiple` (how many
  times Pro's monthly cloud time and storage the plan gives, whole numbers,
  same pair on both intervals, omitted by older services): a value that is not
  a positive `u32` (zero included) reads as `None` and never drops the row. The
  list carries no absolute allowance and this crate never learns one; only a
  subscribed account's own `limits` state numbers.
- One stream quota per client (forward tunnels and reverse serve together). A
  failed `accept()` backs off and continues; one serve message the client cannot
  act on never drops the control connection. The events task ends only when its
  consumer is gone; a slow consumer is resynchronized by reconnecting.

- App-only dependency: the daemon must not depend on this crate. Fixture server
  dependencies are optional and never ship in a release bundle.
- Optional portal targets are closed plan/interval pairs opening a hosted review,
  never direct subscription mutations or client-supplied provider price IDs.
- Native billing returns use an attempt-scoped loopback nonce; only authenticated
  account state confirms payment. Keep callback validation and PROTOCOL in sync.
- Worker status is passive. Its optional preparing phase describes confirmed
  account state, never a percentage, a wake request or provider readiness.
- Constructing a client is inert. Closing/dropping an owning handle cancels its
  tasks and child streams. Never detach a tunnel/reverse task without an owner.
- Non-loopback requires TLS. Only literal `127.0.0.1` permits cleartext. Never
  follow authenticated redirects or downgrade a discovered keeper from HTTPS.
- Device tokens belong in the caller's OS keychain; refresh rotations must be
  persisted through `Client::token_updates`. Daemon tokens stay in memory.
  A started refresh rotation finishes under the token lock (bounded by the HTTP
  timeout) even if its caller is canceled; clearing credentials waits behind it.
  Debug output and errors must not print any token or SSH password.
- Every data message is ≤64 KiB, queues ≤16 frames, concurrent streams ≤128.
  Control frames are ≤128 KiB, REST bodies ≤1 MiB. Backpressure is mandatory.
- Protocol v0 closes the entire stream on TCP EOF; do not silently invent a
  half-close marker or change daemon bytes to implement one.
- Handshake, heartbeat, prompt and pending reverse-stream deadlines are bounded.
  A reverse nonce is single-use and bound to the owning device/control generation.

Check with `cargo +1.96.0 fmt`,
`cargo +1.96.0 clippy -p chimaera-link --all-features --all-targets -- -D warnings`,
`cargo +1.96.0 test -p chimaera-link --all-features`, and drive the conformance
executable against the running fixture/real keeper when transport changes.
