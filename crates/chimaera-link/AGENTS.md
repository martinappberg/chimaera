# chimaera-link — optional device transport

| File | Responsibility |
| --- | --- |
| [PROTOCOL.md](PROTOCOL.md) | Versioned account/keeper contract; change it with code |
| [SSH_AUTH.md](SSH_AUTH.md) | Disabled-until-verified destination-bound native signing grants; no general agent forwarding |
| [CLUSTER.md](CLUSTER.md) | Capability-gated typed cluster control, job-scoped transports and account rollout job holds; implementation advertises only verified support |
| [HANDOFF.md](HANDOFF.md) | Additive baton, mirror credential and scoped daemon delegation contracts |
| [PROVIDERS.md](PROVIDERS.md) | Optional fixed personal-control login adapter and separate project-runtime provider authority; neither is enabled by contract publication |
| `src/continuity.rs` | Negotiated managed execution, immutable checkpoint receipts and exact recovery acknowledgments; recovery secrets have no Debug representation |
| [VIEWING.md](VIEWING.md) | Passive logical project routes, target acknowledgment, path aliases and installation binding |
| `src/placement.rs` | Exact passive placement/capability DTOs and native-only installation identity |
| `src/handoff.rs` | Typed ownership and credential bodies, immutable workspace binding and bounded exact daemon acknowledgment; secrets redact Debug |
| `src/fake_handoff.rs` | Bounded baton and credential-fencing fixture |
| `src/protocol.rs` | Serializable wire types and resource ceilings |
| `src/cluster.rs` | Capability-gated control DTOs, bounded request validation and exact reply/history identity checks; credential-bearing responses omit Debug |
| `src/fake_cluster.rs` | Immutable operation/dedup and job-scoped socket fixture; no SSH or scheduler implementation |
| `src/client.rs` | Account REST, refresh serialization, events reconnect with connection-local prompt IDs, loopback tunnels, reverse serve |
| `src/error.rs` | Typed outcomes callers must tell apart from network failures: `AuthorizationRevoked`, `ServiceUnsupported`, `AlreadySubscribed` (checkout conflict requires a fresh account read) |
| `src/oauth.rs` | PKCE and state validation; caller owns system browser and keychain |
| `src/bridge.rs` | Bounded bidirectional TCP/WebSocket pump and heartbeat |
| `src/transport.rs` | Origin policy, encoded path building, socket type |
| `src/fake.rs` | Loopback fixture; per-device event replacement, account-wide revocation; `fixtures` feature only |
| `src/conformance.rs` | Executable checks shared by fixture and real services |
| `tests/` | Security, lifecycle and real-socket regression coverage |

Invariants:

- Workspace-bound daemon configuration uses a distinct endpoint and exact
  version/binding/root acknowledgment. Never fall back to legacy configure on
  failure; an old daemon can silently ignore an additive delegation field.
  Omitted workspace binding retains legacy account-wide semantics. Consumer
  acceptance alone does not prove remote authorization or project isolation.
  The service-side scoped mint/revoke surface is optional and distinct;
  current worker startup configures `configure/execution` with an unbound
  worker grant. Consumer and account tests alone do not enable project isolation.
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
  `attended_actions:true` only relaxes `limited/hours_exhausted` for explicit
  actions; it never authorizes unattended continuation or overrides other limits.
- Constructing a client is inert. Closing/dropping an owning handle cancels its
  tasks and child streams. Never detach a tunnel/reverse task without an owner.
- Non-loopback requires TLS. Only literal `127.0.0.1` permits cleartext. Never
  follow authenticated redirects or downgrade a discovered keeper from HTTPS.
- Device tokens belong in the caller's OS keychain; refresh rotations must be
  persisted through `Client::token_updates`. Daemon tokens stay in memory.
  A started refresh rotation finishes under the token lock (bounded by the HTTP
  timeout) even if its caller is canceled; clearing credentials waits behind it.
  Debug output and errors must not print any token or SSH password. REST JSON
  parse failures use a stable error without the deserializer source: malformed
  typed fields can embed response secrets in Display/Debug/source chains.
- Every data message is ≤64 KiB, queues ≤16 frames, concurrent streams ≤128.
  Control frames are ≤128 KiB, ordinary REST bodies ≤1 MiB. Cluster requests are ≤64 KiB and protected replies ≤2 MiB. Backpressure is mandatory.
- Protocol v0 closes the entire stream on TCP EOF; do not silently invent a
  half-close marker or change daemon bytes to implement one.
- Handshake, heartbeat, prompt and pending reverse-stream deadlines are bounded.
  A reverse nonce is single-use and bound to the owning device/control generation.

Check with `cargo +1.96.0 fmt`,
`cargo +1.96.0 clippy -p chimaera-link --all-features --all-targets -- -D warnings`,
`cargo +1.96.0 test -p chimaera-link --all-features`, and drive the conformance
executable against the running fixture/real keeper when transport changes.

Events prompt IDs delivered by `Client::events` are opaque local aliases for that socket generation, not IDs to compare with a keeper REST response. Answers map back only through its bounded live prompt table (64 entries, 180 seconds, first answer only); an answer queued during backoff/upgrade or after a reused keeper ID is discarded. `PromptClosed` uses the same local alias. The public wire remains unchanged. The fixture replaces only the authenticated device's old event connection; global revocation still closes every device. Fixture token rotations retain that device identity in tables bounded by the current 256 access / 64 refresh token ceilings, including baton/delegation identity. A first delegation mint revokes nothing; replacing an existing delegation still uses the fixture-only global transport fence to preserve revoked-tunnel safety. `tests/events.rs` drives real loopback reconnect backoff, stalled upgrades and simultaneous devices.

Cluster clients require the explicit version/flags before control mutations or job sockets. A false/missing job-tunnel flag rejects submission before effects. Generic saved history is trusted only from the authenticated exact immutable operation record; start/stop/estimate and host replies additionally correlate their exposed identities. Unknown states never grant a route, prove an end or authorize resubmission. Passive cluster conformance performs only cached overview/facts reads; disposable fixture tests exercise submission and tunnels without invoking SSH/Slurm.
