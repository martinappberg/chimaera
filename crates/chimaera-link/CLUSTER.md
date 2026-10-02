# Kept cluster control and job continuity

Additive version 1 contract. Implementations advertise each capability only after
its acceptance gates pass. This document defines the wire; it does not claim a
keeper deployment supports it. Existing Jobs, job records and workspace hosting
remain the product model. Host-bound SSH signing is a separate, unadvertised
follow-up specified in [SSH_AUTH.md](SSH_AUTH.md).

## Negotiation and host policy

Authenticated `GET /v1/cluster/capabilities` returns:

```json
{"version":1,"cluster_control_v1":true,"job_tunnels_v1":true}
```

Missing route, another version, missing flags or false flags mean unsupported.
Clients never infer support from a daemon build, a host name or a successful
ordinary TCP connection. A keeper advertises `job_tunnels_v1` only after the
account acknowledges the rollout hold contract below. An old account must not
permit a new keeper to start held jobs accidentally.

`POST /v1/hosts` accepts optional
`cluster_policy:{login_serve:false,not_cluster:false}` alongside the existing
resolved `ssh` tuple. These reuse the saved host settings. A separate explicit
policy operation changes them on a kept host; reads never overwrite policy.
Per-Mac `direct_ssh` remains local and is never sent as account policy.

An SSH host may have optional
`cluster:{scheduler:"slurm",login_serve:false,not_cluster:false}`. It is null or
absent on ordinary hosts. Unknown schedulers are unsupported. A connected cluster
control host can have `daemon:null`: no daemon is implied on the login node.
`/v1/hosts/{id}/tcp` still addresses an ordinary or explicitly opted-in login-node
daemon; a control-only cluster answers `409 cluster_requires_job`.

Detection precedes ordinary daemon installation, including for saved hosts.
`not_cluster` is the existing explicit override. Without `login_serve`, an idle
cluster owns no indefinite login connection. A short control operation closes
its login master when no operation or job holds it. A held waiting/running job
keeps the authenticated login connection. Ordinary remotes may stay connected.
Two aliases sharing the same canonical SSH connection share its hold count;
closing one cannot invalidate the other's job.

Passive overview polling returns bounded cached state when an idle cluster has
no authenticated connection. It never opens SSH, requests a password/signature
or starts a cloud worker. Explicit Connect/Reconnect may authenticate. A deliberate
overview/facts refresh uses only the exact established SSH master; a missing
master requires Connect/Reconnect. It fences cached routes before waiting or
reading the scheduler and republishes only the verified current placement.
Background queue reads are shared across all devices with the existing
sixty-second floor. An explicit bounded, owned refresh may run inside that floor. Missing or failed scheduler
reads make the snapshot degraded; they never prove a job has ended.

## Typed control operations

Authenticated `POST /v1/hosts/{id}/cluster/operations` accepts a tagged `operation`
body from this allowlist. It never accepts a raw command, shell script, arbitrary
argv, environment or TCP destination. Startup and agent-rule text remain the
existing bounded, explicit user configuration; they travel on SSH stdin and are
never service diagnostics or persisted service request bodies.

| Operation | Request fields | Reply |
| --- | --- | --- |
| `overview` | `refresh:false` (true only for a deliberate refresh) | `overview` |
| `facts` | `refresh:false` | Existing cluster facts |
| `read_config` | — | Existing cluster config plus `config_sum` |
| `write_config` | `operation_id`, `expected_sum`, `config` | `saved` or conflict |
| `add_workspace` | `operation_id`, `path`, `name` | Existing cluster workspace |
| `remove_workspace` | `operation_id`, `workspace_id` | `saved` |
| `list_dir` | `path` | Existing directory listing |
| `start_job` | `operation_id`, `job_id`, `name?`, `spec`, `open`, `startup`, `attached`, `replaces?`, `save_as?` | `job` |
| `stop_job` | `operation_id`, `job_id` | `stop_pending` or `stopped` |
| `dismiss_job` | `operation_id`, `job_id` | `saved` (terminal jobs only) |
| `queue_open` | `operation_id`, `job_id`, `workspace_id` | `saved` |
| `set_startup` | `operation_id`, `workspace_id?`, `text` | `saved` |
| `forget_setup` | `operation_id`, `name` | `saved` |
| `set_agent_rules` | `operation_id`, `text`, `file?` | `saved` |
| `set_policy` | `operation_id`, `login_serve`, `not_cluster` | Updated host |
| `stop_login_daemon` | `operation_id` | Updated host |
| `start_estimate` | `job_id` | `estimate` with nullable epoch milliseconds |

