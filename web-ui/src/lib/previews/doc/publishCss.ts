/**
 * The stylesheet an exported document carries: the reading view's look
 * (MarkdownView's `.md-doc` rules and app.css's markdown-table recipe) with
 * a light theme's tokens pinned, sized for a standalone page, plus print
 * rules — page margins, and no break inside a figure, table row, code
 * block, alert or diagram, nor right after a heading. Keep it in step with
 * those rules when they change; it is a copy because the page must carry
 * everything itself.
 *
 * Print keeps the page's colors (a done task's box, an alert's tint and
 * glyph are drawn as backgrounds, which a print drops by default).
 *
 * Fonts: the system's. Prose uses the UI stack, code the system monospace
 * (the app's bundled JetBrains Mono is not embedded), and equations are
 * KaTeX's MathML, which the browser draws with its own math font — no
 * font files, nothing to fetch.
 */

/** Alert glyphs (the reading view's, as masks painted in the title's color). */
const ICONS: Record<string, string> = {
  note: "%3Ccircle cx='12' cy='12' r='9'/%3E%3Cpath d='M12 8h.01M11 12h1v4h1'/%3E",
  tip: "%3Cpath d='M3 12h1m8-9v1m8 8h1M5.6 5.6l.7.7m12.1-.7-.7.7M9 16a5 5 0 1 1 6 0a3.5 3.5 0 0 0-1 3a2 2 0 0 1-4 0a3.5 3.5 0 0 0-1-3M9.7 17h4.6'/%3E",
  important:
    "%3Cpath d='M18 4a3 3 0 0 1 3 3v8a3 3 0 0 1-3 3h-5l-5 3v-3H6a3 3 0 0 1-3-3V7a3 3 0 0 1 3-3zM12 8v3M12 14v.01'/%3E",
  warning:
    "%3Cpath d='M12 9v4M10.4 3.6L2.3 17.1a1.9 1.9 0 0 0 1.6 2.9h16.2a1.9 1.9 0 0 0 1.6-2.9L13.6 3.6a1.9 1.9 0 0 0-3.2 0zM12 16h.01'/%3E",
  caution:
    "%3Cpath d='M12.8 2.6l8.6 8.6a1.1 1.1 0 0 1 0 1.6l-8.6 8.6a1.1 1.1 0 0 1-1.6 0l-8.6-8.6a1.1 1.1 0 0 1 0-1.6l8.6-8.6a1.1 1.1 0 0 1 1.6 0zM12 8v4M12 16h.01'/%3E",
};
const ALERT_TINT: Record<string, string> = {
  note: "var(--syn-func)",
  tip: "var(--syn-string)",
  important: "var(--rate)",
  warning: "var(--warn)",
  caution: "var(--err)",
};

function alertRules(): string {
  return Object.keys(ICONS)
    .map(
      (k) =>
        `.markdown-alert-${k}{--md-alert:${ALERT_TINT[k]};--md-alert-icon:url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E${ICONS[k]}%3C/svg%3E")}`,
    )
    .join("\n");
}

/** `tokens`: a light theme's CSS custom properties; `highlight`: the code
 *  highlighter's class rules (they read the `--syn-*` tokens). */
