# Chimaera Pro connections

An optional account connection in the native app. It keeps remote hosts reachable
through an authenticated keeper, relays SSH login prompts, and offers the local
daemon to other signed-in devices. Ordinary SSH connections and the free daemon
continue to work without an account.

**Status: partial.** This page covers the native connection and settings surface.
Cloud workers, project mirroring, automatic handoff and a phone app are not
provided by these controls.

## How it is used

1. Open Settings → Chimaera Pro in the native app. Settings is available from
   Home before opening a workspace, and through the native Settings menu. A browser window has no Pro
   category. A build with no configured endpoint shows only
   “Chimaera Pro isn't available in this build.”
2. With an endpoint configured, choose **Sign in**. Complete the system-browser
   sign-in; the app receives an authorization code through its loopback callback.
3. The panel shows the signed-in email, plan, hosts and devices. Toggle **Keep
   connected** for a saved SSH host. A password or Duo challenge uses the usual
   host-scoped prompt, with “Asked by your Pro connection” underneath its title.
4. Open that host from Home. Its **via Pro** label identifies the connection;
   workspaces still open through a local loopback port with the existing daemon UI.
5. **Sign out** removes this app's credentials and closes its link connections.
   **Sign out everywhere** also revokes other devices and closes SSH logins held
   by the keeper.

A developer can configure `pro.endpoint` in the native app's `app.json` as
`{"pro":{"endpoint":"http://127.0.0.1:PORT"}}`. The file is under
`chimaera_core::config_dir()` (`$CHIMAERA_HOME/config` in an isolated development
run). It is separate from the daemon's settings JSON and takes effect when the app
starts. Tokens never belong in this file. Use the
[loopback fixture](../../crates/chimaera-link/PROTOCOL.md#fixture-and-conformance)
for a local integration run.

## Where it lives

| Surface | Entry points |
| --- | --- |
| Settings and native bridge | `web-ui/src/lib/settings/ProSettings.svelte`, `SettingsView.svelte`, `web-ui/src/lib/net/native.ts` |
| Host and prompt labels | `web-ui/src/lib/workspace/HomeScreen.svelte`, `AskpassModal.svelte` |
| App account lifecycle | `crates/chimaera-app/src/shell/pro.rs` |
| App connections and prompt routing | `crates/chimaera-app/src/shell/connect.rs`, `askpass.rs` |
| Device transport and wire types | `crates/chimaera-link/src/`, [protocol](../../crates/chimaera-link/PROTOCOL.md) |

Native IPC commands: `pro_status`, `pro_sign_in`, `pro_sign_out`,
`pro_sign_out_everywhere`, `pro_hosts`, `pro_set_host_kept`, `pro_devices`.
The app broadcasts `pro-changed` when account/host state changes. The panel
refreshes while visible and catches up when shown again; it does not poll while
parked. None of these commands change the daemon↔UI protocol.

## Constraints and edge cases

- Default endpoint is unset. The Pro transport starts no sockets until explicitly
  configured and signed in; the existing SSH route remains the signed-out fallback.
- Account credentials live in the OS keychain. Refresh-token rotations replace the
  stored pair. Daemon bearer tokens remain in memory and do not enter `hosts.json`.
- SSH aliases resolve on the device. Only hostname, username and port are passed
  to the keeper; local private keys and arbitrary SSH configuration are not copied.
- HTTPS/WSS is required except for literal `127.0.0.1` fixtures. `/v1/me` negotiates
  the supported protocol and keeper origin. A newly signed-in account may still
  be awaiting an assigned keeper; account and device information remains available.
- Data bridges have bounded queues, 64 KiB frames and at most 128 streams.
  Closing one stream does not change the tunnel listener's port.
- Prompt answers remain scoped to the relevant host. Cancellation and expiry
  dismiss prompts in other eligible windows too.
- The local daemon is reverse-served only while the signed-in app owns that link.
  Signing out or quitting closes the offer. Device-host rows have no “Keep
  connected” toggle because their owning device controls availability.

---

## Intent — human-authored ground truth

_No intent captured yet — pending the maintainer's feature-intent review. The
implemented behavior above is derived from code, not a replacement for intent._
