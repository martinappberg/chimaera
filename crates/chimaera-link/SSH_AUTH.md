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
otherwise it is unsupported. An explicit destination-bound key attempt may
continue to keyboard-interactive MFA through the existing bounded askpass flow.
That prompt is admitted only after a native-verified key-signature receipt, is
confined to the original device/session and exact authentication context, and is
freshly authorized before publication and before an answer returns. Channel
loss, revocation or grant expiry cancels pending MFA. It never enables password-
only fallback after a failed signing attempt. Separate explicit password/Duo
connections retain their existing path, and no unattended monitor opens either
authentication channel.

## Common ProxyJump routes

This is a separate additive, disabled route contract. Authenticated capabilities
may add `proxyjump_v1:true` only after the route acceptance gates below. It also
requires the existing `hostbound_v1:true` and `register_only_v1:true` flags. An
absent/false flag, different version or missing route endpoint refuses before
registration or authentication. Ordinary tuple-only version 1 stays unchanged;
a client never submits a routed host to an older keeper or falls back to direct
SSH after a route refusal.

Inert `POST /v1/hosts` may add:

```json
{"alias":"cluster","ssh":{"hostname":"login.example.invalid","user":"person","port":22},"register_only":true,"ssh_route":{"version":1,"jumps":[{"hostname":"bastion.example.invalid","user":"visitor","port":2222}]}}
```

`ssh` remains the final destination. `jumps` is the complete resolved transit
order, from the first host reached by the keeper to the last bastion before the
final destination. The final destination is not repeated in `jumps`. At most
three jumps/four total endpoints are permitted. Every endpoint has an explicit
nonempty user, a positive `u16` port and the existing safe-atom hostname/user
bounds; duplicate canonical tuples and cycles refuse. The complete registration
body is at most 128 KiB. No endpoint, alias or field is an SSH directive, shell
command, local path or executable selector.

The returned protected host row must positively echo the exact `ssh` tuple and
`ssh_route` version/ordered jumps before the native client proceeds. Unknown or
missing acknowledgments refuse; a normal success status alone is insufficient.
Saving creates no SSH, prompt, daemon or cloud wake. A new host is offline.
Changing an existing route observes the existing held-master/job mutation
fences; refusal leaves that route intact. Omitted `ssh_route` preserves an
existing routed identity only when its final tuple is unchanged; it cannot
silently erase routing or retarget that host. An explicit negotiated empty
`jumps` list clears a route through the same mutation fences. Any route change
invalidates its pending authentication grants.

The native app delegates configuration resolution to bounded local `ssh -G`,
then transports only these resolved tuples. Common OpenSSH
`[user@]host[:port]`, SSH URI and comma-separated ProxyJump forms are supported
when they resolve to the bounded route. The effective configuration of every
jump is resolved separately. A nested ProxyJump is flattened before the endpoint
that uses it, retaining OpenSSH's actual connection order, with the same total
three-jump limit and cycle detection. It is never resolved afresh by the keeper.
Ambiguous or unrepresentable routing refuses explicitly. Arbitrary ProxyCommand,
VPN dependencies and Mac-local programs stay on the explicit advanced Direct
path, with its laptop connection lifetime. Local trust commands, revocation
policies and other unsupported settings retain their version 1 refusal behavior;
they are not dropped from the selected route.

Authenticated native-only
`POST /v1/hosts/{host_id}/ssh/auth/route-grants` accepts:

```json
{"version":1,"keeper_boot":"...","destination":{"hostname":"login.example.invalid","user":"person","port":22},"route":{"version":1,"jumps":[{"hostname":"bastion.example.invalid","user":"visitor","port":2222}]},"legs":[{"destination":{"hostname":"bastion.example.invalid","user":"visitor","port":2222},"mode":"interactive","host_keys":[{"key":"<base64 SSH public key blob>","is_ca":false}],"user_keys":[]},{"destination":{"hostname":"login.example.invalid","user":"person","port":22},"mode":"key","host_keys":[{"key":"<base64 SSH public key blob>","is_ca":false}],"user_keys":["<base64 SSH public key blob>"]}]}
```