Workspace/job ids use the existing cluster validators. Launch specifications and
configurations use the existing public cluster types and validation. Facts are
resolved by the keeper, never accepted as caller authority. The keeper prepares
the public cluster binary before a start; the caller cannot supply a binary or
installation path. Configuration writes retain the existing checksum CAS.
Removing a workspace edits its cluster listing and does not delete its folder.

An `overview` reply has the existing Jobs overview fields (scheduler, login node,
home, times, jobs, workspaces, other jobs, degraded state, config and startup),
plus protected native-only `config_sum`, `state_unreadable`, `records` and
`routes`. Records use the existing job-record schema. Routes are job/workspace
ids and `daemon` metadata only; node ports and SSH forwarding destinations never
come from a client. Daemon tokens stay in authorized native memory and never
enter Jobs presentation, saved preferences or logs. Snapshots are bounded to
2 MiB and 128 jobs/workspaces/routes. Job/workspace state values are extensible;
unknown values cannot be interpreted as terminal, ready or writable.

Jobs carry optional `stopping` (absent means false), matching the daemon's
scheduled-job view. A stopping job offers no new job/workspace route or opening
action. Stopping is not terminal proof: existing keeper job holds and SSH tunnels
remain until the scheduler positively confirms the exact allocation ended.

Every mutation uses a stable opaque `operation_id` scoped to account and host.
Start additionally uses a caller-minted validated `job_id`, allocated before
sending. `GET /v1/hosts/{id}/cluster/operations/{operation_id}` returns the exact
operation's state: `pending`, `completed` (with the original reply), `uncertain`
or `unknown`. Reusing an id with a different operation is `409 operation_changed`.
A lost reply must reconcile that exact operation/job. It must never mint another
id and silently submit a second allocation. Unknown or uncertain evidence
requires visible reconciliation, never automatic resubmission. Bounded operation
records cannot evict pending or uncertain submissions; full admission returns
429 before remote side effects. Remote launch records/unique scheduler identity
also fence a retry after a keeper restart. Completed replay performs no side
effect. Cancellation targets the exact job, not an SSH alias or a sibling.

Bodies are at most 64 KiB; startup and agent-rule text remain at most 32 KiB each
and must fit the body ceiling together. Control commands have finite deadlines;
submission/stop work is owned and reconciled if a caller disconnects. A service
returns fixed error codes, never SSH stderr, prompt answers or request text.
Authentication/revocation and source admission use the existing keeper rules.
Delegated daemons cannot mutate host policy or start/stop cluster jobs.

## Job transport and lifetime

Authenticated WebSocket data-plane routes are:

- `/v1/hosts/{id}/jobs/{job_id}/tcp`: the job's job-host daemon.
- `/v1/hosts/{id}/jobs/{job_id}/workspaces/{workspace_id}/tcp`: that workspace's daemon in the exact job.

They reuse the existing one-TCP-connection bridge, authorization revalidation,
heartbeat and aggregate stream quota. Only a verified route from that job's own
hosting state can be dialed; unavailable/moved routes answer 409. A workspace
moving between jobs gets a new exact route, never an alias to an old authority.
Job endpoints are nested resources and cannot be independently removed as hosts.
Existing job-host Open/Close/Move HTTP APIs travel over its authenticated tunnel;
no replacement Jobs UX or ownership transfer is implied by viewing/opening it.

The keeper owns batch and interactive job holds from submission through verified
terminal/absent state. A waiting allocation counts. Once ready, its compute-node
SSH forward is retained for the entire allocation, even with zero viewers. An
interactive job additionally owns the foreground `ssh -tt … srun` child and
bounded output drains. Device disconnection, laptop sleep and a direct-preference
change affect device tunnels, not the job holder or allocation. A second device
views the same job and never starts a second holder/submission.

Stop first requests the existing exact scheduler cancellation, then verifies the
job ended. Ambiguous scheduler/auth/network failure remains pending and holds
resources. After terminal proof, close workspace/job forwards, drain/stop the
attached child and release both routed SSH legs after their last shared hold.
Cleanup is owned, cancellation-safe and reports nonzero/spawn failures truthfully.
Removal of a kept host with held/unknown jobs refuses with `409 jobs_held`; the
user must explicitly stop/reconcile jobs before forgetting the host. Account-wide
revocation still closes logins; signing out one device does not close jobs.

