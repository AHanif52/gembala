// Active UI language. `t()` reads `i18n.lang`, so every template expression calling it re-renders on a switch
// (no restart). Constants evaluated once won't: make them functions or `$derived`.
import { errorText, hasKey, translate, translateN, type Key, type Lang, type Params } from "./index";

export const i18n = $state({ lang: "en" as Lang });
export const t = (k: Key, p?: Params) => translate(i18n.lang, k, p);
export const tn = (k: string, n: number, p?: Params) => translateN(i18n.lang, k, n, p);
/// dynamic key (`msg.kind.${kind}`): its text, or `fallback` when there's no such key
export const tOr = (k: string, fallback: string, p?: Params) => (hasKey(k) ? t(k, p) : fallback);
/// locale for dates and numbers
export const loc = () => (i18n.lang === "id" ? "id-ID" : "en-US");
/// daemon error code or config problem in the active language (see `errorText`)
export const errText = (e: unknown, args?: Record<string, string>) => errorText(i18n.lang, e, args);