There is exactly one ordered leg for every jump and then the final destination.
Each leg must equal that endpoint of the exact saved route. A leg's mode is
immutable for this explicit Connect: `key` requires the existing nonempty
selected user-key set and hostbound verification; `interactive` requires an
empty user-key set and the separately explicit password/keyboard-interactive
flow. Initial native selection may choose interactive when no usable local key
is selected, and the Connect presentation must disclose credential interaction.
It never chooses interactive after a key, binding, host-trust, agent constraint
or signature refusal. Such a refusal ends the whole chain.

Every leg requires this device's exact selected public host trust, including
certificate principal/validity and algorithm policy. Unknown or revoked trust
blocks both modes; an interactive leg is not permission to accept an unknown
host. Each leg retains the eight host-key/eight user-key and 16 KiB decoded-key
limits. The full request is at most 128 KiB; a selection that cannot fit refuses
without dropping keys or legs. Values, private keys and arbitrary SSH options
are absent. Password/MFA answers use the existing confidential transient keeper
prompt handling and are never placed in this request or durable state.
Interactive mode retains that trusted-keeper policy; it does not claim a
native-verified key signature or cryptographic proof of a password prompt's
source.

Success is
`{version:1,grant_id:<opaque id>,expires_in:180,destination:<exact tuple>,route:<exact route>,modes:<exact ordered mode array>}`.
The client validates the complete submitted identity/mode acknowledgment. The
memory-only grant has the same original device/session/account/boot binding,
unpredictability, absolute lifetime and four-per-device/thirty-two-per-keeper
grant ceilings as version 1. Its total connection budget remains thirty-two
and its shared in-flight signing budget remains eight; these are aggregate
limits for the whole chain, not multiplied per leg. No leg is independently
renewed or replaced. A changed saved tuple, order, mode or grant owner refuses.

Its dedicated authenticated native-only WebSocket is
`/v1/hosts/{host_id}/ssh/auth/route-grants/{grant_id}/ws`. The first exact Ready
frame adds `legs:<exact endpoint count>` to the version 1 Ready fields. The
client checks that count, the original keeper boot and this exact grant before
Reconnect. All existing upgrade, frame, queue, monotonic request and timeout
rules apply. Route bind/sign/closed requests and their replies additionally
contain `leg:<zero-based endpoint index>`. A connection identity permanently
belongs to one leg; closing it does not make that identity or its session
identifier reusable anywhere in the grant. An absent/out-of-range/changed leg,
cross-leg reply, duplicate binding or unselected key ends the grant. Key-mode
legs reuse the exact single-destination native verifier and local agent
constraints. The keeper's leg number does not authorize a signature. An
interactive leg accepts no bind/sign request and exposes no selected user keys
or agent connection.

Only explicit Reconnect may consume exactly one
`X-Chimaera-SSH-Auth-Route-Grant` header. It is mutually exclusive with
`X-Chimaera-SSH-Auth-Grant` and forbidden on every other endpoint. Before any
first-hop dial, the owned Connect task revalidates the complete current saved
route and live exact grant/control channel. An existing authenticated master
may be reused only through its captured complete route identity; it cannot
substitute for a changed route. There is no latest grant, per-account agent or
partial-chain fallback.

The keeper generates a private bounded OpenSSH configuration with opaque
per-leg aliases, strict selected public trust and one restricted IdentityAgent
context for each key-mode leg. Each jump process must read that exact generated
configuration: final-target command-line options are not assumed to apply to
jump hosts. Every stanza disables agent forwarding, private/certificate files,
agent additions, ambient trust/configuration and caller-selected custom commands. Key mode
requires hostbound public-key authentication; interactive mode disables
public-key/agent authentication. A key leg may continue to keyboard-interactive
MFA only after its own native-verified signature receipt. Prompts identify the
exact leg and immutable mode, remain confined to the original owner and are
freshly authorized against the full saved route before publication and answer
delivery. One leg's successful signature never licenses another leg's prompt.

