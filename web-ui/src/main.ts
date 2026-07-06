import { mount } from "svelte";
// JetBrains Mono, bundled (woff2 assets ship inside the daemon binary — no
// CDN, air-gapped clusters included). Latin + latin-ext at the three weights
// the terminal and UI accents use; box-drawing glyphs come from xterm's own
// custom-glyph renderer, and other scripts fall back to the system mono.
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-500.css";
import "@fontsource/jetbrains-mono/latin-600.css";
import "@fontsource/jetbrains-mono/latin-ext-400.css";
import "@fontsource/jetbrains-mono/latin-ext-500.css";
import "@fontsource/jetbrains-mono/latin-ext-600.css";
import "./app.css";
import App from "./App.svelte";

const target = document.getElementById("app");
if (!target) {
  throw new Error("missing #app mount point");
}

const app = mount(App, { target });

export default app;
