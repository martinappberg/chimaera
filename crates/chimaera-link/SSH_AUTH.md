# Destination-bound SSH authentication

Additive version 1 contract for an authorized native device to use selected local
SSH-agent keys during an explicit keeper Connect/Reconnect. It is separate from
cluster/job capability and ordinary password/MFA prompts. It does not claim
current services or the native app implement or advertise this capability.
Private keys stay on the device. This is not a general forwarded agent or a
remote signing API. Existing authenticated masters and job allocations outlive
the short authentication grant.

## Capability and inert grant

Authenticated `GET /v1/ssh/auth/capabilities` returns
`{version:1,hostbound_v1:true,register_only_v1:true,keeper_boot:<opaque identity>}`. A missing route,
missing/false flag or different version is unsupported. The keeper boot identity
is unpredictable and changes on each process start. Advertisement requires the
keeper, native verifier and adversarial end-to-end acceptance gates below.
Key-only first Connect also requires positive `register_only_v1`; an older keeper
may ignore unknown fields, so sending an inert-registration field without that
negotiation is forbidden.

Authenticated native-only `POST /v1/hosts/{host_id}/ssh/auth/grants` accepts:

```json
{"version":1,"keeper_boot":"...","destination":{"hostname":"cluster.example.invalid","user":"person","port":22},"host_keys":[{"key":"<base64 SSH public key blob>","is_ca":false}],"user_keys":["<base64 SSH public key blob>"]}
```

The destination must match the exact saved host's validated canonical SSH tuple;
user is explicit and nonempty. A device resolves its alias locally. The service
never accepts arbitrary SSH configuration, private keys, local paths or account,
device/session authority in this body. At most eight host trust entries and eight
selected user public keys are permitted. No duplicate keys or unsupported key
encodings are accepted. Each decoded key blob is at most 16 KiB and the complete
request is at most 128 KiB. CA trust requires the native verifier to validate the
presented host certificate's signature, destination principal and validity; a CA
blob alone is not permission for every host it signs. Public key comments carry
no authority and are omitted.

The response is `{version:1,grant_id:<opaque id>,expires_in:180}`. The id has at
least 256 bits of unpredictable entropy and is bound to the authenticated endpoint,
account, owning device, current session epoch, keeper boot, saved host identity
and exact destination/trust/key sets. Session epoch is server-validated authority,
never a claimed caller number. Retargeting the host, credential/session revocation
or a keeper restart invalidates it. Grant creation is inert: no SSH, socket dial,
cloud wake or credential prompt. Admission is bounded to four grants per device
and thirty-two per keeper; full admission fails before creating resources.

Authenticated native-only WebSocket
`/v1/hosts/{host_id}/ssh/auth/grants/{grant_id}/ws` activates that exact grant's
control channel. Device bearer authentication uses the authorization header,
never a URL token. A grant id is not a bearer credential. Reject browser origins,
delegated daemon credentials, a different device/session/host and replacement of
an already live grant socket. Upgrade and pre-dial authorization are revalidated;
revocation races close the channel before any signature can be accepted. Grant
ids and all packet contents have no Debug/log representation.

A reserved upgrade is not an active signing channel. After the upgrade and fresh
current-device/session/boot validation, the server sends exactly one first text
frame `{type:"ready",version:1,grant_id:<exact id>,keeper_boot:<exact boot>}`.
The native client retains the original selection's keeper boot, checks current
capabilities against that exact boot before upgrading, and validates Ready against
the same original boot before its socket API returns or Reconnect may
use that channel. Ready has a thirty-second maximum deadline within the grant's
absolute lifetime. Missing, duplicate/later Ready, unknown fields/kinds or a
mismatched identity fail closed. No request may precede Ready, and a paused or
failed upgrade cannot admit SSH authentication.

Authenticated `DELETE` of that same grant path is idempotent for its owner and
cancels pending authentication requests. Expiry, device sign-out, channel loss or
revocation does the same. They do not disconnect an already authenticated SSH
master, close job tunnels or cancel an allocation. No grant or auth packet is
written to the durable operation journal. Reusing a bound session identifier on
another connection in the same grant is rejected, not a fresh binding.

## Explicit use

Only explicit `POST /v1/hosts/{host_id}/reconnect` may carry exactly one
`X-Chimaera-SSH-Auth-Grant: <grant_id>` header. It selects only that grant and is
validated against the authenticated device/session, boot and exact saved/resolved
destination before an owned connect task starts. A valid grant requires its live
native control channel. No latest-grant or account-global lookup is permitted.
For a new key-only host the same user-visible Connect action first negotiates
`register_only_v1`, then sends `POST /v1/hosts` with the existing resolved alias/SSH
tuple and additive `register_only:true`. Default false preserves existing
behavior. A supporting keeper saves the exact host and returns its `offline` row
with no SSH task, master, daemon, prompt or cloud wake. The native device uses that
exact returned id to create the inert bound grant and explicitly Reconnect with
the header. An unsupported keeper is refused before registration; never trust an
old service to enforce an unknown field. Cancellation/signing failure leaves the
user's inert saved host reconnectable, with no automatic deletion. Host/destination
change between registration, grant creation and reconnect rejects the grant.

