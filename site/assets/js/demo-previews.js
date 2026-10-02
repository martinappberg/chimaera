/* Authored Project Atlas fixtures. Nothing here executes a project or agent. */
(function () {
  "use strict";

  function plot(compact) {
    return '<svg class="dp-chart' + (compact ? ' dp-chart-compact' : '') + '" viewBox="0 0 320 130" role="img" aria-label="Completed work rises from 14 to 29 across six days.">' +
      '<g class="dp-chart-grid"><path d="M28 18H308M28 48H308M28 78H308M28 108H308"/></g>' +
      '<path class="dp-chart-area" d="M28 88L84 71L140 80L196 53L252 40L308 22V108H28Z"/>' +
      '<path class="dp-chart-line" d="M28 88L84 71L140 80L196 53L252 40L308 22"/>' +
      '<g class="dp-chart-dot"><circle cx="28" cy="88" r="3"/><circle cx="84" cy="71" r="3"/><circle cx="140" cy="80" r="3"/><circle cx="196" cy="53" r="3"/><circle cx="252" cy="40" r="3"/><circle cx="308" cy="22" r="3"/></g>' +
      '<g class="dp-chart-label"><text x="28" y="126">Mon</text><text x="84" y="126">Tue</text><text x="140" y="126">Wed</text><text x="196" y="126">Thu</text><text x="252" y="126">Fri</text><text x="298" y="126">Sat</text><text x="7" y="22">30</text><text x="7" y="82">10</text></g></svg>';
  }

  function grid(headers, rows, sheet, plain) {
    return '<div class="dp-grid-scroll"><table class="dp-grid' + (sheet ? ' dp-sheet-grid' : '') + '"><thead><tr>' +
      headers.map(function (h) { return '<th>' + h + '</th>'; }).join('') + '</tr></thead><tbody>' +
      rows.map(function (row, i) { return '<tr>' + row.map(function (cell, j) {
        var ref = sheet ? sheet + '!' + (j ? String.fromCharCode(64 + j) + String(i + 2) : 'A' + (i + 2) + ':D' + (i + 2)) : 'row ' + (i + 1) + ', ' + headers[j];
        var numeric = /^\d+(\.\d+)?%?$/.test(cell);
        return '<td' + (numeric ? ' class="dp-grid-number"' : '') + (plain ? '' : ' data-demo-reference="' + ref + '"') + '>' + cell + '</td>';
      }).join('') + '</tr>'; }).join('') + '</tbody></table></div>';
  }

  var teams = [
    ['Design', '32', '4', '89%'], ['Product', '28', '5', '85%'],
    ['Engineering', '40', '6', '87%'], ['Operations', '24', '3', '89%']
  ];

  var brief = '<div class="dp-root dp-markdown"><div class="dp-doc-meta">PROJECT ATLAS / PROJECT BRIEF</div>' +
    '<h1>Make the next step clear.</h1><p class="dp-lead" data-demo-reference="Purpose">One place to plan the work, see progress, and decide what comes next.</p>' +
    '<aside class="dp-callout" data-demo-reference="The goal"><span class="dp-callout-icon">◈</span><div><strong>The goal</strong><p>A clear weekly review. Shared numbers. Fewer open questions.</p></div></aside>' +
    '<h2>This week</h2><p data-demo-reference="Weekly summary">We completed <strong>124 tasks</strong> across four teams. The remaining <strong>18 tasks</strong> are assigned and ready for the next review.</p>' +
    '<ul><li data-demo-reference="Review task">Review the results with each team.</li><li data-demo-reference="Report task">Publish the report and the progress chart.</li><li data-demo-reference="Plan task">Agree on the next three priorities.</li></ul>' +
    '<h2>Project files</h2><p><button type="button" role="link" class="dp-doc-link" data-open="csv">data/results.csv</button> · <button type="button" role="link" class="dp-doc-link" data-open="pdf">reports/weekly-report.pdf</button></p>' +
    '<hr><p class="dp-footnote">Owner: Alex Chen · Updated 1 October</p></div>';

  var csv = '<div class="dp-root dp-data"><div class="dp-data-top"><span>4 rows × 4 columns</span><span>UTF-8 · CSV</span></div>' +
    grid(['team', 'completed', 'remaining', 'progress'], teams, false) +
    '<div class="dp-data-bottom">Rows 1 to 4 <span>4 rows</span></div></div>';

  function boundedIndex(index, count) {
    var value = Number(index);
    return Number.isFinite(value) ? Math.max(0, Math.min(count - 1, Math.floor(value))) : 0;
  }

  var workbookSheets = [
    { name: 'Overview', rows: [
      ['1', '<strong>Team</strong>', '<strong>Complete</strong>', '<strong>Remaining</strong>', '<strong>Progress</strong>'],
      ['2', 'Design', '32', '4', '89%'], ['3', 'Product', '28', '5', '85%'],
      ['4', 'Engineering', '40', '6', '87%'], ['5', 'Operations', '24', '3', '89%'],
      ['6', '<strong>Total</strong>', '<strong class="dp-cell-selected">124</strong>', '<strong>18</strong>', '<strong>87%</strong>'],
      ['7', '', '', '', ''], ['8', '', '', '', ''], ['9', '', '', '', '']
    ] },
    { name: 'Timeline', rows: [
      ['1', '<strong>Week</strong>', '<strong>Complete</strong>', '<strong>Remaining</strong>', '<strong>Progress</strong>'],
      ['2', 'Week 1', '95', '15', '86%'], ['3', 'Week 2', '101', '13', '89%'],
      ['4', 'Week 3', '108', '12', '90%'], ['5', 'Week 4', '124', '18', '87%'],
      ['6', '<strong>Month</strong>', '<strong class="dp-cell-selected">428</strong>', '<strong>58</strong>', '<strong>88%</strong>'],
      ['7', '', '', '', ''], ['8', '', '', '', ''], ['9', '', '', '', '']
    ] },
    { name: 'Notes', rows: [
      ['1', '<strong>Decision</strong>', '<strong>Owner</strong>', '<strong>Date</strong>', '<strong>Status</strong>'],
      ['2', 'Weekly review', 'Alex', '1 Oct', 'Agreed'],
      ['3', 'Check source data', 'Sam', '2 Oct', 'Ready'],
      ['4', 'Share next plan', 'Team leads', '3 Oct', 'Planned'],
      ['5', '', '', '', ''], ['6', '', '', '', ''], ['7', '', '', '', ''],
      ['8', '', '', '', ''], ['9', '', '', '', '']
    ] }
  ];

  function renderWorkbook(index) {
    var selected = boundedIndex(index, workbookSheets.length);
    var sheet = workbookSheets[selected];
    var headers = [''].concat(sheet.rows[0].slice(1).map(function (heading) { return heading.replace(/<[^>]*>/g, ''); }));
    var rows = sheet.rows.slice(1).map(function (row, i) { return [String(i + 1)].concat(row.slice(1)); });
    return '<div class="dp-root dp-workbook"><div class="dp-sheet-tabs" aria-label="Workbook sheets">' + workbookSheets.map(function (tab, i) {
        return '<button type="button" class="dp-sheet-tab' + (i === selected ? ' dp-sheet-active' : '') + '" data-demo-view-index="' + i + '" aria-pressed="' + (i === selected) + '">' + tab.name + '</button>';
      }).join('') + '</div>' + grid(headers, rows, sheet.name) + '<div class="dp-data-bottom">Rows 1 to 8 <span>' + sheet.name + '</span></div></div>';
  }

  var workbook = renderWorkbook(0);

  var report = '<div class="dp-root dp-paper-view"><div class="dp-view-meta">1 / 3 <span>100%</span></div><article class="dp-paper dp-report">' +
    '<div class="dp-paper-brand">ATLAS <span>WEEKLY REVIEW</span></div><h1>Good progress.<br>A clear next step.</h1><p class="dp-paper-sub">Week ending 1 October</p>' +
    '<div class="dp-report-stats" data-demo-reference="Page 1, weekly totals"><div><strong>124</strong><span>completed</span></div><div><strong>18</strong><span>remaining</span></div><div><strong>87%</strong><span>complete</span></div></div>' +
    '<h2>Progress through the week</h2><div data-demo-reference="Page 1, progress chart">' + plot(true) + '</div>' +
    '<p data-demo-reference="Page 1, summary">All four teams contributed to this week’s progress. Engineering completed 40 tasks. Design and Operations are closest to their weekly targets.</p>' +
    '<div class="dp-paper-footer"><span>Project Atlas</span><span>01</span></div></article></div>';

  var proposal = '<div class="dp-root dp-paper-view"><div class="dp-view-meta">Page 1 <span>Document</span></div><article class="dp-paper dp-word">' +
    '<div class="dp-word-kicker">PROJECT ATLAS</div><h1>Next phase proposal</h1><p class="dp-word-byline">Alex Chen · 1 October</p>' +
    '<h2>1. A focused next phase</h2><p>The next phase turns the weekly review into a shared routine. Each team will bring its results, highlight an open question, and name its next priority.</p>' +
    '<h2>2. Three priorities</h2><table class="dp-word-table"><tr><th>Priority</th><th>Owner</th></tr><tr><td>Finish the remaining work</td><td>Team leads</td></tr><tr><td>Validate the report</td><td>Alex</td></tr><tr><td>Share the next plan</td><td>Sam</td></tr></table>' +
    '<h2>3. A useful measure</h2><p>Every priority has an owner, a date, and a result that everyone can review.</p><div class="dp-paper-footer"><span>Project Atlas / Proposal</span><span>1</span></div></article></div>';

  function pageControls(selected) {
    return '<div class="dp-view-meta"><span>' + (selected + 1) + ' / 3</span><div class="dp-view-pages" aria-label="Report pages">' + [0, 1, 2].map(function (index) {
      return '<button type="button" data-demo-view-index="' + index + '" aria-pressed="' + (index === selected) + '" aria-label="Page ' + (index + 1) + '">' + (index + 1) + '</button>';
    }).join('') + '</div><span>100%</span></div>';
  }

  function renderPDF(index) {
    var selected = boundedIndex(index, 3);
    if (selected === 0) return report.replace('<div class="dp-view-meta">1 / 3 <span>100%</span></div>', pageControls(0));
    var body = selected === 1
      ? '<h1>Four teams.<br>One shared picture.</h1><p class="dp-paper-sub">A closer look at the week’s results</p><h2>Tasks completed by team</h2><div class="dp-team-bars" data-demo-reference="Page 2, team results">' +
        teams.map(function (team) { return '<div><span>' + team[0] + '</span><i style="--dp-bar:' + (Number(team[1]) / 40 * 100) + '%"></i><strong>' + team[1] + '</strong></div>'; }).join('') +
        '</div><h2>Where we stand</h2><p data-demo-reference="Page 2, summary">Engineering completed the most work this week. Design and Operations reached 89% of their plans. The remaining tasks have owners in all four teams.</p><table class="dp-word-table" data-demo-reference="Page 2, remaining tasks"><tr><th>Team</th><th>Remaining</th></tr>' +
        teams.map(function (team) { return '<tr><td>' + team[0] + '</td><td>' + team[2] + '</td></tr>'; }).join('') + '</table>'
      : '<h1>What comes next.</h1><p class="dp-paper-sub">Three priorities for the next review</p><h2>01 / Finish the remaining work</h2><p data-demo-reference="Page 3, priority 1">Each team will review its open tasks and confirm the next delivery date. There are 18 tasks remaining across the project.</p><h2>02 / Validate the report</h2><p data-demo-reference="Page 3, priority 2">Check the weekly numbers against the source data. Keep the report, workbook, and chart aligned.</p><h2>03 / Share the next plan</h2><p data-demo-reference="Page 3, priority 3">Give every priority an owner and a clear result. Review the plan together before the next week begins.</p><div class="dp-paper-note" data-demo-reference="Page 3, decision">A useful next step has an owner, a date, and a result we can review.</div>';
    return '<div class="dp-root dp-paper-view">' + pageControls(selected) + '<article class="dp-paper dp-report"><div class="dp-paper-brand">ATLAS <span>WEEKLY REVIEW</span></div>' + body + '<div class="dp-paper-footer"><span>Project Atlas</span><span>0' + (selected + 1) + '</span></div></article></div>';
  }

  var slideBodies = [
    '<div class="dp-slide-label">PROJECT ATLAS</div>' +
    '<h1>A good week.<br>A clear path ahead.</h1><div class="dp-slide-rule"></div><div class="dp-slide-stats"><div><strong>124</strong><span>tasks completed</span></div><div><strong>87%</strong><span>of the plan</span></div></div><div class="dp-slide-footer"><span>Weekly review · 1 October</span><span>01</span></div>',
    '<div class="dp-slide-label">THE NUMBERS</div><h1>Small steps add up.</h1><div class="dp-slide-rule"></div><div class="dp-slide-stats"><div><strong>124</strong><span>complete</span></div><div><strong>18</strong><span>remaining</span></div><div><strong>87%</strong><span>of the plan</span></div></div><p class="dp-slide-caption">The next step is already assigned.</p><div class="dp-slide-footer"><span>Project Atlas / Weekly results</span><span>02</span></div>',
    '<div class="dp-slide-label">FOUR TEAMS</div><h1>Everyone contributes.</h1><div class="dp-slide-team"><span>Design <b>32</b></span><span>Product <b>28</b></span><span>Engineering <b>40</b></span><span>Operations <b>24</b></span></div><div class="dp-slide-footer"><span>Tasks completed this week</span><span>03</span></div>',
    '<div class="dp-slide-label">WHAT’S NEXT</div><h1>Keep the next step clear.</h1><ol class="dp-slide-priorities"><li>Finish the remaining work.</li><li>Validate the report.</li><li>Share the next plan.</li></ol><div class="dp-slide-footer"><span>Project Atlas / Next priorities</span><span>04</span></div>'
  ];

  function renderSlides(index) {
    var selected = boundedIndex(index, slideBodies.length);
    var titles = ['A good week.', 'The numbers', 'Four teams', 'What’s next'];
    return '<div class="dp-root dp-deck"><div class="dp-view-meta">' + (selected + 1) + ' / 4 <span>Weekly review</span></div><div class="dp-slide">' + slideBodies[selected] + '</div>' +
      '<div class="dp-thumbnails" aria-label="Slide thumbnails">' + titles.map(function (title, i) {
        return '<button type="button" class="dp-thumbnail' + (selected === i ? ' dp-thumbnail-active' : '') + '" data-demo-view-index="' + i + '" aria-pressed="' + (selected === i) + '" aria-label="Slide ' + (i + 1) + ': ' + title + '"><span>0' + (i + 1) + '</span><strong>' + title + '</strong><i></i></button>';
      }).join('') + '</div></div>';
  }

  var slide = renderSlides(0);

  var pythonLines = [
    '<span class="dp-syn-comment"># Project Atlas · weekly summary</span>',
    '<span class="dp-syn-keyword">from</span> pathlib <span class="dp-syn-keyword">import</span> Path',
    '<span class="dp-syn-keyword">import</span> pandas <span class="dp-syn-keyword">as</span> pd', '',
    'ROOT = Path(__file__).resolve().parents[<span class="dp-syn-number">1</span>]',
    'data = pd.read_csv(ROOT / <span class="dp-syn-string">"data/results.csv"</span>)', '',
    'completed = data[<span class="dp-syn-string">"completed"</span>].sum()',
    'remaining = data[<span class="dp-syn-string">"remaining"</span>].sum()',
    'progress = completed / (completed + remaining)', '',
    'summary = {',
    '    <span class="dp-syn-string">"completed"</span>: <span class="dp-syn-call">int</span>(completed),',
    '    <span class="dp-syn-string">"remaining"</span>: <span class="dp-syn-call">int</span>(remaining),',
    '    <span class="dp-syn-string">"progress"</span>: <span class="dp-syn-call">round</span>(progress, <span class="dp-syn-number">3</span>),',
    '}', '', '<span class="dp-syn-call">print</span>(summary)'
  ];
  var python = '<div class="dp-root dp-code"><div class="dp-code-lines">' + pythonLines.map(function (line, i) {
    return '<div class="dp-code-line" data-demo-reference="line ' + (i + 1) + '"><span class="dp-line-number">' + (i + 1) + '</span><code>' + (line || ' ') + '</code></div>';
  }).join('') + '</div><div class="dp-code-status">Python <span>UTF-8 · LF</span></div></div>';

  var notebook = '<div class="dp-root dp-notebook"><h1>Atlas, in numbers</h1><p class="dp-notebook-intro" data-demo-reference="Introduction">A quick look at the week’s progress.</p>' +
    '<div class="dp-nb-cell" data-demo-reference="Cell 1"><span class="dp-nb-prompt">In [1]:</span><pre><span class="dp-syn-keyword">import</span> pandas <span class="dp-syn-keyword">as</span> pd\n<span class="dp-syn-keyword">import</span> matplotlib.pyplot <span class="dp-syn-keyword">as</span> plt\ndata = pd.read_csv(<span class="dp-syn-string">"../data/results.csv"</span>)\ndata[[<span class="dp-syn-string">"team"</span>, <span class="dp-syn-string">"completed"</span>]]</pre></div>' +
    '<div class="dp-nb-output" data-demo-reference="Cell 1, table"><span class="dp-nb-prompt">Out[1]:</span>' + grid(['', 'team', 'completed'], [['0', 'Design', '32'], ['1', 'Product', '28'], ['2', 'Engineering', '40'], ['3', 'Operations', '24']], false, true) + '</div>' +
    '<div class="dp-nb-cell" data-demo-reference="Cell 2"><span class="dp-nb-prompt">In [2]:</span><pre>daily = [<span class="dp-syn-number">14, 18, 16, 22, 25, 29</span>]\nplt.plot(daily, marker=<span class="dp-syn-string">"o"</span>)</pre></div>' +
    '<div class="dp-nb-plot" data-demo-reference="Cell 2, plot"><span>Tasks completed each day</span>' + plot(false) + '</div></div>';

  var graphic = '<div class="dp-root dp-image-view"><div class="dp-image-stage"><svg class="dp-poster" viewBox="0 0 500 320" role="img" aria-label="Project Atlas progress graphic. 124 tasks complete, 18 remaining.">' +
    '<rect width="500" height="320" rx="3" fill="#f5f4ef"/><path d="M320 0h180v320H320z" fill="#dce6e1"/><circle cx="438" cy="53" r="114" fill="#c6d8d0"/><circle cx="450" cy="294" r="104" fill="#b3ccc0"/>' +
    '<g fill="#203b31" font-family="Arial, sans-serif"><text x="35" y="42" font-size="12" letter-spacing="3">PROJECT ATLAS</text><text x="32" y="108" font-size="37" font-weight="700">Work moves</text><text x="32" y="153" font-size="37" font-weight="700">forward.</text><text x="35" y="198" font-size="13">One clear priority at a time.</text><text x="35" y="257" font-size="33" font-weight="700">124</text><text x="36" y="279" font-size="11">TASKS COMPLETE</text><text x="188" y="257" font-size="33" font-weight="700">87%</text><text x="189" y="279" font-size="11">OF THE PLAN</text></g>' +
    '<g stroke="#3e7160" stroke-width="2" fill="none"><path d="M351 230V134l35-35 34 35v96M351 134h69M368 116v114M403 116v114"/><circle cx="385" cy="74" r="9"/></g></svg></div>' +
    '<div class="dp-image-meta" data-demo-reference="Full image, 500 × 320">500 × 320 · SVG <span>100%</span></div></div>';

  var parquet = '<div class="dp-root dp-data"><div class="dp-data-top"><span>1,024 rows · 4 columns</span><span>Parquet</span></div>' +
    '<div class="dp-column-types"><span>task_id <i>string</i></span><span>team <i>string</i></span><span>status <i>string</i></span><span>hours <i>float64</i></span></div>' +
    grid(['task_id', 'team', 'status', 'hours'], [
      ['AT-001', 'Design', '<span class="dp-data-done">complete</span>', '2.5'],
      ['AT-002', 'Product', '<span class="dp-data-done">complete</span>', '1.0'],
      ['AT-003', 'Engineering', '<span class="dp-data-done">complete</span>', '4.0'],
      ['AT-004', 'Operations', 'in progress', '1.5'],
      ['AT-005', 'Design', '<span class="dp-data-done">complete</span>', '3.0'],
      ['AT-006', 'Engineering', 'planned', '2.0'],
      ['AT-007', 'Product', '<span class="dp-data-done">complete</span>', '1.5'],
      ['AT-008', 'Operations', '<span class="dp-data-done">complete</span>', '2.0']
    ], false) + '<div class="dp-data-bottom">Rows 1 to 8 of 1,024 <span>Row group 1 / 4</span></div></div>';

  var diagram = '<div class="dp-root dp-board"><svg viewBox="0 0 420 330" role="img" aria-label="Atlas workflow: plan, work, review, and share.">' +
    '<defs><pattern id="dp-board-dots" width="14" height="14" patternUnits="userSpaceOnUse"><circle cx="1" cy="1" r=".7" fill="currentColor" opacity=".18"/></pattern><marker id="dp-arrow" markerWidth="8" markerHeight="8" refX="6" refY="4" orient="auto"><path d="M0 0L7 4 0 8" fill="none" stroke="currentColor"/></marker></defs><rect width="420" height="330" fill="url(#dp-board-dots)"/>' +
    '<text class="dp-board-title" x="27" y="36">How Atlas moves forward</text><g class="dp-board-edge" marker-end="url(#dp-arrow)"><path d="M210 90V119M210 177V206M274 149h71v88H274M210 264v36H68V64h77"/></g>' +
    '<g class="dp-board-node"><rect x="146" y="49" width="128" height="42" rx="7"/><text x="210" y="75">Plan the next step</text></g>' +
    '<g class="dp-board-node dp-board-primary"><rect x="146" y="121" width="128" height="56" rx="7"/><text x="210" y="146">Do the work</text><text class="dp-board-small" x="210" y="163">Four teams, one plan</text></g>' +
    '<g class="dp-board-node"><rect x="146" y="208" width="128" height="56" rx="7"/><text x="210" y="234">Review the results</text><text class="dp-board-small" x="210" y="251">124 tasks complete</text></g>' +
    '<g class="dp-board-note"><rect x="305" y="75" width="89" height="53" rx="2"/><text x="316" y="96">Share the</text><text x="316" y="113">weekly report</text></g><text class="dp-board-small" x="78" y="292">Next week</text></svg><div class="dp-board-meta">1 page <span>100%</span></div></div>';

  function browserHTML(period) {
    var month = period === 'month';
    var total = month ? '428' : '124';
    var remaining = month ? '58' : '18';
    var percent = month ? '88%' : '87%';
    var line = month ? 'M28 94L121 71L214 51L308 16' : 'M28 88L84 71L140 80L196 53L252 40L308 22';
    var area = line + 'V108H28Z';
    var labels = month ? ['Week 1', 'Week 2', 'Week 3', 'Week 4'] : ['Mon', 'Wed', 'Fri', 'Sat'];
    return '<div class="dp-root dp-live-report"><div class="dp-live-head"><div><span class="dp-live-kicker">PROJECT ATLAS</span><h1>Progress, at a glance.</h1></div><div class="dp-periods" aria-label="Report period">' +
      '<button type="button" data-demo-period="week" aria-pressed="' + (!month) + '">Week</button><button type="button" data-demo-period="month" aria-pressed="' + month + '">Month</button></div></div>' +
      '<div class="dp-live-stats"><div><strong>' + total + '</strong><span>completed</span></div><div><strong>' + remaining + '</strong><span>remaining</span></div><div><strong>' + percent + '</strong><span>of the plan</span></div></div>' +
      '<svg class="dp-live-chart" viewBox="0 0 320 132" role="img" aria-label="' + (month ? 'Monthly' : 'Weekly') + ' completed work"><g class="dp-chart-grid"><path d="M28 18H308M28 48H308M28 78H308M28 108H308"/></g><path class="dp-chart-area" d="' + area + '"/><path class="dp-chart-line" d="' + line + '"/><g class="dp-chart-label"><text x="28" y="128">' + labels[0] + '</text><text x="110" y="128">' + labels[1] + '</text><text x="215" y="128">' + labels[2] + '</text><text x="285" y="128">' + labels[3] + '</text></g></svg></div>';
  }

  var htmlReport = '<div class="dp-root dp-html-report"><header><span>ATLAS / REPORT</span><span>1 October</span></header><h1>The week in view.</h1><p class="dp-lead">Four teams. One shared picture.</p>' +
    '<div class="dp-html-metric"><strong>87<span>%</span></strong><div><b>of the plan complete</b><p>124 tasks completed. 18 ready for the next step.</p></div></div><h2>Steady progress</h2>' +
    '<div>' + plot(false) + '</div><div class="dp-html-team"><span>Design <b>32</b></span><span>Product <b>28</b></span><span>Engineering <b>40</b></span><span>Operations <b>24</b></span></div><footer>PROJECT ATLAS · WEEKLY REPORT</footer></div>';

  var video = '<div class="dp-root dp-media-view"><div class="dp-media-stage"><video controls preload="metadata" playsinline poster="assets/demo/atlas-video-poster.svg" aria-label="Project Atlas progress animation"><source src="assets/demo/atlas-overview.mp4" type="video/mp4"></video></div><div class="dp-media-description"><h1>Atlas overview</h1><p>A short look at this week’s progress.</p><span data-demo-reference="00:00 to 00:05">00:05 · 640 × 360 · MP4</span></div></div>';

  var audio = '<div class="dp-root dp-audio-view"><div class="dp-audio-art"><svg viewBox="0 0 160 120" role="img" aria-label="Atlas audio cover"><circle cx="80" cy="60" r="43"/><circle cx="80" cy="60" r="28"/><circle cx="80" cy="60" r="9"/><path d="M16 64h8l5-15 7 29 8-41 9 55 9-66 9 68 10-74 9 74 9-68 9 58 8-39 7 24 7-15h11"/></svg></div><h1>Atlas theme</h1><p>A short musical sketch.</p><audio controls preload="metadata" aria-label="Play Atlas theme"><source src="assets/demo/atlas-theme.wav" type="audio/wav"></audio><div class="dp-audio-meta" data-demo-reference="00:00 to 00:05">00:05 · Mono · WAV</div></div>';

  window.ChimaeraDemoPreviews = {
    files: [
      { id: 'brief', name: 'brief.md', folder: 'docs', icon: 'markdown', kind: 'markdown', html: brief, source: '# Make the next step clear.\n\nOne place to plan the work, see progress, and decide what comes next.\n\n> [!NOTE] The goal\n> A clear weekly review. Shared numbers. Fewer open questions.\n\n## This week\n\nWe completed **124 tasks** across four teams. The remaining **18 tasks** are assigned and ready for the next review.\n\n- Review the results with each team.\n- Publish the report and the progress chart.\n- Agree on the next three priorities.\n\n[Results](../data/results.csv) · [Weekly report](../reports/weekly-report.pdf)' },
      { id: 'csv', name: 'results.csv', folder: 'data', icon: 'table', kind: 'csv', html: csv, source: 'team,completed,remaining,progress\nDesign,32,4,89%\nProduct,28,5,85%\nEngineering,40,6,87%\nOperations,24,3,89%' },
      { id: 'xlsx', name: 'project-plan.xlsx', folder: 'data', icon: 'spreadsheet', kind: 'xlsx', html: workbook, render: renderWorkbook },
      { id: 'pdf', name: 'weekly-report.pdf', folder: 'reports', icon: 'pdf', kind: 'pdf', html: renderPDF(0), render: renderPDF },
      { id: 'docx', name: 'proposal.docx', folder: 'docs', icon: 'document', kind: 'docx', html: proposal },
      { id: 'pptx', name: 'review.pptx', folder: 'reports', icon: 'slides', kind: 'pptx', html: slide, render: renderSlides },
      { id: 'python', name: 'summarize.py', folder: 'src', icon: 'code', kind: 'code', html: python, source: pythonLines.map(function (line) { return line.replace(/<[^>]*>/g, ''); }).join('\n') },
      { id: 'notebook', name: 'explore.ipynb', folder: 'analysis', icon: 'notebook', kind: 'notebook', html: notebook },
      { id: 'svg', name: 'progress.svg', folder: 'figures', icon: 'image', kind: 'svg', html: graphic },
      { id: 'parquet', name: 'events.parquet', folder: 'data', icon: 'table', kind: 'parquet', html: parquet },
      { id: 'diagram', name: 'workflow.drawio', folder: 'docs', icon: 'diagram', kind: 'diagram', html: diagram },
      { id: 'html', name: 'report.html', folder: 'reports', icon: 'html', kind: 'html', html: htmlReport },
      { id: 'video', name: 'overview.mp4', folder: 'media', icon: 'video', kind: 'video', html: video },
      { id: 'audio', name: 'theme.wav', folder: 'media', icon: 'audio', kind: 'audio', html: audio }
    ],
    browserHTML: browserHTML
  };
})();
