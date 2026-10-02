/* A local sample workspace. Uses the app's glyphs and pane conventions; no agent calls. */
(function () {
  "use strict";
  var root = document.getElementById("workspace-demo");
  var previews = window.ChimaeraDemoPreviews;
  if (!root || !previews || !previews.files.length) return;
  var fileList = root.querySelector("#wb-files");
  var content = root.querySelector("#wb-preview");
  var tabStrip = root.querySelector("#wb-tabs");
  var chat = root.querySelector("#wb-chat-messages");
  var composer = root.querySelector("#wb-composer");
  var reference = root.querySelector("#wb-reference");
  var refButton = root.querySelector("#wb-ref-button");
  var browserContent = root.querySelector("#wb-browser-content");
  var current = (previews.files.find(function (file) { return file.kind === "pdf"; }) || previews.files[0]).id;
  var openTabs = current === previews.files[0].id ? [current] : [previews.files[0].id, current];
  var agent = "claude";
  var source = false;
  var selection = "";
  var expanded = false;
  var oldOverflow = "";
  var inertSiblings = [];
  var backdrop = null;
  var narrow = window.matchMedia("(max-width: 700px)");
  var returnFocus = null;
  var draftReference = null;
  var filter = "";
  var period = "week";
  var paths = {};
  var agents = {
    claude: { name: "Claude Code", glyph: "M8 2.5l1.4 3.6 3.6 1.4-3.6 1.4L8 12.5 6.6 8.9 3 7.5l3.6-1.4z", task: "Build the project overview", reply: "The report and interactive overview are ready. I kept the source data alongside them so you can check the results." },
    codex: { name: "Codex", glyph: "M6.4 4.8L3.6 8l2.8 3.2M9.6 4.8L12.4 8l-2.8 3.2", task: "Review the results", reply: "I checked the report against the source table. The totals agree: 124 completed and 18 remaining. The notebook records the calculation." },
    agy: { name: "Antigravity", glyph: "M8 5.1m-1.5 0a1.5 1.5 0 1 0 3 0a1.5 1.5 0 1 0-3 0M3.7 11.4q4.3-3.1 8.6 0", task: "Explore the project", reply: "The brief, data, and report are in the project folder. I can work with these files and the tools configured for this workspace." },
    grok: { name: "Grok Build", glyph: "M4 12L12 4M4 4h4M8 12h4", task: "Explore the project", reply: "The project files are ready to use. I can help develop the report, inspect the data, or work on the browser view." }
  };
  function escape(value) {
    return String(value).replace(/[&<>"']/g, function (c) { return {"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]; });
  }
  function icon(name) {
    var glyph = window.ChimaeraDemoIcons[name] || window.ChimaeraDemoIcons.text;
    return '<svg class="wb-icon wb-icon--' + glyph.c + '" viewBox="0 0 24 24" aria-hidden="true">' + glyph.d.map(function (d) { return '<path d="' + d + '"/>'; }).join("") + '</svg>';
  }
  function agentIcon(id) {
    return '<svg class="wb-agent-icon" viewBox="0 0 16 16" aria-hidden="true"><path d="' + agents[id].glyph + '"/></svg>';
  }
  var views = {
    _dashboard: { name: "Dashboard", icon: "board", html: '<div class="wb-page"><div class="wb-page-eyebrow">project atlas</div><h2>Dashboard</h2><p class="wb-secondary">Two agents. One project.</p><h3>Since you left</h3><div class="wb-activity"><strong>Project overview completed</strong><p>Claude Code created the report and browser view.</p></div><div class="wb-activity"><strong>Source data reviewed</strong><p>Codex checked the totals and recorded the calculation.</p></div><button class="wb-text-button" data-open="_timeline">Open Timeline →</button></div>' },
    _timeline: { name: "Timeline", icon: "logs", html: '<div class="wb-page"><div class="wb-page-eyebrow">today</div><h2>Timeline</h2><div class="wb-activity"><small>10:27 · Codex</small><strong>Review completed</strong><p>Checked the report against the source data.</p></div><div class="wb-activity"><small>10:24 · Claude Code</small><strong>Report and browser view created</strong><p>Source files kept alongside the outputs.</p></div><div class="wb-activity"><small>10:18 · Project knowledge</small><strong>Decision recorded</strong><p>Keep the original data with every report.</p></div></div>' },
    _knowledge: { name: "Knowledge", icon: "notebook", html: '<div class="wb-page"><div class="wb-page-eyebrow">recorded in project files</div><h2>Knowledge</h2><div class="wb-knowledge-tabs"><span>Decisions</span><span>Findings</span><span>Handoffs</span></div><div class="wb-activity"><small>Decision</small><strong>Keep the source data</strong><p>Store the original table beside the report so every result can be checked.</p></div><div class="wb-activity"><small>Finding</small><strong>124 complete. 18 remaining.</strong><p>The report and interactive overview use the same source table.</p></div><div class="wb-activity"><small>Handoff</small><strong>Ready for the next review</strong><p>Compare this period with the previous one.</p></div></div>' },
    _extensions: { name: "Extensions", icon: "box", html: '<div class="wb-page"><h2>Extensions</h2><div class="wb-knowledge-tabs"><span>Plugins</span><span>Connections</span><span>Skills</span></div><div class="wb-activity"><small>Workbench plugin</small><strong>Project knowledge <span class="wb-enabled">On</span></strong><p>Findings, decisions, and handoffs in the workspace.</p></div><div class="wb-activity"><small>Agent setup</small><strong>Your skills and connections</strong><p>Available from the agents configured on this host.</p></div><a href="docs.html#extensions" class="wb-text-button">About extensions →</a></div>' },
    _terminal: { name: "dev server", icon: "shell", html: '<div class="wb-terminal"><p><span>~/project-atlas</span> $ npm run dev</p><p class="wb-secondary">&gt; atlas@1.0.0 dev<br>&gt; vite</p><p><span>VITE</span> ready in 182 ms</p><p>Local: <button data-action="focus-browser">http://localhost:5173/</button></p><p class="wb-secondary">Watching for file changes.</p></div>' }
  };
  previews.files.forEach(function (file) { paths[file.id] = file; });
  function item(id) { return paths[id] || views[id]; }
  function renderFiles() {
    var focusedFile = fileList.contains(document.activeElement) ? document.activeElement.dataset.open : null;
    var scrollTop = fileList.scrollTop;
    fileList.innerHTML = previews.files.filter(function (file) { return (file.name + " " + file.kind).toLowerCase().includes(filter.toLowerCase()); }).map(function (file) {
      return '<button type="button" class="wb-file" data-open="' + escape(file.id) + '" aria-label="Open ' + escape(file.name) + '" aria-pressed="' + (file.id === current) + '" title="' + escape(file.kind + ' · ' + (file.folder ? file.folder + '/' : '') + file.name) + '">' + icon(file.icon) + '<span>' + escape(file.name) + '</span></button>';
    }).join("") || '<p class="wb-empty">No matching files</p>';
    fileList.scrollTop = scrollTop;
    if (focusedFile) {
      var next = fileList.querySelector('[data-open="' + focusedFile + '"]');
      if (next) next.focus({preventScroll:true});
    }
  }
  function renderTabs() {
    var restoreFocus = tabStrip.contains(document.activeElement);
    tabStrip.innerHTML = openTabs.map(function (id) {
      var file = item(id);
      return '<div class="wb-tab-item"><button type="button" role="tab" id="wb-tab-' + id + '" aria-selected="' + (id === current) + '" aria-controls="wb-preview" tabindex="' + (id === current ? '0' : '-1') + '" class="wb-tab" data-open="' + id + '">' + icon(file.icon) + '<span>' + escape(file.name) + '</span></button>' + (openTabs.length > 1 ? '<button class="wb-tab-close" data-close-tab="' + id + '" aria-label="Close ' + escape(file.name) + '">×</button>' : '') + '</div>';
    }).join("");
    content.setAttribute("aria-labelledby", "wb-tab-" + current);
    if (restoreFocus) root.querySelector("#wb-tab-" + current).focus({preventScroll:true});
  }
  function prepareReferences() {
    content.querySelectorAll("[data-demo-reference]").forEach(function (target) {
      target.tabIndex = 0;
      target.setAttribute("role", "button");
      target.setAttribute("aria-pressed", "false");
      target.setAttribute("aria-label", "Select " + target.dataset.demoReference + ": " + target.textContent.trim().replace(/\s+/g, " ").slice(0, 200));
    });
  }
  function selectReference(target) {
    selection = target.dataset.demoReference;
    refButton.textContent = "Reference selection";
    refButton.classList.add("wb-has-selection");
    content.querySelectorAll("[data-demo-reference]").forEach(function (element) {
      element.classList.toggle("wb-selected-region", element === target);
      element.setAttribute("aria-pressed", String(element === target));
    });
  }
  function stopMedia() {
    root.querySelectorAll("video,audio").forEach(function (media) { media.pause(); });
  }
  function renderPreview() {
    stopMedia();
    var file = item(current);
    content.innerHTML = source && file.source ? '<pre class="wb-source"><code>' + escape(file.source) + '</code></pre>' : file.html;
    prepareReferences();
    content.scrollTop = 0;
    root.querySelector("#wb-path").textContent = (file.folder ? file.folder + " / " : "") + file.name;
    var sourceToggle = root.querySelector("#wb-source");
    sourceToggle.hidden = !file.source;
    sourceToggle.textContent = source ? "Reading" : "Source";
    sourceToggle.setAttribute("aria-pressed", String(source));
    refButton.hidden = !paths[current];
    refButton.textContent = "Reference file";
    refButton.classList.remove("wb-has-selection");
    root.querySelectorAll("[data-nav]").forEach(function (button) { button.setAttribute("aria-pressed", String(button.dataset.open === current)); });
    renderTabs();
    renderFiles();
  }
  function syncSidebar() {
    var closed = narrow.matches && !root.classList.contains("wb-sidebar-open");
    root.querySelector("#wb-sidebar").inert = closed;
    root.querySelector('[data-action="sidebar"]').setAttribute("aria-expanded", String(!closed));
  }
  function closeSidebar() {
    root.classList.remove("wb-sidebar-open");
    syncSidebar();
  }
  function open(id) {
    if (!item(id)) return;
    current = id; source = false; selection = "";
    if (!openTabs.includes(id)) { openTabs.push(id); if (openTabs.length > 4) openTabs.shift(); }
    root.dataset.mobilePanel = "file";
    root.dataset.paneFocus = root.dataset.paneFocus === "browser" ? "" : root.dataset.paneFocus;
    renderPreview();
    updateMobile();
    closeSidebar();
  }
  function renderChat() {
    var person = agents[agent];
    var brief = previews.files[0];
    var data = previews.files.find(function (file) { return /\.csv$/i.test(file.name); }) || brief;
    var pdf = previews.files.find(function (file) { return /\.pdf$/i.test(file.name); }) || brief;
    var html = previews.files.find(function (file) { return /\.html$/i.test(file.name); }) || brief;
    var notebook = previews.files.find(function (file) { return /\.ipynb$/i.test(file.name); }) || brief;
    root.querySelector("#wb-agent-tab").innerHTML = agentIcon(agent) + '<span>' + escape(person.task) + '</span>';
    root.querySelector("#wb-agent-name").textContent = person.name;
    chat.innerHTML = '<div class="wb-chat-turn wb-chat-user"><div class="wb-chat-label">You</div><div class="wb-attachments"><button data-open="' + brief.id + '">' + icon(brief.icon) + escape(brief.name) + '</button><button data-open="' + data.id + '">' + icon(data.icon) + escape(data.name) + '</button></div><p>' + (agent === "codex" ? 'Check the report against the source data. Keep a record of the calculation.' : 'Turn these notes and data into a report and a small interactive page. Keep the source files alongside the results.') + '</p></div>' +
      '<details class="wb-tool"><summary>' + icon("code") + 'Read project files<span>3 files</span></summary><div>' + escape(brief.name) + '<br>' + escape(data.name) + '<br>' + escape(notebook.name) + '</div></details>' +
      '<div class="wb-chat-turn"><div class="wb-chat-label">' + agentIcon(agent) + escape(person.name) + '</div><p>' + escape(person.reply) + '</p><div class="wb-artifacts"><button data-open="' + pdf.id + '">' + icon(pdf.icon) + '<span>' + escape(pdf.name) + '<small>Open document</small></span>↗</button><button data-open="' + html.id + '">' + icon(html.icon) + '<span>' + escape(html.name) + '<small>Open report</small></span>↗</button></div></div>';
    composer.placeholder = "Message " + person.name;
    root.querySelectorAll("[data-agent]").forEach(function (button) { button.setAttribute("aria-pressed", String(button.dataset.agent === agent)); });
  }
  function renderReference() {
    reference.hidden = !draftReference;
    reference.innerHTML = draftReference ? '<span>' + icon(draftReference.icon) + escape(draftReference.name + (draftReference.part ? " · " + draftReference.part : "")) + '</span><button data-action="clear-reference" aria-label="Remove file reference">×</button>' : "";
  }
  function addReference() {
    var file = item(current);
    draftReference = {name:file.name,icon:file.icon,part:selection};
    renderReference();
    root.dataset.mobilePanel = "chat";
    root.dataset.paneFocus = "";
    updateMobile();
    composer.focus({preventScroll:true});
    root.querySelector("#wb-status").textContent = "Added " + file.name + (selection ? " · " + selection : "") + " to the draft";
  }
  function updateMobile() {
    root.querySelectorAll("[data-mobile]").forEach(function (button) { button.setAttribute("aria-pressed", String(button.dataset.mobile === root.dataset.mobilePanel)); });
  }
  function toggleExpanded() {
    expanded = !expanded;
    root.classList.toggle("wb-expanded", expanded);
    var button = root.querySelector('[data-action="expand"]');
    button.setAttribute("aria-label", expanded ? "Close expanded workspace" : "Expand workspace preview");
    button.setAttribute("aria-pressed", String(expanded));
    root.setAttribute("role", expanded ? "dialog" : "region");
    if (expanded) {
      returnFocus = document.activeElement;
      oldOverflow = document.body.style.overflow;
      document.body.style.overflow = "hidden";
      root.setAttribute("aria-modal", "true");
      backdrop = document.createElement("div");
      backdrop.className = "wb-backdrop";
      backdrop.addEventListener("click", toggleExpanded);
      document.body.appendChild(backdrop);
      var branch = root;
      while (branch.parentElement) {
        Array.from(branch.parentElement.children).forEach(function (sibling) {
          if (sibling !== branch && sibling !== backdrop) {
            inertSiblings.push({element:sibling,wasInert:sibling.inert});
            sibling.inert = true;
          }
        });
        branch = branch.parentElement;
        if (branch === document.body) break;
      }
      button.focus({preventScroll:true});
    } else {
      document.body.style.overflow = oldOverflow;
      root.removeAttribute("aria-modal");
      inertSiblings.forEach(function (entry) { entry.element.inert = entry.wasInert; });
      inertSiblings = [];
      if (backdrop) backdrop.remove();
      backdrop = null;
      if (returnFocus && returnFocus.isConnected) returnFocus.focus({preventScroll:true});
    }
  }
  root.addEventListener("click", function (event) {
    var target = event.target.closest("button,[data-demo-reference]");
    if (!target || !root.contains(target)) return;
    if (target.dataset.open) { open(target.dataset.open); return; }
    if (target.dataset.agent) { agent = target.dataset.agent; renderChat(); root.dataset.paneFocus = ""; root.dataset.mobilePanel = "chat"; updateMobile(); closeSidebar(); root.querySelector(".wb-agent-menu").open = false; return; }
    if (target.dataset.closeTab) {
      var id = target.dataset.closeTab;
      openTabs = openTabs.filter(function (tab) { return tab !== id; });
      if (current === id) current = openTabs[openTabs.length - 1];
      source = false; selection = ""; renderPreview(); root.querySelector("#wb-tab-" + current).focus({preventScroll:true}); return;
    }
    if (target.hasAttribute("data-demo-view-index")) {
      var file = item(current);
      if (typeof file.render === "function") {
        var restoreViewFocus = document.activeElement === target;
        selection = "";
        refButton.textContent = "Reference file";
        refButton.classList.remove("wb-has-selection");
        content.innerHTML = file.render(Number(target.dataset.demoViewIndex));
        prepareReferences();
        content.scrollTop = 0;
        if (restoreViewFocus) content.querySelector('[data-demo-view-index][aria-pressed="true"]').focus({preventScroll:true});
      }
      return;
    }
    if (target.dataset.demoPeriod) {
      var restorePeriodFocus = document.activeElement === target;
      period = target.dataset.demoPeriod;
      browserContent.innerHTML = previews.browserHTML(period);
      if (restorePeriodFocus) browserContent.querySelector('[data-demo-period][aria-pressed="true"]').focus({preventScroll:true});
      return;
    }
    if (target.dataset.demoReference) {
      selectReference(target);
      return;
    }
    if (target.dataset.mobile) { stopMedia(); root.dataset.mobilePanel = target.dataset.mobile; updateMobile(); return; }
    switch (target.dataset.action) {
      case "reference": addReference(); break;
      case "clear-reference": draftReference = null; renderReference(); break;
      case "source": source = !source; selection = ""; renderPreview(); break;
      case "expand": toggleExpanded(); break;
      case "focus-file": root.dataset.paneFocus = root.dataset.paneFocus === "file" ? "" : "file"; break;
      case "focus-browser": root.dataset.paneFocus = root.dataset.paneFocus === "browser" ? "" : "browser"; root.dataset.mobilePanel = "browser"; updateMobile(); break;
      case "sidebar": root.classList.toggle("wb-sidebar-open"); syncSidebar(); break;
      case "search": var search = root.querySelector("#wb-search"); search.hidden = !search.hidden; if (!search.hidden) search.focus(); break;
      case "reload-browser": browserContent.innerHTML = previews.browserHTML(period); break;
    }
  });
  content.addEventListener("keydown", function (event) {
    if (event.key !== "Enter" && event.key !== " ") return;
    var target = event.target.closest("[data-demo-reference]");
    if (!target || event.target !== target) return;
    event.preventDefault();
    selectReference(target);
    // Keep selection and insertion adjacent for keyboard navigation.
    refButton.focus({preventScroll:true});
  });
  root.querySelector("#wb-search").addEventListener("input", function (event) { filter = event.target.value; renderFiles(); });
  tabStrip.addEventListener("keydown", function (event) {
    if (!["ArrowLeft","ArrowRight","Home","End"].includes(event.key)) return;
    event.preventDefault();
    var index = openTabs.indexOf(current);
    index = event.key === "Home" ? 0 : event.key === "End" ? openTabs.length - 1 : (index + (event.key === "ArrowRight" ? 1 : -1) + openTabs.length) % openTabs.length;
    open(openTabs[index]);
    root.querySelector("#wb-tab-" + current).focus({preventScroll:true});
  });
  fileList.addEventListener("keydown", function (event) {
    if (!["ArrowUp","ArrowDown","Home","End"].includes(event.key)) return;
    var buttons = Array.from(fileList.querySelectorAll("button"));
    if (!buttons.length) return;
    event.preventDefault();
    var index = buttons.indexOf(document.activeElement);
    index = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : (index + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
    buttons[index].focus({preventScroll:true});
    buttons[index].scrollIntoView({block:"nearest"});
  });
  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape") {
      if (expanded) { event.preventDefault(); toggleExpanded(); }
      closeSidebar();
      root.dataset.paneFocus = "";
    }
    if (event.key === "Tab" && expanded) {
      var focusable = Array.from(root.querySelectorAll('button:not([disabled]),a[href],textarea,input,summary,[tabindex="0"]')).filter(function (element) { return element.getClientRects().length > 0; });
      var first = focusable[0], last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    }
  });
  document.addEventListener("visibilitychange", function () { if (document.hidden) stopMedia(); });
  window.addEventListener("pagehide", function () { stopMedia(); if (expanded) toggleExpanded(); });
  narrow.addEventListener("change", syncSidebar);
  if ("IntersectionObserver" in window) {
    var mediaVisibility = new IntersectionObserver(function (entries) { if (!entries[0].isIntersecting) stopMedia(); });
    mediaVisibility.observe(root);
  }
  renderPreview(); renderChat(); renderReference(); updateMobile(); syncSidebar();
  browserContent.innerHTML = previews.browserHTML(period);
  root.dataset.ready = "true";
})();
