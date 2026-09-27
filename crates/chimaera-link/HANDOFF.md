# Handoff extension v1

This additive extension to [Link v0](PROTOCOL.md) defines workspace ownership
and scoped Git access. Its independent `baton_version` is 1. Existing Link v0
connections and clients remain compatible. Requests use the account origin,
JSON, and the same device bearer authorization as `/v1/me`.

## Workspace baton

One workspace has one recorded writer. Workspace ids are stable across hosts.
A holder id is either the signed-in device id or a worker id authorized by its
service credential. The server derives the account and holder authority from
that credential; a caller cannot claim another holder by naming it.

| Method and path | JSON request | Result |
| --- | --- | --- |
| `GET /v1/baton/{workspace_id}` | — | Current baton |
| `POST /v1/baton/{workspace_id}/acquire` | `{ "holder_id": "device-id", "expected_epoch": 0 }` | Acquired baton |
| `POST /v1/baton/{workspace_id}/renew` | `{ "holder_id": "device-id", "epoch": 1 }` | Renewed baton |
| `POST /v1/baton/{workspace_id}/release` | `{ "holder_id": "device-id", "epoch": 1 }` | Released baton |

Every success returns:

```json
{
  "workspace_id": "w-12345678",
  "holder_id": "device-id",
  "epoch": 1,
  "expires_at": "2026-09-27T01:01:30Z",
  "server_now": "2026-09-27T01:00:00Z",
  "requires_fork": false
}
```

The initial baton has epoch 0, null holder/expiry, and `requires_fork: false`.
Acquisition compares the required `expected_epoch` atomically. Every ownership
transition on acquisition increments the epoch; acquiring again as the current
unexpired holder is idempotent. A different unexpired holder prevents acquisition.
Release verifies holder and epoch, clears holder/expiry, and preserves the epoch.
The next acquisition increments it. Renew verifies the same holder and epoch and
fails after expiry; it cannot resurrect an expired lease. Leases last 90 seconds;
clients renew every 5 seconds while active. `server_now` makes expiry interpretable
without trusting the client wall clock.

A mismatched epoch returns HTTP 409 with
`{ "error": "stale_epoch", "baton": <current baton> }`. An occupied baton uses
409 `held`; an expired renewal uses 409 `expired`; forbidden holder identity uses
403 `forbidden`. Invalid JSON/ids use 400. Epoch arithmetic must reject overflow.
Acquire/release may return 409 `mirror_commit_in_progress` for up to 10 seconds
while a previously verified push atomically publishes its refs. Retry with jitter
and the same expected epoch; renew remains available during that fence.

An expired holder remains visible in GET until the next successful acquisition.
Taking over that expired, unreleased baton increments the epoch and sets
`requires_fork: true`: the receiver must preserve the prior branch and create a
separate continuation. A clean release permits the next acquisition without a
fork. The flag remains attached to that ownership epoch, including renews; clean
release clears it. Service unreachability is not evidence of another owner: the
laptop may keep working offline. A later verified newer epoch fences shared
writes and must not silently resume the old cloud agent.

## Mirror credentials

`POST /v1/mirror/credentials` accepts:

```json
{ "workspace_id": "w-12345678", "epoch": 1 }
```

Omitting `epoch` requests read-only credentials. Supplying it requests write
credentials and must match the caller's current, unexpired baton holder and
epoch. Response:

```json
{
  "workspace_id": "w-12345678",
  "repository_url": "https://mirror.example/workspaces/w-12345678/repository.git",
  "working_tree_url": "https://mirror.example/workspaces/w-12345678/working-tree.git",
  "username": "scoped",
  "password": "opaque-short-lived-secret",
  "expires_at": "2026-09-27T01:15:00Z",
  "read_only": false,
  "storage_limit_bytes": 21474836480,
  "max_file_bytes": 100000000
}
```

