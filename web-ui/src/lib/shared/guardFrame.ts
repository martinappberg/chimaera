/**
 * The navigation guard for a framed page (an agent's page, a plugin's web
 * view): the page's sandboxed frame sits inside a wrapper document of this
 * window's own origin whose CSP says `frame-src 'self'`. A navigation of a
 * frame is checked against its *container's* `frame-src` before any request
 * leaves, so a page that tries `location = "https://elsewhere/?data"` (or a
 * link, a meta refresh) is stopped there: nothing reaches the other site.
 * The page's own CSP (the daemon's) covers everything else it could send.
 * The native app's view needs none of this: its shell refuses the
 * navigation itself.
 *
 * The wrapper only relays: a message from its own page goes up to this
 * window, a message from this window goes down to the page. So
 * `paneFrame.ts` binds to the wrapper's `<iframe>` as it would to the page
 * itself (`e.source` is the wrapper; the page API accepts commands whose
 * source is its parent, the wrapper). A press in the page still activates
 * this window (activation reaches every ancestor), and focus in the page is
 * focus in the wrapper's frame.
 *
 * The wrapper is NOT sandboxed: `'self'` in its CSP must be this window's
 * origin, the daemon's, which the page's own URL is on. It runs only the
 * relay below; the page inside never shares its origin.
 *
 * The wrapper wears this window's colour scheme, from its first paint and
 * across theme switches. A frame whose document's scheme differs from its
 * element's gets an opaque backdrop in the document's Canvas colour, so a
 * scheme-less wrapper in a dark window flashed white each time a view was
 * created, until the page inside painted over it.
 */

const escapeAttr = (s: string): string =>
  s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/** This window's mode, as the appearance settings write it on the root. */
const windowMode = (): "light" | "dark" =>
  typeof document !== "undefined" && document.documentElement.dataset.theme === "dark" ? "dark" : "light";

/** The wrapper's `srcdoc` for the page at `url` (same-origin, `/raw/…` or
 *  `/web/…`), titled `title`, starting in `mode`. Only `url` and `title`
 *  belong in a caller's reactive dependencies: a new `srcdoc` reloads the
 *  page, and the wrapper follows a theme switch by itself. */
export function guardedPage(url: string, title: string, mode: "light" | "dark" = windowMode()): string {
  return (
    "<!doctype html><html><head>" +
    `<meta http-equiv="Content-Security-Policy" content="frame-src 'self'">` +
    `<style>html{color-scheme:${mode}}` +
    "html,body{margin:0;height:100%;overflow:hidden;background:transparent}" +
    "iframe{display:block;border:0;width:100%;height:100%}</style>" +
    "</head><body>" +
    `<iframe sandbox="allow-scripts" referrerpolicy="no-referrer" src="${escapeAttr(url)}" title="${escapeAttr(title)}"></iframe>` +
    "<script>" +
    "const f=document.querySelector('iframe');" +
    "addEventListener('message',(e)=>{" +
    "if(f.contentWindow!==null&&e.source===f.contentWindow)parent.postMessage(e.data,'*');" +
    "else if(e.source===parent&&f.contentWindow!==null)f.contentWindow.postMessage(e.data,'*');" +
    "});" +
    // Same origin as this window, so the wrapper reads its root's scheme
    // itself; the observer goes with the wrapper's document.
    "try{const r=parent.document.documentElement;" +
    "const follow=()=>{document.documentElement.style.colorScheme=getComputedStyle(r).colorScheme};" +
    "const o=new MutationObserver(follow);" +
    "o.observe(r,{attributes:true,attributeFilter:['data-theme','style','class']});" +
    "addEventListener('pagehide',()=>o.disconnect());follow()}catch{}" +
    "</script></body></html>"
  );
}
