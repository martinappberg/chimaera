# Chimaera documentation

Chimaera is an agent workbench: a workspace for everything you do with agents.
Start with the guide for what you want to do.

| I want to… | Start here |
|---|---|
| Install and use Chimaera | [Getting started and usage](https://chimaera.sh/docs.html) |
| See what the app supports | [Feature catalog](features/README.md) |
| Build or change the application | [Contributing](../.github/CONTRIBUTING.md), then [repository orientation](../AGENTS.md) |
| Build an extension | [Extension authoring](agent-guides/plugins.md) |
| Add or change an agent integration | [Agent integrations](agent-guides/agent-integrations.md) |
| Understand the implementation | [Architecture](agent-guides/architecture.md) |
| Understand a design decision | [Design records](design/README.md) |

## Using the workspace

- [Agents](features/agents.md), [dashboard](features/dashboard.md), and
  [communication](features/agent-communication.md): work with several agents and
  use a Mastermind coordinator.
- [Files and previews](features/files-and-previews.md) and
  [browser panes](features/browser-pane.md): inspect outputs and reference them in a conversation.
- [Session history](features/session-history.md) and
  [Timeline and Knowledge](features/timeline-and-knowledge.md): return to the work
  and the records behind it.
- [Extensions](features/plugins.md): workbench plugins, agent plugins, skills, and connections.
- [Remote hosts](features/remote-connect.md) and [clusters](features/compute.md):
  work on the machine that owns the files and compute.

The [full catalog](features/README.md) covers each capability, its UI entry points,
implementation, and current limits.

## Developing and operating Chimaera

- [Repository orientation](../AGENTS.md) points people and coding agents to area
  maps, path-scoped rules, and verification workflows.
- [Contributing](../.github/CONTRIBUTING.md) covers prerequisites, isolated
  development, checks, and the CLA.
- [Extension authoring](agent-guides/plugins.md) provides a starter component,
  the plugin interface, permissions, local installation, and a live verification checklist.
- [Agent integrations](agent-guides/agent-integrations.md) covers protocol
  boundaries and adding or changing an agent adapter.
- [Architecture](agent-guides/architecture.md) explains implementation structure
  and constraints.
- [Releases](agent-guides/releases.md) covers versioning, packaging, and publishing.
- [Cloud development](agent-guides/cloud-sessions.md) covers the cloud session workflow.

## Product direction and history

The [product story](product-story.md) records the agreed public framing.
[Design records](design/README.md) preserve proposals, tradeoffs, and decisions.
[Dated field notes](history/field-notes.md) and verification reports under
`history/` record what was observed at the time.

For what ships now, use the feature catalog and implementation. Historical plans
can contain superseded behavior and unfinished proposals. They are kept so the
reasoning remains available, without presenting those proposals as current features.
