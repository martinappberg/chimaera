# chimaera-link — optional device transport

| File | Responsibility |
| --- | --- |
| [PROTOCOL.md](PROTOCOL.md) | Versioned account/keeper contract; change it with code |
| [HANDOFF.md](HANDOFF.md) | Additive baton, mirror credential and scoped daemon delegation contracts |
| `src/handoff.rs` | Typed ownership and credential bodies; secrets redact Debug |
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

- App-only dependency: the daemon must not depend on this crate. Fixture server
  dependencies are optional and never ship in a release bundle.
- Worker status is passive. Its optional preparing phase describes confirmed
  account state, never a percentage, a wake request or provider readiness.
- Constructing a client is inert. Closing/dropping an owning handle cancels its
  tasks and child streams. Never detach a tunnel/reverse task without an owner.
- Non-loopback requires TLS. Only literal `127.0.0.1` permits cleartext. Never
  follow authenticated redirects or downgrade a discovered keeper from HTTPS.
- Device tokens belong in the caller's OS keychain; refresh rotations must be
  persisted through `Client::token_updates`. Daemon tokens stay in memory.
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