export function exportCss(tokens: Readonly<Record<string, string>>, highlight: string): string {
  const vars = Object.entries(tokens)
    .filter(([k]) => /^--[a-z0-9-]+$/.test(k))
    .map(([k, v]) => `${k}:${v.replace(/[;{}<]/g, "")};`)
    .join("");
  return `
:root{color-scheme:light;${vars}
--ui-font:system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;
--mono:ui-monospace,"SF Mono",SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace}
*{box-sizing:border-box}
html{background:var(--bg)}
body{margin:0;color:var(--fg);font-family:var(--ui-font);font-size:16px;line-height:1.65;-webkit-text-size-adjust:100%;text-size-adjust:100%}
.page{max-width:46rem;margin:0 auto;padding:2.6rem 1.5rem 4rem}
.md-doc{overflow-wrap:break-word}
.doc-meta{margin:0 0 1.8em;padding-bottom:1em;border-bottom:1px solid var(--edge)}
.doc-title{font-size:1.9em;line-height:1.2;margin:0 0 .3em;font-weight:650;letter-spacing:-.015em}
.doc-summary{margin:.2em 0;font-size:1.08em;color:color-mix(in srgb,var(--fg) 78%,var(--muted))}
.doc-updated{margin:.5em 0 0;font-size:.85em;color:var(--muted)}
.md-doc>h1+.doc-meta{margin:-.2em 0 1.6em;padding:0;border:0}
.md-doc>:first-child{margin-top:0}
.md-doc h1,.md-doc h2,.md-doc h3,.md-doc h4,.md-doc h5,.md-doc h6{line-height:1.25;margin:1.6em 0 .55em;font-weight:600;letter-spacing:-.01em;position:relative}
.md-doc h1{font-size:1.576em;margin-top:.2em;padding-bottom:.35em;border-bottom:1px solid var(--edge)}
.md-doc h2{font-size:1.25em;padding-bottom:.25em;border-bottom:1px solid var(--edge)}
.md-doc h3{font-size:1.087em}
.md-doc h4,.md-doc h5,.md-doc h6{font-size:1em}
.md-doc a.anchor{position:absolute;left:-1.1em;padding-right:.3em;color:var(--muted);text-decoration:none;opacity:0}
.md-doc a.anchor::before{content:"#"}
.md-doc :is(h1,h2,h3,h4,h5,h6):hover a.anchor{opacity:.7}
.md-doc p{margin:.7em 0}
.md-doc a{color:var(--accent);text-decoration:none}
.md-doc a:hover{text-decoration:underline}
.md-doc a.wikilink{text-decoration:underline dotted color-mix(in srgb,var(--accent) 55%,transparent);text-underline-offset:.18em}
.md-doc code{font-family:var(--mono);font-size:.84em;background:color-mix(in srgb,var(--fg) 6%,transparent);border-radius:4px;padding:.12em .34em}
.md-doc pre{background:color-mix(in srgb,var(--fg) 4.5%,transparent);border:1px solid var(--edge);border-radius:8px;padding:.8em 1em;overflow:hidden;line-height:1.5}
.md-doc pre code{display:block;overflow-x:auto;background:none;padding:0;font-size:.848em;white-space:pre}
.md-doc blockquote{margin:.8em 0;padding:.55em 1em;border-left:3px solid color-mix(in srgb,var(--accent) 60%,transparent);border-radius:0 8px 8px 0;background:linear-gradient(to right,color-mix(in srgb,var(--accent) 5%,transparent),color-mix(in srgb,var(--fg) 3%,transparent) 55%);color:color-mix(in srgb,var(--fg) 45%,var(--muted))}
.md-doc blockquote>:first-child{margin-top:0}
.md-doc blockquote>:last-child{margin-bottom:0}
.md-doc ul,.md-doc ol{padding-left:1.6em;margin:.6em 0}
.md-doc li{margin:.2em 0}
.md-doc li::marker{color:color-mix(in srgb,var(--accent) 70%,var(--muted))}
.md-doc hr{border:none;border-top:1px solid var(--edge);margin:1.8em 0}
.md-doc img{max-width:100%;height:auto}
.md-doc table{display:block;overflow-x:auto;padding:1px;margin:1em 0;border-collapse:collapse;font-size:.924em}
.md-doc table :is(th,td){border:1px solid var(--edge);padding:.35em .7em;text-align:left;font-variant-numeric:tabular-nums}
.md-doc table :is(th,td)[align="center" i]{text-align:center}
.md-doc table :is(th,td)[align="right" i]{text-align:right}
.md-doc table th{font-weight:600;background:color-mix(in srgb,var(--fg) 4%,transparent)}
.md-doc .md-math{color:inherit}
.md-doc math{font-family:"Latin Modern Math","STIX Two Math","Cambria Math",math;font-size:1.02em}
.md-doc .md-math-display{display:block;max-width:100%;overflow-x:auto;overflow-y:hidden;margin:.55em 0;padding:.1em 0}
.md-doc .md-mermaid{margin:.9em 0}
.md-doc .md-mermaid svg{display:block;max-width:100%;height:auto;margin:0 auto}
.md-doc .md-mermaid-note{margin:0 0 .4em;font-family:var(--mono);font-size:.76em;white-space:pre-wrap;color:var(--err)}
.md-doc .md-task{display:inline-block;position:relative;width:.92em;height:.92em;margin:0 .45em 0 0;vertical-align:-.12em;border:1.5px solid color-mix(in srgb,var(--fg) 38%,transparent);border-radius:3px;background:var(--term-bg)}
.md-doc .md-task[data-task="done"]{background:var(--accent);border-color:var(--accent)}
.md-doc .md-task[data-task="done"]::after{content:"";position:absolute;left:30%;top:8%;width:28%;height:58%;border:solid var(--term-bg);border-width:0 .13em .13em 0;transform:rotate(45deg)}
.md-doc ul>li.md-task-item{list-style:none}
.md-doc ul>li.md-task-item>.md-task:first-child,.md-doc ul>li.md-task-item>p:first-child>.md-task:first-child{margin-left:-1.35em;margin-right:.43em}
.md-doc .md-task-text{color:var(--muted);text-decoration:line-through;text-decoration-color:color-mix(in srgb,var(--muted) 70%,transparent)}
.markdown-alert{--md-alert:var(--syn-func);margin:.9em 0;padding:.55em 1em .6em;border-left:3px solid color-mix(in srgb,var(--md-alert) 75%,transparent);border-radius:0 8px 8px 0;background:color-mix(in srgb,var(--md-alert) 7%,transparent)}
${alertRules()}
.markdown-alert-title{display:flex;align-items:center;gap:.45em;margin:0 0 .25em;font-weight:600;font-size:.94em;color:var(--md-alert)}
.markdown-alert-title::before{content:"";flex:none;width:1.05em;height:1.05em;background:currentColor;-webkit-mask:var(--md-alert-icon) center/contain no-repeat;mask:var(--md-alert-icon) center/contain no-repeat}
.markdown-alert>:last-child{margin-bottom:0}
.markdown-alert>.markdown-alert-title+*{margin-top:0}
.md-doc section.footnotes{margin-top:2.2em;padding-top:.6em;border-top:1px solid var(--edge);font-size:.88em;color:color-mix(in srgb,var(--fg) 75%,var(--muted))}
.md-doc .footnote-ref a,.md-doc a.footnote-backref{font-variant-numeric:tabular-nums}
.md-doc .md-crop{display:block;position:relative;overflow:hidden;max-width:100%}
.md-doc .md-crop img{position:absolute;max-width:none}
.md-doc .md-file{display:flex;align-items:center;gap:.6em;max-width:100%;padding:.55em .8em;border:1px solid color-mix(in srgb,var(--edge) 80%,var(--fg) 8%);border-radius:8px;background:color-mix(in srgb,var(--fg) 2.5%,transparent);color:var(--fg);text-decoration:none;line-height:1.35}
.md-doc .md-file:hover{text-decoration:none;border-color:color-mix(in srgb,var(--accent) 45%,var(--edge))}
.md-doc .md-file svg{flex:none;width:1.25em;height:1.25em;color:var(--muted)}
.md-doc .md-file-name{font-family:var(--mono);font-size:.86em;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.md-doc .md-file-frag{flex:none;padding:0 .55em;border-radius:999px;font-size:.78em;background:color-mix(in srgb,var(--accent) 11%,transparent);color:color-mix(in srgb,var(--accent) 80%,var(--fg))}
.md-doc .md-file-alt{margin-left:auto;padding-left:.6em;font-size:.82em;color:var(--muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.md-doc .md-image-link{color:var(--muted);font-style:italic}
${highlight}
@page{margin:16mm 15mm}
@media print{
html{-webkit-print-color-adjust:exact;print-color-adjust:exact}
html,body{background:#fff}
body{font-size:10.5pt;line-height:1.5}
.page{max-width:none;padding:0}
.md-doc a{color:inherit;text-decoration:underline;text-decoration-color:color-mix(in srgb,var(--accent) 60%,transparent)}
.md-doc a.anchor{display:none}
.md-doc :is(h1,h2,h3,h4,h5,h6){break-after:avoid;page-break-after:avoid}
.md-doc :is(pre,blockquote,.markdown-alert,.md-mermaid,.md-math-display,img,.md-crop,.md-file,li,tr){break-inside:avoid;page-break-inside:avoid}
.md-doc p:has(>img:only-child){break-inside:avoid}
.md-doc pre code{white-space:pre-wrap;overflow:visible}
.md-doc table{display:table;overflow:visible}
.md-doc thead{display:table-header-group}
.md-doc .md-math-display{overflow:visible}
.md-doc p,.md-doc li{orphans:3;widows:3}
}
`;
}