Initial bounds are sixteen job holders globally and eight per host, within the
existing 128-host/stream, four-connect, 64-request and 32-prompt limits. A bounded
per-host actor serializes scheduler state and operation admission. Jobs output
uses fixed byte drains and small retained tails. Reconnects back off and never
request credentials repeatedly while no authorized device can answer. Keeper
crashes still end foreground SSH sessions; this promises laptop-disconnect
survival, not migration of live SSH processes between keepers.

## Account rollout hold contract

Authenticated `POST /internal/v1/keeper/report` accepts optional
`jobs:"idle"|"held"|"unknown"` alongside the existing held-work/volume report.
An account that durably stores and enforces it returns
`{"job_hold_v1":true,"jobs_revision":42}`. Missing/false acknowledgment, 204 or a missing route
means unsupported. Report negotiation precedes job admission/capability
advertisement. `held` includes waiting/running allocations and retained attached
children/tunnels; `unknown` includes unverified recovery or lost scheduler state.

The keeper reserves a local job hold and obtains an acknowledged `held` report
**before** any remote submission or attached child starts. It drains older
reports first and keeps that reservation through uncertain outcomes. Clearing a
hold requires terminal proof, not a disconnected caller or a failed request.

Authenticated keeper-only `GET /internal/v1/keeper/job-hold` returns
`{version:1,revision:<nonnegative integer>,jobs:null|"idle"|"held"|"unknown"}`.
Revision zero with null jobs means no supported report has been committed. A
report carrying `jobs` must also carry `jobs_revision` equal to that revision.
The first supported report is `unknown`; it arms persistent protection before
recovery or job admission. A successful conditional report increments the
revision and includes `jobs_revision` in its `job_hold_v1:true` acknowledgment.
A stale condition is `412 jobs_changed`, never an unconditional replacement.
Omitting `jobs` never changes its revision/state. Lost replies reconcile through
the getter, with no submission until a current held reservation is acknowledged.
Late idle reports therefore cannot overwrite a newer held/unknown reservation.

The getter and an accepted conditional `unknown` report acknowledgment may add
`empty_journal_recovery_v1:true`. Omission or false grants no recovery proof. The
account computes this field under the same cell/job-hold transaction: the
negotiated maintenance fence was reserved from authoritative `idle`, the exact
original provider instance/effect is settled, and the authenticated current
keeper credential matches the dedicated rollout-key reconstruction for that
account, machine and fence. Legacy revision zero, generic unfenced `unknown`,
prior `held`, incomplete/ambiguous replacement and a different credential never
qualify. No timeout, image match or absence of viewers substitutes for this proof.

The keeper consumes proof only when both the locked getter at revision r and the
exact conditional `unknown` acknowledgment at r+1 qualify in one arm attempt.
The account recomputes qualification in that report transaction; a changed
credential, scope or fence cannot reuse an earlier true field. A lost response
restarts this bounded proof exchange with a fresh getter, never a stored bare
boolean. This permits an empty durable job journal with no children, forwards or
in-flight open/cleanup resources to reconcile maintenance `unknown` to `idle`.
Any nonempty job/resource evidence still needs the ordinary scheduler and cleanup
proofs. The subsequent conditional `idle` report remains the only action clearing
that exact maintenance fence.

Account report acceptance and automated keeper-rollout reservation are mutually
exclusive durable transactions, bound to the current keeper machine. Rollout
must reserve its restart while checking the job state under the same database
fence used by report acceptance; a read followed by an unfenced provider call is
insufficient. A report attempting job admission after rollout owns that fence
gets `409 rollout_pending` before SSH side effects. The rollout reservation
persists through the provider operation and uncertain outcomes, and is cleared
only after reconciliation proves that admission is safe. A timeout or expired
process lease does not by itself reopen admission. Every automated restart path
(including urgent/overdue rollout) observes this fence. Ordinary bookkeeping
reports cannot release it or erase negotiated job protection.

Once an account records a supported job state, older reports omitting `jobs`
cannot clear that state or its negotiated protection. A held/unknown job state or
a stale/missing supported report refuses every automated running-keeper restart,
including an expired normal/urgent rollout deadline. Only a fresh authoritative
`idle` report permits the existing rollout logic to run. Its seventy-second
freshness window is unchanged. Job protection is separate from ordinary SSH
activity, whose existing bounded rollout policy remains. Reports contain no
host, job, project or credential identities. Restart recovery reports unknown
until all saved job evidence has been reconciled; absence of a live viewer is
never idle proof. Old keepers that never negotiated retain legacy behavior.

