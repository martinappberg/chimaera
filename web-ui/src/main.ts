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
import { installReloadHook } from "./lib/layout/windowReload";
import { isAccountHome } from "./lib/net/base";

// Before mount: the native Reload Window must still reach a window whose App
// fails to boot.
installReloadHook();

// The native Reload Window (menu.rs RELOAD_WINDOW_JS) reads this mount point
// to tell a page that never booted from an older UI without the hook.
const target = document.getElementById("app");
if (!target) {
  throw new Error("missing #app mount point");
}

// The web's Home (the account's own signed-in page) has no daemon behind it,
// so it mounts its own Home and Settings instead of the workbench, which
// would start asking one for workspaces and sessions. Loaded only there.
const app = isAccountHome()
  ? import("./lib/pro/AccountHome.svelte").then(({ default: AccountHome }) => mount(AccountHome, { target }))
  : mount(App, { target });

export default app;
