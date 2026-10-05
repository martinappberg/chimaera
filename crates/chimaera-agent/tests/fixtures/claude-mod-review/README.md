# Review planner demo Mod

A separate, realistic fixture for inspecting Chimaera's native Mod layout and
controls. Its scope is example data; its checkmarks record manual preparation.
It never reads files, runs checks, calls tools, or requests a model turn.

Two panes open when a surface attaches: **Review planner** and **Example diff**.
The diff is static illustrative content, not a real repository change or a
finding. Its native `Code` element uses `format: 'diff'`. Switch panes using the
tabs or their navigation buttons, and close/reopen them to inspect the host UI.

In the planner, change the scope and review focus,
check or undo the three preparation steps, and reset the checklist. **Prepare
review prompt** appends an unsent prompt to the composer, preserving any draft
already there. It reports whether that draft update succeeded. Do not send the
prepared prompt during a no-model UI inspection.

State lasts for the Claude process, including pane close/reopen and surface
reattach. Restarting Claude restores the example values.

## Launch

From the repository root, load this fixture only for the disposable session:

```sh
claude --plugin-dir "$PWD/crates/chimaera-agent/tests/fixtures/claude-mod-review"
```

For a Chimaera isolated preview, point that preview's Claude executable setting
at a temporary wrapper that forwards its arguments to the installed Claude
binary with this `--plugin-dir`. Keep the wrapper and setting in the preview's
disposable state; do not install or enable the fixture globally. Use a new
Claude chat so the process picks up the plugin.

Available immediate commands, neither of which starts a model turn:

- `/chimaera-review-demo` opens the pane with its current state.
- `/chimaera-review-demo-diff` opens the illustrative diff pane.
- `/chimaera-review-demo-reset` restores the scope, focus, and checklist, then
  opens the pane.

Validation uses the installed native plugin engine without a model request:

```sh
claude plugin validate crates/chimaera-agent/tests/fixtures/claude-mod-review
```

This fixture is for live UI inspection. The independent `claude-mod` fixture
remains the native protocol smoke fixture.
