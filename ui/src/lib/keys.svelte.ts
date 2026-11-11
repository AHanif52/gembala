// Shortcut labels and the app modifier (FR-39, FR-98). The combos come from the Rust tables (`*_KEYS` in lib.rs) through
// `shortcuts`, so a label can't drift from the shortcut that runs: macOS shows ⌘, Linux/Windows Ctrl+Shift.
import { invoke } from "@tauri-apps/api/core";
import { pretty } from "./config";

export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.userAgent);

/// The app modifier is held: ⌘ on macOS; Ctrl+Shift elsewhere, where a plain Ctrl belongs to the terminal (FR-39).
export const mod = (e: { metaKey: boolean; ctrlKey: boolean; shiftKey: boolean }) => (isMac ? e.metaKey : e.ctrlKey && e.shiftKey);

/// `⌘` or `Ctrl+Shift`, for sentences that name the app key.
export const modLabel = () => (isMac ? "⌘" : "Ctrl+Shift");

/// A combo no menu action owns (⌘↩ in an editor, ⌘C/⌘V): the same text as a label, mapped for this system.
export const keyText = (mac: string) =>
  pretty(isMac ? mac : ["Ctrl", "Shift", ...mac.split("+").filter((m) => !["Cmd", "Ctrl", "Shift"].includes(m))].join("+"));

/// Quit follows the native menu, not the app key.
export const quitKey = () => (isMac ? "⌘Q" : "Ctrl+Q");

let combos = $state<Record<string, string>>({});

/// Label of a menu action (`tab.search`); empty before the first `loadCombos` and for an unknown id.
export const combo = (id: string) => pretty(combos[id] ?? "");

/// Read the combos once the page opens, and again after `[keys]` changes.
export async function loadCombos() {
  const r = await invoke<[[string, string, string][], string[]]>("shortcuts").catch(() => null);
  combos = Object.fromEntries((r?.[0] ?? []).map(([id, , key]) => [id, key]));
}
