// Stands in for mathjax-full (>2 MB), which Marp imports unconditionally
// but only calls when rendering with `math: "mathjax"`. SlidesView renders
// with KaTeX (MathML output, the app's math policy), so none of these run.
// See vite.config.ts.
function unavailable() {
  throw new Error("MathJax is not bundled; render Marp math with KaTeX");
}
exports.liteAdaptor = unavailable;
exports.RegisterHTMLHandler = unavailable;
exports.TeX = unavailable;
exports.SVG = unavailable;
exports.AllPackages = [];
exports.mathjax = { document: unavailable };