Every other endpoint, including `POST /v1/hosts` registration, rejects this header
rather than silently ignoring or broadening it. Registration has its separate
`register_only` field and cannot consume a grant bound to an existing host id. In particular polls, overview/facts refresh, operation history and
job streams cannot acquire a new signing context. Background monitors and refresh
use captured existing masters only and never reauthenticate. The grant installs
an exact-destination `IdentityAgent` socket and strict public host-key trust for
that connect task, with agent forwarding disabled. Removing the socket cancels
pending authentication; it leaves the established master intact. Per-Mac
“Connect directly from this computer” bypasses keeper routing as already defined
and remains available for sites/configurations the keeper cannot support.

## Bounded control messages

JSON text frames are at most 128 KiB, decoded SSH packets at most 64 KiB, queued
frames at most sixteen and in-flight signing requests at most eight per grant.
At most thirty-two agent connections belong to a grant. Every connection has a
keeper-minted opaque `connection_id`; `request_id` is a positive monotonic integer
shared by the grant. Replies match the exact live connection/request/kind, once.
Unknown kinds/fields, replay, out-of-order replies, duplicate bindings, oversize
or malformed packets fail closed with fixed errors. Each request has a thirty-
second deadline within the grant's absolute 180-second lifetime. Disconnect or
expiry drains pending requests with failure, never a usable partial signature.

The base64 packet contains the SSH agent message body starting at its message-
type byte; the four-byte Unix socket length prefix is excluded. The receiver
validates the complete body before adding framing on a local agent socket.

Keeper-to-native requests are:

```json
{"type":"session_bind","connection_id":"...","request_id":1,"packet":"<base64 exact SSH_AGENTC_EXTENSION packet>"}
{"type":"sign","connection_id":"...","request_id":2,"packet":"<base64 exact SSH2_AGENTC_SIGN_REQUEST packet>"}
{"type":"connection_closed","connection_id":"..."}
```

Native-to-keeper replies are:

```json
{"type":"bound","connection_id":"...","request_id":1}
{"type":"signature","connection_id":"...","request_id":2,"packet":"<base64 exact SSH2_AGENT_SIGN_RESPONSE packet>"}
{"type":"failure","connection_id":"...","request_id":2,"error":"key_unavailable"}
```

Failure codes are the closed allowlist `unsupported`, `invalid_binding`,
`invalid_request`, `key_unavailable`, `agent_refused`, `expired`, `revoked`,
`unavailable`. Unknown codes fail closed without showing remote diagnostics.
The keeper answers identities from the selected public keys locally. It refuses
agent add/remove/lock/unlock, arbitrary extensions, forwarding binds and every
other agent message. A connection requires one successful non-forwarding
`session-bind@openssh.com` before signing; neither service memory nor another
agent connection substitutes for that binding.

## Native verification and compatibility

Before accepting a bind the native verifier parses the complete exact packet,
requires `is_forwarding=false`, verifies the server's signature over the session
identifier with the presented trusted host key/certificate, and binds that
session identifier to this live grant/connection. There are no trailing bytes or
unknown extensions. The grant destination and public host trust come from this
device's resolved known_hosts trust or an explicit host fingerprint approval,
never from a server's unverified claim. Host-key changes require new trust and a
new explicit connect, never automatic acceptance.

Before consulting the local agent the native verifier parses the complete sign
request. Its key must be selected; SSH signing flags and algorithm must match the
key and requested hostbound algorithm. The signed bytes must be the exact SSH
session identifier plus SSH_MSG_USERAUTH_REQUEST for the grant's exact user,
service `ssh-connection`, method `publickey-hostbound-v00@openssh.com`, signature
present flag, selected public key and the already bound destination host key.
Reject plain `publickey`, arbitrary signing bytes, inconsistent algorithm/flags,
a different user/key/host/session and trailing fields. Only after verification
may the native app ask its local agent to sign that exact packet. When the agent
supports session binding, the accepted binding is also delivered on that same
local agent connection before signing, preserving its destination constraints; any agent
constraint, unlock/touch requirement or refusal remains authoritative. Returned
signature packets are parsed and correlated before delivery.

This follows the [OpenSSH destination restriction model](https://www.openssh.org/agent-restrict.html).
Session binding alone does not make arbitrary signatures safe. Actual client,
server and agent capabilities must support the required hostbound flow; version
strings alone are not proof. An unavailable/locked local key prompts local unlock
or gives a fixed local-only failure. Unsupported hostbound authentication gives
explicit password/MFA or direct-SSH guidance. Never silently forward an
unrestricted agent, copy a key, downgrade a refused request or reinterpret an
unbound signature as authorized. Native verifier support for any older local
agent must preserve all destination checks and that agent's own constraints;
otherwise it is unsupported. Password/Duo remains the existing bounded askpass
flow, and no unattended monitor opens either authentication channel.

## Acceptance before advertisement

Disposable fixtures exercise exact destination/device/epoch/boot binding,
retarget/revoke during paused upgrade/signing, inert registration/grant creation,
zero SSH before exact grant-bound reconnect, frame/key/
queue/admission ceilings, absolute expiry, replay and ordering, selected-key
isolation, malformed/trailing packets and every forbidden agent message. Real
OpenSSH/local-agent tests cover trusted host keys/certificates, hostbound server
support, locked and constrained keys, agent refusal and unknown extensions.
Adversarial native tests prove a malicious keeper cannot request a signature for
another user, key, session or destination. Existing password/MFA and direct SSH
remain functional. A real keeper/native Connect then interactive/batch allocation
survives laptop disconnect and grant expiry; reconnect requires a fresh explicit
grant while passive refresh never prompts. These are enablement gates, not claims
of completed testing in this contract-only checkpoint.
