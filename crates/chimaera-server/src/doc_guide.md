# Writing documents in a Chimaera workspace

The user reads your files in Chimaera, a workbench that renders markdown, embeds
other files inside documents, and turns file references into links that open at
the right spot. Write plain, portable markdown: everything below also renders on
GitHub and in Obsidian, so a document you write here is ready to publish.

## The dialect

- CommonMark + GitHub Flavored Markdown: headings, lists, tables, task lists
  (`- [ ]`, `- [x]`), footnotes (`[^1]`), strikethrough, fenced code with a
  language (```` ```python ````).
- Alerts (GitHub syntax), one per blockquote, the marker alone on its first
  line (a title after it turns the alert back into a plain quote on GitHub):
  ```
  > [!NOTE]
  > Useful context the reader should not miss.
  ```
  Types: `NOTE`, `TIP`, `IMPORTANT`, `WARNING`, `CAUTION`.
- Math: `$E = mc^2$` inline, `$$ ... $$` or a ```` ```math ```` fence for display.
  Write `\$5` when a dollar sign is currency next to other dollars.
- Diagrams: a ```` ```mermaid ```` fence.
- Metadata: optional YAML frontmatter at the very top, shown as a properties panel:
  ```
  ---
  title: QC report, batch 7
  summary: One-sentence abstract.
  status: draft            # draft | review | final
  audience: internal       # internal | public
  updated: 2026-09-25
  tags: [qc, rnaseq]
  ---
  ```

## Links and embeds

- Link to other files with RELATIVE paths from the document's own folder:
  `[methods](methods.md)`, `[the figure script](../scripts/plot.py)`.
  Encode spaces as `%20`. Never write absolute local paths (`/home/...`,
  `/scratch/...`) in a document someone else will read.
- Link into a file with a fragment:
  - lines: `[the filter](src/filter.py#L40-L58)`
  - a heading: `[results](report.md#results)` (GitHub slug: lowercase, spaces to `-`)
  - a PDF page: `[Fig. 3](paper.pdf#page=4)`
- Embed a file with image syntax; it renders by file type:
  - image: `![UMAP of all cells, colored by cluster](figs/umap.png)`
  - PDF page: `![Supplementary table](supp.pdf#page=2)`
  - a section of another note: `![](protocol.md#reagents)`
  - code lines: `![](src/filter.py#L40-L58)`
  - table rows: `![](results/de.tsv#row=1-20)`; spreadsheet range:
    `![](summary.xlsx#sheet=Genes&range=A1:F20)`
  - an HTML report, video, audio, notebook cell or slide:
    `![](multiqc_report.html)`, `![](demo.mp4#t=30,45)`, `![](analysis.ipynb#cell=7)`,
    `![](deck.pptx#slide=3)`
  - optional width: `![UMAP|400](figs/umap.png)`
- Always write meaningful alt text: GitHub shows it wherever it cannot embed.
- Do not use Obsidian wikilinks (`[[note]]`, `![[file]]`), MDX, Markdoc tags,
  Pandoc or Quarto `:::` blocks, or MyST directives. They show as literal text on
  GitHub.

## Referring to files in your replies

When you mention a file to the user, write its workspace-relative path, with a
line when it matters: `src/filter.py:42` or `src/filter.py#L40-L58`. These open
in the workbench at that spot. A markdown link works too:
`[filter.py](src/filter.py#L40)`.

## Fragments you may receive from the user

The user can point you at part of any file. A reference is one line,
`@path#fragment (context) "quoted text"`; everything after the path is optional:

| Fragment | Meaning |
|---|---|
| `#L12-L20` | lines 12 to 20 |
| `#page=4` | PDF page 4 (the quote is text the user selected on it) |
| `#xywh=160,120,320,240` | a region (x, y, width, height from the top-left) of an image, in its pixels |
| `#page=4&xywh=72,90,200,120` | a region of PDF page 4, in PDF points (1/72 inch) from the page's top-left |
| `#t=12.5`, `#t=12.5,20` | a moment, or seconds 12.5 to 20, of a video or audio file |
| `#row=5-9`, `#cell=5,2-9,4` | rows, or a cell range (row,column), of a CSV/TSV: 1-based, counting data rows below the header |
| `#sheet=Summary&range=B2:F9` | a spreadsheet range in A1 notation (the sheet name URL-encoded) |
| `#cell=7`, `#slide=3` | a notebook cell, a slide (1-based) |

- `(§ Results)` after the fragment is the heading a markdown selection sits under.
- A table's quote is TSV on one line: the header first, `\t` between cells, `\n`
  between rows. It stops at 50 rows, 20 columns or 8 KB, and `…` marks a cut:
  read the file for the rest.
- `(region image: /path/ref-1.png)` at the end is a PNG of the region the user
  drew. Open it to see what they see.
- These fragments work in your own links too, and open at that spot:
  `[Fig. 2](paper.pdf#page=3&xywh=72,272,320,220)`, `results/de.tsv#row=5-9`.

## Slides

For a presentation, write a Marp deck: markdown with `marp: true` in the
frontmatter and `---` between slides. It renders as slides here and exports to
HTML or PDF. Keep one idea per slide and put figures in with image syntax.

## Before you hand a document over

Run the `check_document` tool on it. It reports broken links and embeds,
heading and line fragments that point nowhere, dangling footnotes, absolute
local paths, missing alt text, oversized images and syntax GitHub shows as
literal text, each with its line and a fix. Fix what it finds, then tell the
user the document's path.
