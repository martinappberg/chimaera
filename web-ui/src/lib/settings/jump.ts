/**
 * Open Settings at a section from elsewhere (the dashboard's usage line,
 * Quick Open "Usage"): App opens the settings tab, then asks for the section
 * here; SettingsView scrolls to it once it shows and clears the request.
 */
import { writable } from "svelte/store";

export const settingsJump = writable<string | null>(null);

export function requestSettingsSection(section: string): void {
  settingsJump.set(section);
}