## JSON envelopes and correlation

Control requests use the `operation` string as their JSON discriminator. Example:
{"operation":"start_job","operation_id":"op_123","job_id":"j-0000abcd","name":null,"spec":{"time":"1:00"},"open":[],"startup":"","attached":false,"replaces":null,"save_as":null}

An operation id contains 1–128 ASCII letters, digits, underscores or hyphens. It is opaque, account/host scoped, and has no path or credential authority. Existing core validators continue to own job/workspace ids. Requests reject unknown operation kinds and fields; service replies accept additive fields and retain unknown discriminator/state values as unsupported evidence.

Replies use `result` as discriminator:
- overview: {"result":"overview","overview":<existing overview fields plus config_sum/state_unreadable/records/routes>}
- facts: {"result":"facts","facts":<existing cluster facts>}
- read_config: {"result":"config","config":<existing ClusterConfig>,"config_sum":"..."}
- generic saved mutations: {"result":"saved"}
- list_dir: {"result":"directory","directory":<existing directory listing>}
- add_workspace: {"result":"workspace","workspace":<existing cluster workspace>}
- start_job: {"result":"job","job_id":"j-0000abcd","slurm_job_id":null,"attached":true}; a positively refused batch submission may instead return {"result":"refused","job_id":"j-0000abcd","refusal":"batch_not_allowed"}
- stop_job: {"result":"stop_pending","job_id":"j-0000abcd"} or {"result":"stopped","job_id":"j-0000abcd"}
- set_policy/stop_login_daemon: {"result":"host","host":<existing host plus cluster metadata>}
- start_estimate: {"result":"estimate","job_id":"j-0000abcd","at_ms":null}

