# web-ui/src/lib/workspace — workbench domain surfaces and stores

Orientation for coding agents. This area owns the workbench's domain data and the surfaces that show it: sessions and workspaces, the files tree, git, the agent launcher and agent setup, Home, clusters and compute, Timeline, Knowledge, session history, agent communication, notices, and updates. Each store mirrors daemon state; the daemon stays the authority. It does not own the pane layout (`layout/`), file views (`previews/`), chat (`chat/`), or the dashboard's composition (`dashboard/`). Parent map: repo-root [AGENTS.md](../../../../AGENTS.md). Rules: [.claude/rules/web-ui.md](../../../../.claude/rules/web-ui.md). Feature page: [workbench](../../../../docs/features/workbench.md).

## File map

**Entry points.** `sessions.ts` (workspace and session types, roster predicates, `/sessions` and `/workspaces` calls) and `HomeScreen.svelte` (the local Home: hosts and workspaces). App activates the stores below with the workspace.

| Family | Files |
|---|---|
| Sessions | `sessions.ts`, `sessionRoster.ts` (pure half of App's `applySessions`: kill tombstones, first-known `created_at` rail order, agent-set change), `sessionCycle.ts` (next agent/terminal chord), `recentSessions.ts` (grace for a new tab before its create lands), `recents.svelte.ts` (rail Recents: fetch, fit, expand, archive + Undo, reopen), `worktreeNotice.svelte.ts` (the rail's kept-worktree notice + open a worktree in a new window), `unread.svelte.ts`, `agentLinks.ts` (linked terminals), `sameFile.svelte.ts` + `SameFileNotice.svelte`, `SessionsView.svelte` (all past sessions), `SessionChangesView.svelte` + `SessionEdits.svelte` |
| Files | `FileTree.svelte` (rail FILES, lazy), `fsEvents.ts` (in-app fs-mutation bus: `fsEpoch`, `lastFsMutation`), `diskWatch.ts` (daemon disk-change monitor), `fileClipboard.svelte.ts`, `fileTransfer.ts`, `QuickOpen.svelte` |
| Git | `git.ts` (status/diff client + stores), `gitDeco.ts` (one status decoration everywhere), `gitFormat.ts`, `GitView.svelte` (Source Control panel), `GitRepoSection.svelte`, `GitDetailView.svelte` (`gitx` tab), `GitHistoryList.svelte`, `CommitRow.svelte` |
| Agents | `launcher.ts` (catalog, resumables, saved default) + `Launcher.svelte`, `launcherState.svelte.ts` (the rail's new-agent split button + popover state; starts go back through App's spawn), `agentSetup.ts` + `AgentSetupDialog.svelte` (managed install/update), `agentSignIn.ts` + `AgentSignInCard.svelte`, `operations.ts` (shared operation phases and request helper) |
| Home and hosts | `HomeScreen.svelte`, `HomeNavigation.svelte`, `HomeActions.svelte` (row menu), `FolderPicker.svelte`, `RemoteSettingsDialog.svelte`, `ReauthOverlay.svelte` (blocking re-auth), `AskpassModal.svelte` + `askpassScope.ts` (in-app SSH prompts) |
| Clusters and compute | `cluster.ts` (pure words and launch spec), `clusterRow.ts` (the home row's subset), `clusterStore.svelte.ts`, `ClusterPage.svelte`, `ClusterStartSheet.svelte`, `ClusterFolderPicker.svelte`, `ClusterSettingDialog.svelte`, `JobClusterHome.svelte`, `JobWindowNotices.svelte`, `compute.ts` (`/api/v1/compute` store), `ComputeBanner.svelte`, `ComputeStrip.svelte` |
| Timeline, Knowledge, history | `timeline.svelte.ts` + `timelineModel.ts` (pure), `TimelineView.svelte`, `TimelineRow.svelte` (shared row anatomy), `knowledge.ts`, `history.ts` (history, edits, activity, CSV) |
| Comms, notices, updates | `comms.svelte.ts` (agent communication), `notices.ts` (browser notifications), `update.svelte.ts` + `UpdateToast.svelte` |

Tests: `*.test.ts` beside the modules.

## State owners

| State | Owner | Who mutates |
|---|---|---|
| Git status, repos, worktrees (readable stores) | `git.ts` | Its private writables, through its exported functions. |
| Timeline entries (`timelineStore`, runes class) | `timeline.svelte.ts` | The class methods. |
| Comms state (`commsStore`) | `comms.svelte.ts` | The class methods. |
| Knowledge (readable stores) | `knowledge.ts` | Its exported functions. |
| Compute snapshot (`computeStatus`) | `compute.ts` | The module's fetch loop. |
| Agent catalog (`agentCatalog`) | `launcher.ts` | The module (read-only `subscribe` export). |
| `updateState` (`$state`) | `update.svelte.ts` | The module's functions. |
| `fsEpoch`, `lastFsMutation`, `pendingDelete` | `fsEvents.ts` | Any surface that creates, renames, or deletes; App reads `lastFsMutation` to fix open tabs. |
| Unread marks, `sameFile`, `clusterOverviews`, file clipboard | Their `*.svelte.ts` modules | Their exported functions. |
| Rail Recents, kept-worktree notice, launcher popover + catalog copy (`RecentsRail`, `KeptWorktreeNotice`, `LauncherState`) | `recents.svelte.ts`, `worktreeNotice.svelte.ts`, `launcherState.svelte.ts` | Their class methods. App constructs one each and calls their `track*` methods at the original effect positions; the session roster and `layout` stay App's (spawns go through App's `spawnSession`). |
| Tombstones, first-known `created_at`, previous agent set (`SessionRoster`) | `sessionRoster.ts` | `applySessions`, `tombstone` and the tombstone expiry in App. |