Per-leg password/MFA context cannot be inferred from prompt text or inherited
from the final SSH process. A generated hop may invoke only the keeper's fixed
same-binary route-leg helper, with a closed bounded argument shape identifying
an immutable route-owned leg. The helper resolves the fixed SSH executable,
exact generated configuration, selected hop, stdio destination and opaque
askpass context from that live validated owner; it accepts no caller command,
script, environment, arbitrary configuration path or replacement destination.
It starts SSH with an argument vector and that leg's context. Any generated
OpenSSH proxy command is entirely keeper-authored from validated safe atoms;
arbitrary native ProxyCommand text never crosses this boundary. Helper startup
and its pending authentication are canceled with the same whole-route owner,
absolute deadline and retained cleanup budget. A missing, expired or replaced
context refuses before another SSH or credential prompt.

A route-owned `prompt` event retains the existing id/host/text/echo fields and
adds mandatory
`ssh_route_auth:{grant_id,keeper_boot,leg,mode,destination}`. It is delivered
only to the original device. Before showing credential UI, the native owner
checks every field against its live local route selection/grant and current
account generation; missing/changed metadata or no owning Connect refuses the
prompt. It displays the verified endpoint and whether this is initial
interactive authentication or key-mode MFA. The connection-local prompt id and
existing `answer`/`prompt_closed` messages remain unchanged. There is no answer
replay after control/event replacement. The keeper rechecks the original device,
full route, grant lifetime and that leg's mode/signature admission before an
answer returns to the exact waiting process.

The generated config, trust files, agent sockets, prompts and grant are one
owned first-Connect context. Cancellation, retarget, channel loss, sign-out or
expiry stops pending hop authentication and drains/removes its files under the
existing bounded ownership rules. Established target masters and their transit
connections outlive this short grant. The captured master identity includes the
complete ordered resolved route; later native configuration edits cannot change
its selection. Ordinary input, cluster workers, job forwards and their lifetimes
are unchanged. Passive reads, overview/facts, job streams and background
reconciliation use only that captured established master; a missing transit leg
never authorizes a new dial or credential prompt.

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

ProxyJump advertisement additionally requires real one-bastion and two-bastion
OpenSSH fixtures with distinct users, keys, ports and host trust/certificates,
plus bounded nested-route resolution and equivalent final master capture. Test
mixed key/interactive modes and per-leg MFA, refusal without authentication
downgrade, changed/revoked trust on every hop, cross-leg key/user/session/reply
substitution, retarget/sign-out/cancellation during an intermediate hop, exact
Ready and aggregate bounds. Prove no ambient private-key/configuration access,
no effects before a routed registration/grant acknowledgment, and no routed
request to an older keeper. Target/shared cluster worker/job sessions must
survive native disconnect and grant expiry; background activity after a missing
master must never reconnect. The common route contract does not certify arbitrary
Mac-local ProxyCommand/VPN compatibility or enable current services.

## Exact per-leg authentication policy

The separately negotiated, disabled `route_policy_v1:true` capability extends
`proxyjump_v1`. A client sending a policy requires both flags before any inert
registration or grant request. It never removes a policy to retry an older
keeper. Omitted/false capability or absent/mismatched policy acknowledgment
refuses before Reconnect. Existing policy-absent route version 1 retains only its
explicitly limited method behavior; it does not acknowledge a narrower policy.

Each submitted leg may add this immutable policy, captured from that leg's
bounded native effective OpenSSH configuration:

```json
{"policy":{"version":1,"methods":["publickey","keyboard-interactive","password"],"host_key_algorithms":["ssh-ed25519"],"ca_signature_algorithms":["ssh-ed25519"],"pubkey_accepted_algorithms":["ssh-ed25519"],"kex_algorithms":["curve25519-sha256"],"ciphers":["chacha20-poly1305@openssh.com"],"macs":["hmac-sha2-256-etm@openssh.com"]}}
```

