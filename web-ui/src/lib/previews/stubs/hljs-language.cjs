// Stands in for the highlight.js grammars Marp would otherwise bundle (all
// ~190 of them, >1 MB) — see vite.config.ts. A fenced block in one of these
// languages renders as plain monospace; the common languages stay real.
module.exports = function plain() {
  return { name: "plain", disableAutodetect: true, contains: [] };
};