## Invariants / gotchas

- **Git, Timeline, and Knowledge are pulled, never pushed.** `/ws/events` carries only a small epoch nudge. Each store mirrors only the active workspace (`git.ts`, `timeline.svelte.ts`, `knowledge.ts` headers).
- **Store modules own their `visibilitychange` listener** when the return needs an immediate refetch (`compute.ts`, `comms.svelte.ts`, `timeline.svelte.ts`, `knowledge.ts`). See the rule bullet on recurring work.
- **Compute asks at most once a minute.** That is the scheduler-politeness floor (`compute.ts` header).
- **Use `branchLabel()` from `sessions.ts`** for branch names, detached `Commit <hash>` and unborn `No commits yet` across git surfaces. A missing Git context renders no chip.
- **Use `isMastermind()` from `sessions.ts`.** Do not re-type the predicate inline ([dashboard map](../dashboard/AGENTS.md)).
- **Unread is per window and in memory.** A reload forgets it on purpose (`unread.svelte.ts` header).
- **"Since you left" is per viewer.** The last-seen seq lives in localStorage, and every access is wrapped (`timeline.svelte.ts` header).
- **Cluster words are cluster, job, workspace, chats** — never server, session, or allocation (`cluster.ts` header).
- **A remote window takes only its own host's SSH prompts.** Null scope is the local fallback (`askpassScope.ts`).
- **Pending agent setup survives a reload.** `agentSetup.ts` stores the operation id before the POST and clears it only after the result shows ([settings map](../settings/AGENTS.md)).
- **A deleted or vanished tree row hands focus on, never to `<body>`.** `FileTree` moves focus from a focused row that a re-list drops to the same path, a renamed row's new path, the row now in its place, or the tree. A delete asked from a row also re-checks once the confirm dialog closes, because the disk monitor can re-list before the delete call returns. It only acts when focus was on one of this tree's rows and is now nowhere, and never from an `inert` tree. Its context-menu picks rely on `ContextMenuHost` giving focus back to the right-clicked row before a pick runs, so the dialog records the row as its opener ([ui-contracts §2–3](../../../../docs/agent-guides/ui-contracts.md#2-buttons-dialogs-popovers-menus-tabs-selection-destructive-actions)).
- **`fsEvents` and `diskWatch` are separate signals.** One in-repo change can refresh twice; both re-lists are idempotent (`fsEvents.ts` header).

## Checks

```sh
npm --prefix web-ui run check
npm --prefix web-ui run test -- src/lib/workspace
```

Drive store and surface changes live in the isolated preview (**verify-app**).

## Neighbours

[layout](../layout/AGENTS.md) · [previews](../previews/AGENTS.md) · [net](../net/AGENTS.md) · [shared](../shared/AGENTS.md) · [dashboard](../dashboard/AGENTS.md) · [chat](../chat/AGENTS.md) · [settings](../settings/AGENTS.md) · [knowledge](../knowledge/AGENTS.md) · [plugins](../plugins/AGENTS.md). Feature pages: [git](../../../../docs/features/git.md), [compute](../../../../docs/features/compute.md), [agents](../../../../docs/features/agents.md), [timeline-and-knowledge](../../../../docs/features/timeline-and-knowledge.md), [session-history](../../../../docs/features/session-history.md), [agent-communication](../../../../docs/features/agent-communication.md), [notifications](../../../../docs/features/notifications.md), [linked-terminals](../../../../docs/features/linked-terminals.md).