A `refused` completion is valid only for the exact immutable operation and
stable job ID of `start_job` with `attached:false`. Its closed reason is
`batch_not_allowed`, `account_required`, `qos_required`, `constraint_required`
or `other` (the existing scheduler-refusal classifications). Missing/unknown
reasons, a different job, an interactive start or a receipt reporting a scheduler
ID are not positive non-submission proof. The service may produce it only from
an intact nonce-framed result of that fresh batch attempt: the scheduler command
positively rejected submission with a recognized policy result, no allocation
ID was returned, and no child,
compute tunnel or other allocation resource was acquired. A scheduler ID combined
with an error, an already claimed start, a lost/incomplete receipt and arbitrary
SSH/process failure remain uncertain and keep their hold. An empty-ID/nonzero
`sbatch` exit alone is insufficient: receive timeouts can occur after controller
acceptance. Current stable remote proof accepts only exact C-locale controller
account/QOS/feature rejection diagnostics; generic/site-specific text (including
`other` guidance) remains uncertain until another authoritative proof exists.
This distinction follows the separate controller-response and communication-error
paths in [SchedMD submission code](https://github.com/SchedMD/slurm/blob/master/src/api/submit.c)
and the shared failure exit in [sbatch](https://github.com/SchedMD/slurm/blob/master/src/sbatch/sbatch.c).

The keeper durably records that fresh refusal as a terminal non-allocation and
its operation as completed before releasing the exact submission reservation.
A persistence/cleanup/report failure retains protection. It must never relabel
an already submitted, held or previously uncertain allocation from a later error
or different attempt. Exact operation retries/history replay the fixed refusal
without invoking SSH again. Only the bounded reason/job ID is stored or sent;
raw scheduler stderr, startup text and credentials are not refusal journal data.
Native clients display fixed guidance and retain the existing refusal field for
the Jobs start sheet. Mixed-version/unknown replies stay unresolved, with no new
submission or resource cleanup inferred from an unsupported result.

A fresh explicit batch start may first run one bounded Slurm `sbatch --test-only`
check with the same script and normalized launch arguments, using its captured
existing master. [Slurm documents this as no actual submission](https://slurm.schedmd.com/sbatch.html#OPT_test-only);
its controller path still performs site job-submit validation. This is never a
poll, a retry of an immutable operation or a new authentication attempt. A
successful check is only permission to proceed with the one real submission;
it does not establish that submission's eventual outcome.

An intact fresh nonce-framed **preflight-refusal** receipt may also complete
`refused`: the generated command must positively stop before invoking real
submission, with no allocation child or tunnel acquired. The bounded reason is
presentation guidance, including `batch_not_allowed` for a site policy or `other`
for an unavailable preflight; raw diagnostics are neither journal data nor
non-submission authority. Proof comes from this distinct script phase and known
zero real-submit effects. A missing, malformed, timed-out or lost phase receipt
remains uncertain because the script might later proceed. The preflight never
reclassifies an earlier real submission or an existing uncertain claim. Both
paths use the same atomic completed-operation/non-allocation terminal record
before exact reservation release. Unsupported or nonconforming site wrappers
must not be treated as verified dry-run support.

Protected overview routes are a list of {job_id,workspace_id?:<id>,daemon:<existing Daemon>}. An omitted workspace_id addresses the job-host. They contain no node, port or client-supplied SSH destination. Daemon credentials have no Debug representation and remain native RAM only.

Operation history replies are {"state":"pending"}, {"state":"uncertain"}, {"state":"unknown"}, or {"state":"completed","reply":<original typed reply>}. A completed state without a validated reply is not completion evidence. Unknown values never trigger resubmission or job cleanup.

Fixed control errors are {"error":<code>} without SSH diagnostics. Codes include operation_changed, cluster_requires_job, jobs_held, job_unavailable, jobs_changed and rollout_pending. The reservation uses the published rollout_pending spelling; no alias is implied.

Client serialization enforces the 64KiB request ceiling before sending, validates existing launch/config/id fields, and reads cluster replies under their explicit 2MiB ceiling. The ordinary 1MiB REST ceiling stays unchanged. Missing/false/version-mismatched capabilities fail before mutations and job socket creation; absence is never inferred from arbitrary errors on a submitted mutation.

Clients correlate replies before accepting them: start/stop/estimate job_id equals the exact requested job id; completed history replies match the exact retained operation kind and its requested identities. A job mutation never accepts a different id merely because it is otherwise valid. Unknown or mismatched replies leave the operation unresolved. All enclosing route/snapshot/reply/history types that can contain daemon credentials omit Debug or explicitly redact those values. Durable replay records contain sanitized non-secret results only; protected daemon routes are rebuilt from current verified state in RAM and never replay a persisted bearer.

A generic `saved` history reply is authoritative only because the authenticated history URL names the exact immutable account/host operation record. It must never be synthesized from a latest-job result or unrelated cache. Request hashes remain internal deduplication state; they are not a client wire requirement.

Keeper lifecycle cleanup requires positive terminal scheduler evidence for the exact stable job identity. The routed SSH terminal probe checks successful bounded sacct allocation output against the full deterministic job name, scheduler id when known and current SSH user UID; a matching successful scontrol job record can supply evidence when accounting is unavailable. Fresh nonce framing and exact terminal states are required. Failed, missing, malformed, mismatched or conflicting observations preserve uncertainty; queue absence, a disconnected viewer, an exited SSH client and the existing presentation-only ENDED fallback never release a job hold. Account Unknown and maintenance protection are separate from transport retention: uncertainty alone does not authorize an indefinite idle HPC login, while known live allocations retain their job tunnels and the explicit login_serve override remains available.

Terminal accounting queries start at the recorded submission time with ten minutes of clock allowance and never query more than thirty-one days of history. Missing/zero, implausibly future or older submission timestamps skip accounting rather than scanning whole history; an exact known scheduler-id/name/UID scontrol observation may still prove termination, including for jobs longer than thirty-one days. Without either bounded accounting or a known scheduler id, uncertainty requires explicit recovery. Duplicate matching allocation records remain ambiguous even when their terminal labels agree; scheduler-id recycling, requeue history or name ambiguity cannot silently release a hold.

The account-internal saved-host record accepts optional cluster_policy:{login_serve:boolean,not_cluster:boolean}, containing only these existing policy choices. An older omitted policy preserves the stored choice only when alias and SSH hostname/user/port identity are unchanged; retargeting or replacing that identity clears inherited policy unless the new request explicitly supplies it. New keepers read back the exact host record after policy writes and require matching policy acknowledgment, so an old account ignoring an additive field cannot silently promise login-node opt-in persistence. Every policy change remains authenticated and bound to the exact saved account/host; host deletion, retargeting and reconnect cannot abandon live job holders.