Methods form a nonempty ordered list of at most three distinct closed enum
values: `publickey`, `keyboard-interactive`, `password`. A key leg requires
`publickey` first and may retain either or both interactive methods after it in
their configured order. Every later credential prompt still requires this
exact leg's native-verified hostbound signature, live original Connect owner and
fresh keeper authorization. If public-key authentication never produced that
eligible signature, enabling a configured password fallback does not grant a
prompt. There is no key-refusal downgrade. A key-only policy offers no credential
prompt. An interactive leg contains no `publickey` method or selected user keys
and retains its nonempty configured subset/order of password and
keyboard-interactive. Password-first mixed key policies are explicitly
unsupported rather than reordered. Methods locally disabled or omitted from
the configured preference list cannot be re-enabled by the keeper.

Each algorithm list is nonempty, at most 64 distinct resolved names, with names
at most 128 ASCII bytes consisting of letters, digits and `-@._+`. Leading
`+`/`-`, wildcards, comma-separated expressions, whitespace and SSH directives
refuse. The combined serialized policy is at most 8 KiB. Native resolution
expands local list modifiers; only complete resolved lists cross the wire. The
native verifier uses these same host, CA-signature and user-signature lists.
The policy also requires resolved `kex_algorithms`, `ciphers` and `macs` arrays
with those identical per-list and combined-policy bounds. Every generated
final/hop SSH process receives all six ordered CSV forms through fixed
argument-vector options. The keeper first captures one bounded supported-algorithm
snapshot from its fixed SSH binary, shared by every leg of this attempt. For each
list it emits only the intersection with that snapshot, preserving the native
order. Unsupported preferences are omitted; no algorithm is added or reordered.
Any empty list refuses before authentication leases, prompts or SSH effects. The
six closed `ssh -Q` queries share one five-second deadline and a 64 KiB aggregate
output ceiling, with owned cancellation-safe cleanup and capacity. No caller
query, executable, configuration or environment participates. The request and
acknowledgment retain the full original policy rather than the emitted subset.
Interactive legs enforce the same host, CA and transport restrictions even
though they request no native signature. This supports a newer native client's
defaults on an older keeper without silently widening a strict policy. It never
adds algorithms outside the native allow-lists, substitutes keeper defaults,
uses ambient trust, or enables another authentication method. No private keys, policy-selected executable,
configuration path or credential answer is
included. Policies are transient grant metadata, not account host settings.

A policy-bearing request includes a policy on every leg. Its grant response
adds `policies:<exact ordered policy array>` alongside the existing `modes`;
the client requires exact policy equality before consuming Ready or requesting
Reconnect. Mixed missing/present leg policies, malformed lists and absent,
shortened, reordered or changed response policies refuse. Existing total
128 KiB request/response and aggregate grant resource bounds remain unchanged.
The existing prompt provenance remains bound to the original grant/boot/leg/
mode/destination. Policy agreement is an additional gate, never a replacement
for original-owner, signature, trust or deadline verification.

Key-mode compatibility requires the server's positive
`publickey-hostbound@openssh.com` extension advertisement. OpenSSH introduced
that extension in 8.9; a version banner alone is not proof because vendors may
backport features. A server without it cannot use keeper-originated key-mode
signing on any leg. Even an otherwise valid signed session binding and matching
session identifier cannot authorize an ordinary unbound `publickey` signature:
the host identity must also be inside the signed user-authentication request.
OpenSSH's trusted-origin first-hop exception does not apply when the SSH client
and key exchange run on the remote keeper. This avoids the re-signing attack
described in [OpenSSH's destination restriction design](https://www.openssh.org/agent-restrict.html).
Explicit initially selected password/MFA may remain compatible with such a
server under its exact host trust/method policy; otherwise the advanced Direct
path uses the laptop's own SSH client and connection lifetime. No automatic
fallback occurs, and existing accepted masters are unaffected.

Before advertising policy support, disposable native/keeper/OpenSSH fixtures
cover key-only, password-only, keyboard-interactive-only and ordered
key-plus-MFA/password policies, every locally disabled method, exact policy ACK,
CA/host/user and KEX/cipher/MAC algorithm narrowing, and changed policies during a pending prompt.
A valid session binding followed by standard unbound public-key signing must
still refuse without consulting the local signing agent. These gates supplement
the complete route acceptance above; this contract does not enable support.
