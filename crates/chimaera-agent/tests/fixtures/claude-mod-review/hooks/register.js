const PANE = 'chimaera-review-demo';
const DIFF_PANE = 'chimaera-review-example-diff';
const INITIAL_SCOPE = 'Chat composer: draft handling and keyboard focus';
const EXAMPLE_DIFF = [
  '--- a/example/composer-focus.ts',
  '+++ b/example/composer-focus.ts',
  '@@ -1,4 +1,6 @@',
  ' function restoreComposerFocus(composer, pane) {',
  '-  composer.focus();',
  '-  pane.focused = true;',
  '+  if (pane.contains(document.activeElement)) {',
  '+    composer.focus({ preventScroll: true });',
  '+  }',
  '+  pane.focused = false;',
  ' }',
].join('\n');
const ITEMS = [
  { id: 'scope', title: 'Confirm the scope', detail: 'Name the behavior and files to inspect.' },
  { id: 'edges', title: 'Choose the edge cases', detail: 'Consider reconnects, cancellation, and focus changes.' },
  { id: 'checks', title: 'Plan verification', detail: 'Decide which checks would support the review.' },
];
const LENSES = [
  { value: 'correctness', label: 'Correctness and failure paths' },
  { value: 'usability', label: 'Usability and accessibility' },
  { value: 'lifecycle', label: 'State and lifecycle' },
];
let scope = INITIAL_SCOPE;
let lens = 'correctness';
let checked = new Set();
let feedback = '';

function progress() { return checked.size + ' of ' + ITEMS.length + ' preparation steps checked'; }
function redraw($) { $.ui.invalidate('ui.render'); }
async function open($) {
  await $.ui.open({ id: PANE, title: 'Review planner', closeOnEscape: true });
}
async function openDiff($) {
  await $.ui.open({ id: DIFF_PANE, title: 'Example diff', closeOnEscape: true });
}
function reviewPrompt() {
  const selectedLens = LENSES.find((item) => item.value === lens).label;
  return [
    'Please review: ' + (scope.trim() || 'the scope I specify before sending this prompt') + '.',
    'Focus: ' + selectedLens + '.',
    '',
    'Look for concrete bugs and missing edge cases. Explain each finding with its location, impact, and a way to verify it.',
    'Distinguish checks you actually ran from checks you recommend. If there are no findings, say what you inspected and what remains unverified.',
    '',
    'Preparation checklist (manually selected in the demo):',
    ...ITEMS.map((item) => '- [' + (checked.has(item.id) ? 'x' : ' ') + '] ' + item.title),
    '',
    'This prompt came from a UI demo. The demo has not inspected code or run tests; these checkmarks record preparation only.',
  ].join('\n');
}

export function register(on) {
  on('session.start', async ($, e, next) => {
    await $.command.register({ name: 'chimaera-review-demo', description: 'Open the example review planner (no model call)', immediate: true });
    await $.command.register({ name: 'chimaera-review-demo-diff', description: 'Open an illustrative diff (no model call)', immediate: true });
    await $.command.register({ name: 'chimaera-review-demo-reset', description: 'Restore the review planner example data (no model call)', immediate: true });
    return next(e);
  });
  on('session.attach', async ($, e, next) => { await openDiff($); await open($); return next(e); });
  on('command.run', { command: 'chimaera-review-demo' }, async ($) => { await open($); return {}; });
  on('command.run', { command: 'chimaera-review-demo-diff' }, async ($) => { await openDiff($); return {}; });
  on('command.run', { command: 'chimaera-review-demo-reset' }, async ($) => {
    scope = INITIAL_SCOPE; lens = 'correctness'; checked = new Set(); feedback = '';
    redraw($); await open($); return {};
  });
  on('ui.render', { component: 'Pane' }, ($, e, next) => {
    if (e.requestId === DIFF_PANE) {
      const { Box, Text, Code, Button } = $.ui.resolve(e);
      return Box({ flexDirection: 'column', gap: 1, children: [
        Box({ flexDirection: 'column', children: [
          Text({ dimColor: true, children: ['EXAMPLE · ILLUSTRATIVE DIFF'] }),
          Text({ bold: true, children: ['Returning focus to the composer'] }),
          Text({ dimColor: true, children: ['A static example for inspecting the diff layout. This is not a repository change or a review finding.'] }),
        ] }),
        Code({ source: EXAMPLE_DIFF, path: 'example/composer-focus.ts', language: 'typescript', format: 'diff' }),
        Text({ children: ['Review question: when should closing a pane return focus to the composer?'] }),
        Button({ key: 'back-to-planner', label: 'Back to review planner', onPress: () => open($) }),
      ] });
    }
    if (e.requestId !== PANE) return next(e);
    const { Box, Text, Button, Input, Select } = $.ui.resolve(e);
    const setScope = (text) => { scope = text.slice(0, 240); feedback = ''; redraw($); };
    return Box({ flexDirection: 'column', gap: 1, children: [
      Box({ flexDirection: 'column', children: [
        Text({ dimColor: true, children: ['EXAMPLE · REVIEW PLANNING'] }),
        Text({ bold: true, children: ['Prepare a focused review'] }),
        Text({ dimColor: true, children: ['Demo data and a manual checklist. No code has been inspected.'] }),
      ] }),
      Input({ key: 'review-scope', label: 'Review scope', value: scope, placeholder: 'What should the review cover?', onInput: setScope, onSubmit: setScope }),
      Select({ key: 'review-lens', label: 'Focus', value: lens, options: LENSES, onSelect: (value) => {
        if (LENSES.some((item) => item.value === value)) lens = value;
        feedback = ''; redraw($);
      } }),
      Button({ key: 'view-example-diff', label: 'View example diff', dimColor: true, onPress: () => openDiff($) }),
      Box({ flexDirection: 'row', justifyContent: 'space-between', alignItems: 'center', gap: 1, children: [
        Text({ bold: true, children: ['Preparation checklist'] }),
        Text({ dimColor: true, children: [progress()] }),
      ] }),
      ...ITEMS.map((item, index) => Box({ flexDirection: 'row', alignItems: 'center', gap: 1, children: [
        Box({ flexShrink: 0, children: [Text({ color: checked.has(item.id) ? 'green' : 'gray', children: [checked.has(item.id) ? '✓' : String(index + 1).padStart(2, '0')] })] }),
        Box({ flexDirection: 'column', flexGrow: 1, children: [
          Text({ bold: true, children: [item.title] }),
          Text({ dimColor: true, children: [item.detail] }),
        ] }),
        Button({ key: 'check-' + item.id, label: checked.has(item.id) ? 'Undo' : 'Check', onPress: () => {
          if (checked.has(item.id)) checked.delete(item.id); else checked.add(item.id);
          feedback = ''; redraw($);
        } }),
      ] })),
      Box({ flexDirection: 'row', flexWrap: 'wrap', gap: 1, children: [
        Button({ key: 'prepare-review', label: 'Prepare review prompt', onPress: async () => {
          const box = await $.prompt.read();
          const result = await $.prompt.fill({ text: (box.text ? '\n\n' : '') + reviewPrompt(), mode: 'append' });
          feedback = result.isFilled ? 'Review prompt added to the composer. Check it before sending.' : 'The composer could not accept the draft. Return to it and try again.';
          redraw($);
        } }),
        Button({ key: 'reset-checklist', label: 'Reset checklist', dimColor: true, onPress: () => { checked = new Set(); feedback = ''; redraw($); } }),
      ] }),
      Text({ dimColor: true, children: [feedback || 'Preparing a prompt only updates the draft. Nothing is sent.'] }),
    ] });
  });
}
