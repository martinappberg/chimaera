/**
 * Keeping Office documents offline. A .docx or .pptx is untrusted input: its
 * relationships can point at remote images and media, and docx-preview builds
 * CSS from strings in the document (font names, theme values), so a crafted
 * name can smuggle a `url(https://…)` into a stylesheet. The viewers render
 * into DETACHED nodes, run them through `sanitizeRendered` (where the
 * browser's own CSS parser decides what is a URL, so escapes can't hide
 * one), and only then attach them — nothing a document names is ever
 * fetched, and nothing in it runs.
 *
 * The string helpers are pure (tested); the DOM pass needs a browser.
 */

/** Sources a viewer may load: bytes already in this page (the zip's parts,
 *  handed out as blob: URLs) or inline data. Never the network. */
export function isInlineSource(url: string): boolean {
  return /^\s*(blob:|data:(image|font|audio|video)\/)/i.test(url);
}

/** Links a reader may follow (the viewer routes the click itself). */
export function isFollowableHref(href: string): boolean {
  return /^\s*(https?:|mailto:|#)/i.test(href);
}

/** Every `url(…)` target in a serialized CSS value. */
export function cssUrls(value: string): string[] {
  const out: string[] = [];
  const re = /url\(\s*(?:"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'|([^)]*?))\s*\)/gi;
  for (let m = re.exec(value); m !== null; m = re.exec(value)) out.push(m[1] ?? m[2] ?? m[3] ?? "");
  return out;
}

/**
 * Whether a CSS declaration could fetch something remote. Ordinary values
 * come back from the CSSOM with escapes decoded (`\75 rl(` reads `url(`); a
 * custom property keeps its raw tokens and is re-parsed wherever `var()`
 * lands it, so one carrying an escape or any url is refused outright.
 */
export function cssValueFetches(value: string, prop = ""): boolean {
  if (prop.startsWith("--") && /\\|url|image-set/i.test(value)) return true;
  if (/image-set\(|@import|expression\(/i.test(value)) return true;
  return cssUrls(value).some((u) => !isInlineSource(u) && !u.startsWith("#"));
}

/** Whether an (SVG) attribute value could fetch: a url() reference to
 *  anything but this document's own `#id`s or inline data, or an escape
 *  that could spell one. */
export function attrFetches(value: string): boolean {
  if (!/url|\\/i.test(value)) return false;
  if (/\\/.test(value)) return true;
  return cssUrls(value).some((u) => !isInlineSource(u) && !u.trim().startsWith("#"));
}

/** Relationship types a viewer may keep pointing outside the file: links,
 *  which only ever open on a click. */
const LINK_REL = /\/hyperlink$/i;

/**
 * A package relationships part (`*.rels`) with every external target that
 * would load something (a linked picture, video, audio, OLE object) removed.
 * External hyperlinks stay; the viewer routes their clicks.
 */
export function stripExternalRels(xml: string): { xml: string; removed: number } {
  let removed = 0;
  const out = xml.replace(/<Relationship\b[^>]*?\/?>/g, (tag) => {
    if (!/\bTargetMode\s*=\s*["']External["']/i.test(tag)) return tag;
    const type = /\bType\s*=\s*["']([^"']*)["']/i.exec(tag)?.[1] ?? "";
    if (LINK_REL.test(type)) return tag;
    removed += 1;
    return "";
  });
  return { xml: out, removed };
}

/** Elements a rendered document never needs and that could load or run things. */
const DROP = "script,iframe,frame,frameset,object,embed,link,meta,base,form,portal,template,noscript";

/** Rewrite one stylesheet's text through the CSSOM of an inert document:
 *  keep only rules whose every value stays in-page. */
function cleanStylesheet(css: string, inert: Document): { css: string; blocked: number } {
  const style = inert.createElement("style");
  style.textContent = css;
  inert.head.appendChild(style);
  let blocked = 0;
  const clean = (decls: Pick<CSSStyleDeclaration, "length" | "getPropertyValue" | "removeProperty"> & { [i: number]: string }): void => {
    for (let i = decls.length - 1; i >= 0; i--) {
      const prop = decls[i];
      if (cssValueFetches(decls.getPropertyValue(prop), prop)) {
        decls.removeProperty(prop);
        blocked += 1;
      }
    }
  };
  // Only rule kinds a document stylesheet needs survive; each is rebuilt
  // from its checked parts (a style rule's nested rules are dropped rather
  // than trusted through its cssText).
  const walk = (rules: CSSRuleList): string[] => {
    const out: string[] = [];
    for (const rule of Array.from(rules)) {
      if (rule instanceof CSSStyleRule) {
        clean(rule.style);
        if (rule.cssRules.length > 0) blocked += 1;
        out.push(`${rule.selectorText}{${rule.style.cssText}}`);
      } else if (rule instanceof CSSFontFaceRule || rule instanceof CSSPageRule || rule instanceof CSSKeyframeRule) {
        clean(rule.style);
        out.push(rule.cssText);
      } else if (rule instanceof CSSMediaRule || rule instanceof CSSSupportsRule || rule instanceof CSSKeyframesRule) {
        const inner = walk(rule.cssRules);
        const head = rule.cssText.slice(0, rule.cssText.indexOf("{"));
        out.push(`${head}{${inner.join("\n")}}`);
      } else {
        blocked += 1;
      }
    }
    return out;
  };
  const text = style.sheet === null ? "" : walk(style.sheet.cssRules).join("\n");
  style.remove();
  return { css: text, blocked };
}

/**
 * Make rendered, still-detached document nodes inert: drop active elements
 * and event handlers, keep only in-page sources, keep only followable links,
 * and strip any CSS (sheets and inline styles) that would fetch. Returns how
 * many things were blocked, for the viewer's quiet note.
 */
export function sanitizeRendered(nodes: Iterable<Node>): number {
  const inert = document.implementation.createHTMLDocument("");
  let blocked = 0;
  for (const node of nodes) {
    if (!(node instanceof Element)) continue;
    const all = [node, ...Array.from(node.querySelectorAll("*"))];
    for (const el of all) {
      if (el.matches(DROP)) {
        el.remove();
        blocked += 1;
        continue;
      }
      if (el instanceof HTMLStyleElement || el.localName === "style") {
        const res = cleanStylesheet(el.textContent ?? "", inert);
        el.textContent = res.css;
        blocked += res.blocked;
        continue;
      }
      for (const attr of Array.from(el.attributes)) {
        const name = attr.name.toLowerCase();
        if (name.startsWith("on") || name === "srcset" || name === "formaction" || name === "action") {
          el.removeAttribute(attr.name);
          continue;
        }
        if (name === "src" || name === "poster" || name === "data" || (name === "href" && el.localName !== "a") || name === "xlink:href") {
          if (!isInlineSource(attr.value)) {
            el.removeAttribute(attr.name);
            blocked += 1;
          }
          continue;
        }
        if (name === "href" && !isFollowableHref(attr.value)) {
          el.removeAttribute(attr.name);
          continue;
        }
        if (name === "target") {
          el.removeAttribute(attr.name);
          continue;
        }
        // SVG paint and effect attributes (fill, filter, mask, marker-*)
        // take url() references too.
        if (name !== "style" && attrFetches(attr.value)) {
          el.removeAttribute(attr.name);
          blocked += 1;
        }
      }
      const inline = (el as HTMLElement).style as CSSStyleDeclaration | undefined;
      if (inline !== undefined && inline.length > 0) {
        for (let i = inline.length - 1; i >= 0; i--) {
          const prop = inline[i];
          if (cssValueFetches(inline.getPropertyValue(prop), prop)) {
            inline.removeProperty(prop);
            blocked += 1;
          }
        }
      }
    }
  }
  return blocked;
}