The two remotes separately retain the user's repository history and shadow
working-tree snapshots; shadow commits never modify the user's branches.
Credentials are scoped to this account, workspace, permission and, for writes,
epoch, with lifetime at most 900 seconds. Git receive-pack revalidates the current
unexpired holder and epoch when accepting a push, so transferred ownership fences
an old token immediately. Read-only credentials cannot execute receive-pack.
Exhausted storage rejects writes without removing existing objects.

URLs contain no userinfo, query secrets or credentials and require HTTPS, except
literal loopback fixture endpoints. Passwords stay in memory and reach Git through
a scoped credential helper, never command-line arguments, persisted Git config or
logs. Redirects must not forward authorization to another origin. Secret-bearing
response bodies and token debug output must be redacted.

## Conformance

Implementations must exercise initial acquisition, CAS conflict, holder identity
binding, renewal, release/reacquisition, expired takeover/fork, and immediate
fencing of mirror writes after ownership moves. Run `link-conformance --handoff` against a compatible test account; add
`--test-hooks` only for the loopback fixture. The fixture implements baton and
credential issuance, with bearer-protected expiry and mirror-write authorization
hooks. Its mirror-write hook tests the fence; it is not a Git object store.
Private Git services additionally test real receive-pack quota and ref updates.

## Daemon delegation

An app can authorize its daemon to keep mirroring after the app exits without
sharing its rotating OAuth refresh token. `POST /v1/delegations` with `{}` and
a full device bearer returns:

```json
{
  "access_token": "opaque-scoped-secret",
  "expires_at": "2026-09-28T01:00:00Z",
  "scope": ["baton", "mirror", "keeper"],
  "device_id": "device-id"
}
```

There is one active delegation per device. Minting a replacement immediately
revokes the previous one. Its lifetime is at most 24 hours, capped at the parent
device's fixed refresh expiry. `POST /v1/delegations/renew` with `{}` and the
delegation bearer returns the same token with a new expiry under the same cap.
Renewal never extends the parent device's authorization lifetime. Daemons renew
hourly with jitter, keep credentials only in memory, and stop authenticated
background work on definitive 401/403. Network failure preserves local work.

The scoped token can access only baton, mirror and keeper transport operations,
plus its own renewal. It cannot read `/v1/me`, enumerate or revoke devices, access
billing, start OAuth or mint another delegation. The keeper introspector accepts
it as the same account and original device holder, restricted to the keeper
scope. Parent device revocation and sign-out-everywhere invalidate it and close
its keeper transport. Servers store only a token hash. The app passes this
credential only to its authenticated local daemon; it never persists the value
in configuration, logs, bundles or mirrors.

## Automatic takeover policy

After publishing a usable mirror, the current holder may PUT
`/v1/baton/{workspace}/policy` with
`{holder_id, epoch, handoff_enabled, offline_takeover, has_agents}`. It requires
an unexpired owned epoch and returns 204. Delegation credentials may publish the
same policy. A full device may DELETE that path to disable all three flags even
when another device holds the baton; scoped delegations cannot disable policy.

Policy survives ownership changes. Automatic worker wake considers an expired
**device** holder only, and requires all three flags, a published mirror, and
current entitlement and budget. An expired worker lease alone never wakes a
worker. These flags do not change the lease compare-and-swap rules or permit
active-owner takeover.

## Explicit worker wake

A full device may POST `/v1/worker/wake` with `{}` for a deliberate cloud action.
It returns 202 with `{worker_id, state, keeper_url}`: `worker_id` is nullable,
`keeper_url` is empty until assigned, and state is `pending`, `starting`,
`started`, `suspended`, `stopped`, or `retry`. Provisioning unavailable returns
503; entitlement or budget refusal returns 403 with a stable error code. The
account is derived from the device credential. Delegations cannot invoke it.
Directory/health polling must not call this endpoint.


Deleting the handoff policy also disables mirroring for that workspace account-wide:
existing derived Git grants stop working, and new read/write grants are denied.
Publishing another policy does not clear this privacy choice. Only an explicit
full-device POST `/v1/baton/{workspace}/enable-mirror` with `{}` may re-enable
credential issuance (204); daemon delegations and worker credentials cannot call
it. Re-enabling does not restore the automatic handoff flags.
