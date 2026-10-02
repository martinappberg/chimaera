# The Chimaera product story

Public-messaging decisions and evidence from the 2026-10-01 refresh. This is an
editorial guide; the [feature catalog](features/README.md) owns behavior.

## Agreed direction

The maintainer selected **“A workspace for everything you do with agents.”**
The audience should remain broad. Lead with what people can do and why the
workspace helps. Persistence is operating detail, not the lead selling point.
Public copy should describe implemented capabilities confidently rather than
organize itself around caveats, missing features, or future products.

**Agent workbench** remains the category. **Workspace** is the visitor's concrete
mental model: open a folder, work with agents, inspect outputs, and keep context.
**Extension platform** describes how tools and workflows can grow around that
workspace. Use it where extensibility is the subject, backed by actual plugins,
tools, and views.

## The value proposition

> Work with your agents, open what they make, and keep the whole project in view.

An agent conversation is part of a larger activity. Someone still needs to direct
the work, inspect the result, decide what changes, and return to the project later.
Chimaera puts those actions in the same environment, with the project folder as
their common home.

Lead with three connected benefits:

1. **Your project, in view.** Open the folder, see its files, and arrange documents,
   tables, images, apps, and conversations side by side.
2. **Your choice of agent.** Use Claude Code, Codex, or another supported agent in
   the same project environment. These are the user’s own agents, with their
   accounts, skills, and connections on the workspace host. Chimaera provides
   the workspace around them. Add a second agent when it helps.
3. **Local or remote.** Bring that workspace to your machine or the host with the
   project's files and tools.

References into files connect the first two: show the agent the document, passage,
cell, or region that matters. Agent communication, project history, Knowledge,
and extensions add depth after that basic working environment is understood.

The follow-up discussion prioritized **workspace layout, files as usable context,
agent choice, and remote workspaces** over elaborate scenarios. Public examples
should show the environment in use. Naming a knowledge integration is useful in
setup documentation, but it distracts from the central promise in the hero.

## What gives that story substance

| Capability | Why someone would use it | Current reference |
|---|---|---|
| Multiple agent providers and workspace messaging | Give different tasks to different agents while keeping their work connected | [Agents](features/agents.md), [communication](features/agent-communication.md) |
| Dashboard and Mastermind | See what needs a decision and get a briefing across ongoing work | [Dashboard](features/dashboard.md) |
| Rich file viewing and references into outputs | Inspect a result and show an agent exactly what to change | [Files and previews](features/files-and-previews.md) |
| Markdown with linked notes, diagrams, and editing | Read and develop project documents in the working environment | [Files and previews](features/files-and-previews.md) |
| Session records, Timeline, and Git history | Trace the work and return to its outputs | [Session history](features/session-history.md), [Git](features/git.md) |
| Knowledge providers | Browse recorded project findings, decisions, and handoffs beside the agents | [Timeline and Knowledge](features/timeline-and-knowledge.md) |
| Terminals, browser panes, and linked shells | Run and inspect the project's tools where the conversation is happening | [Terminals](features/terminals.md), [browser panes](features/browser-pane.md), [linked terminals](features/linked-terminals.md) |
| Remote hosts and Slurm workspaces | Work on the machine with the files, environment, and compute | [Remote connect](features/remote-connect.md), [compute](features/compute.md) |
| Workbench extensions and agent integrations | Adapt the environment to a workflow and develop new capabilities | [Extensions](features/plugins.md), [authoring guide](agent-guides/plugins.md) |

## Explaining the category

These are mental models for understanding Chimaera, not competitive claims about
the current feature sets of other products.

| Familiar starting point | What Chimaera puts around it |
|---|---|
| An editor | A project where agents do work and people inspect, guide, and refine it |
| An agent chat | Files, tools, other agents, activity, and project records in the same workspace |
| A file browser | Conversations and actions next to the files being read or produced |
| A notes or knowledge tool | Recorded project context beside the agents and outputs it informs |
| A remote terminal | The same visual project workspace on the machine doing the work |

“The next IDE, built around agents” is a useful design ambition. For public copy,
the concrete workspace story explains more with less assumed knowledge. A new
acronym adds a category for visitors to decode. Claims to be first or uniquely
complete would need current market evidence and do not help explain the workflow.

## Page structure and examples

The homepage moves from the workspace promise to an illustrative project, then
the three-step work loop, the capability groups, project knowledge and extensions,
local/remote workspaces, download, and practical questions. Keep knowledge and
extensions together: Timeline shows project activity, Knowledge extensions expose
recorded project context, and the platform adds useful tools and views. Give local
and remote workspaces their own section rather than mixing deployment and extensions. The README gives the same story in a scannable
form, with links into the deeper guides. Usage docs explain setup and feature
behavior; agent maps explain where and how to change the implementation.

Use concrete actions rather than an industry label: prepare a report, inspect a
table, build an app, review changes, follow a decision. Rotate examples across
documents, data, and code. Cluster support is a substantial capability further
down the page, not a restriction on who should use the product.

The homepage uses a labeled interactive preview with sample files and conversations.
Keep an agent beside the work. Let visitors open different file types, inspect a
browser pane, and bring a file reference into the conversation. Match the app's
current visual structure and keep the preview within a stable page height.

## Competitive positioning check

Current first-party apps already overlap with file previews, context references,
arrangeable panes, remote work, and session coordination. Do not claim those
individual affordances are exclusive. The October check used
[Claude desktop documentation](https://code.claude.com/docs/en/desktop),
[OpenAI's file tools](https://learn.chatgpt.com/docs/artifacts-viewer), and
[OpenAI's project documentation](https://learn.chatgpt.com/docs/projects).
Those surfaces evolve; revisit the sources before making a specific comparison.

The proposed reason to choose Chimaera is the combination: a project workspace
with a choice of agent harnesses, varied file viewers and direct references,
local and remote deployment, and an extension platform. This is a positioning
judgment based on Chimaera's implementation, not a claim that every user should
switch or that every feature is unique.

## Copy and showcase refinements

Use short, concrete sentences. Avoid em and en dashes in public copy. Keep the
“Inside the workspace” feature section detailed; the maintainer values its breadth.
Elsewhere, explain the task and the benefit without slogans or dense lists.
Knowledge and extensions deserve their own section. Describe the platform through
new views, file viewers, and agent tools; document compilers are examples, not its
identity.

The maintainer now prefers a faithful interactive workspace to the simplified
animated illustration. Use the current app's rail, compact tabs, panes, file glyphs,
and colors. Include varied file types and a browser. Natural file and pane
navigation is welcome; playback controls and a tutorial remain unnecessary.

## Maintenance

Keep the headline and core story aligned across the README, homepage, and usage
docs. Tie new feature claims to the catalog and implementation. Provider lists,
permissions, and setup requirements belong in the relevant guides so they can be
kept precise without turning the homepage into a specification. Revisit framing
with the maintainer when the product changes substantially.

Keep the dedicated knowledge and extensions section broad. Document compilers are
examples, not the platform story. Preserve the larger hero download action and
keep the social preview image aligned with the approved workspace headline.
Within the demo, a short wheel-scroll hold at a pane boundary should release to
the page with continued scrolling; touch and keyboard navigation remain native.
