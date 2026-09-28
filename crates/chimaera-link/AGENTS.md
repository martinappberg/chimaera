# chimaera-link — optional device transport

| File | Responsibility |
| --- | --- |
| [PROTOCOL.md](PROTOCOL.md) | Versioned account/keeper contract; change it with code |
| [HANDOFF.md](HANDOFF.md) | Additive baton, mirror credential and scoped daemon delegation contracts |
| `src/continuity.rs` | Negotiated managed execution, immutable checkpoint receipts and exact recovery acknowledgments; recovery secrets have no Debug representation |
| `src/handoff.rs` | Typed ownership and credential bodies, immutable workspace binding and bounded exact daemon acknowledgment; secrets redact Debug |
| `src/fake_handoff.rs` | Bounded baton and credential-fencing fixture |
| `src/protocol.rs` | Serializable wire types and resource ceilings |
| `src/client.rs` | Account REST, refresh serialization, events reconnect, loopback tunnels, reverse serve |
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
