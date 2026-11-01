/// Protokol keyboard kitty, hanya flag 1 (disambiguate): Ctrl+Enter/Shift+Enter/Esc bisa dibedakan agent
/// (Claude Code dan lain-lain menanyakan `CSI ? u`). Flag lain diabaikan, dijawab apa adanya yang didukung.
/// ponytail: satu tumpukan untuk layar utama dan alternatif; pisahkan kalau ada app yang bergantung padanya.
export const SUPPORTED = 1;

/// CSI `?u` (tanya), `>u` (push), `<u` (pop), `=u` (set). Mengembalikan balasan untuk app bila ada.
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

/// Urutan CSI-u untuk tombol yang ambigu di mode legacy; null = biarkan xterm (teks biasa, panah, F-key, Cmd).
/// Enter/Tab/Backspace tanpa modifier tetap legacy supaya `reset` di shell tetap bisa diketik (sesuai spesifikasi).
export function encodeKey(e: Key, optionIsMeta: boolean): string | null {
  if (e.metaKey) return null;
  const alt = e.altKey && optionIsMeta;
  const mods = 1 + (e.shiftKey ? 1 : 0) + (alt ? 2 : 0) + (e.ctrlKey ? 4 : 0);
  const m = mods > 1 ? `;${mods}` : "";
  if (e.key === "Escape") return `\x1b[27${m}u`;
  const special = ({ Enter: 13, Tab: 9, Backspace: 127 } as Record<string, number>)[e.key];
  if (special) return mods > 1 ? `\x1b[${special}${m}u` : null;
  if (!e.ctrlKey && !alt) return null;
  // kode tombol tanpa Shift/Option (Option di macOS mengubah e.key jadi huruf lain)
  const base = /^Key([A-Z])$/.exec(e.code)?.[1].toLowerCase() ?? /^Digit(\d)$/.exec(e.code)?.[1] ?? (e.key.length === 1 ? e.key.toLowerCase() : null);
  return base ? `\x1b[${base.codePointAt(0)}${m}u` : null;
}
