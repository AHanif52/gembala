// UI text (TECH section 20): one file per language, keys in en.ts; a key missing from id.ts falls back to English.
// Pure module so `node --test` can import it; the reactive language lives in state.svelte.ts.
import en from "./en.ts";
import id from "./id.ts";

export type Key = keyof typeof en;
export type Lang = "en" | "id";
export type Params = Record<string, string | number>;
const DICT: Record<Lang, Partial<Record<Key, string>>> = { en, id };

export const hasKey = (k: string): k is Key => k in en;

/// `{name}` is replaced from `p`; a key missing everywhere shows itself
export function translate(lang: Lang, key: Key, p?: Params): string {
  const s = DICT[lang][key] ?? en[key] ?? key;
  return p ? s.replace(/\{(\w+)\}/g, (m, k) => (k in p ? String(p[k]) : m)) : s;
}

/// plural: `key.one` / `key.other` picked by Intl.PluralRules, `{n}` filled in; Indonesian only needs `.other`
export function translateN(lang: Lang, key: string, n: number, p?: Params): string {
  const form = `${key}.${new Intl.PluralRules(lang).select(n)}`;
  return translate(lang, (hasKey(form) && DICT[lang][form] !== undefined ? form : `${key}.other`) as Key, { n, ...p });
}

/// `system` = first supported language in the macOS list (`system_lang`), anything else = English
export function resolveLang(pref: string, system: readonly string[]): Lang {
  if (pref === "id" || pref === "en") return pref;
  for (const l of system) {
    const c = l.slice(0, 2).toLowerCase();
    if (c === "id" || c === "en") return c;
  }
  return "en";
}

/// daemon error or config problem (core/src/diag.rs): `code` → `err.<code>`, `at` in front, `then` → `err.then.<then>`
/// after it. Anything else (free text, OS text, older daemon) shows as is.
export function errorText(lang: Lang, e: unknown, args?: Record<string, string>): string {
  if (e && typeof e === "object" && "code" in e) return errorText(lang, (e as { code: string }).code, (e as { args?: Record<string, string> }).args);
  const s = String(e ?? "").replace(/^Error: /, "");
  if (!hasKey(`err.${s}`)) return s;
  const text = translate(lang, `err.${s}` as Key, args);
  const then = args?.then && hasKey(`err.then.${args.then}`) ? `; ${translate(lang, `err.then.${args.then}` as Key, args)}` : "";
  return `${args?.at ? `${args.at}: ` : ""}${text}${then}`;
}

/// FR-84 / SEC-33: native banners may only show a fixed waiting reason.
export function waitingReasonKey(reason: string): Key | undefined {
  const keys: Key[] = ["reason.permission", "reason.input", "reason.dialog", "reason.trust", "reason.worker", "reason.sandbox"];
  return keys.find((key) => id[key] === reason);
}
