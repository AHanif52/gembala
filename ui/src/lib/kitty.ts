/// kitty keyboard protocol, flag 1 only (disambiguate): agents can tell Ctrl+Enter/Shift+Enter/Esc apart
/// (Claude Code and others query `CSI ? u`). Other flags are ignored, the answer is what's supported.
/// ponytail: one stack for the main and alternate screens; split it if an app depends on that.
export const SUPPORTED = 1;

/// CSI `?u` (query), `>u` (push), `<u` (pop), `=u` (set). Returns a reply for the app if there is one.
export function onCsi(stack: number[], prefix: string, params: number[]): string | undefined {
  const cur = stack.at(-1) ?? 0;
  const [a = 0, b = 1] = params;
  if (prefix === "?") return `\x1b[?${cur}u`;
  if (prefix === ">") {
    stack.push(a & SUPPORTED);
    if (stack.length > 16) stack.shift();
  } else if (prefix === "<") stack.splice(-Math.max(1, a));
  else if (prefix === "=") {
    const f = a & SUPPORTED;
    const next = b === 2 ? cur | f : b === 3 ? cur & ~f : f;
    if (stack.length) stack[stack.length - 1] = next;
    else stack.push(next);
  }
}

type Key = { key: string; code: string; ctrlKey: boolean; altKey: boolean; shiftKey: boolean; metaKey: boolean };

/// CSI-u sequence for keys that are ambiguous in legacy mode; null = leave it to xterm (plain text, arrows, F-keys, Cmd).
/// Enter/Tab/Backspace without modifiers stay legacy so `reset` can still be typed in the shell (per the spec).
export function encodeKey(e: Key, optionIsMeta: boolean): string | null {
  if (e.metaKey) return null;
  const alt = e.altKey && optionIsMeta;
  const mods = 1 + (e.shiftKey ? 1 : 0) + (alt ? 2 : 0) + (e.ctrlKey ? 4 : 0);
  const m = mods > 1 ? `;${mods}` : "";
  if (e.key === "Escape") return `\x1b[27${m}u`;
  const special = ({ Enter: 13, Tab: 9, Backspace: 127 } as Record<string, number>)[e.key];
  if (special) return mods > 1 ? `\x1b[${special}${m}u` : null;
  if (!e.ctrlKey && !alt) return null;
  // key code without Shift/Option (Option on macOS turns e.key into another letter)
  const base = /^Key([A-Z])$/.exec(e.code)?.[1].toLowerCase() ?? /^Digit(\d)$/.exec(e.code)?.[1] ?? (e.key.length === 1 ? e.key.toLowerCase() : null);
  return base ? `\x1b[${base.codePointAt(0)}${m}u` : null;
}
