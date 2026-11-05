// Active UI language. `t()` reads `i18n.lang`, so every template expression calling it re-renders on a switch
// (no restart). Constants evaluated once won't: make them functions or `$derived`.
import { hasKey, translate, translateN, type Key, type Lang, type Params } from "./index";

export const i18n = $state({ lang: "en" as Lang });
export const t = (k: Key, p?: Params) => translate(i18n.lang, k, p);
export const tn = (k: string, n: number, p?: Params) => translateN(i18n.lang, k, n, p);
/// locale for dates and numbers
export const loc = () => (i18n.lang === "id" ? "id-ID" : "en-US");
/// daemon error: known code → `err.<code>` text, anything else (free text, older daemon) as is
export const errText = (e: unknown) => {
  const s = String(e ?? "").replace(/^Error: /, "");
  return hasKey(`err.${s}`) ? t(`err.${s}` as Key) : s;
};
