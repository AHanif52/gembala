// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import en from "./en.ts";
import id from "./id.ts";
import { errorText, resolveLang, translate, translateN } from "./index.ts";

const params = (s) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort().join(",");
const SRC = new URL("../..", import.meta.url).pathname;
const files = (d) => readdirSync(d).flatMap((f) => (statSync(join(d, f)).isDirectory() ? files(join(d, f)) : /\.(svelte|ts)$/.test(f) ? [join(d, f)] : []));

test("id.ts only has keys from en.ts, with the same {params}", () => {
  for (const [k, v] of Object.entries(id)) {
    assert.ok(k in en, `unknown key in id.ts: ${k}`);
    assert.equal(params(v), params(en[k]), `params differ: ${k}`);
  }
  for (const k of Object.keys(en).filter((k) => /\.(one|few|many)$/.test(k))) assert.ok(k.replace(/\.\w+$/, ".other") in en, `plural without .other: ${k}`);
});

test("every t()/tn() key used in src exists in en.ts", () => {
  const keys = Object.keys(en);
  for (const f of files(SRC).filter((f) => !f.includes("/i18n/"))) {
    const text = readFileSync(f, "utf8");
    // `tt`/`ttn` = alias in +page.svelte, where `t` is often a tab
    for (const [, q, k] of text.matchAll(/\b(?:t|tn|tt|ttn)\((["`])([^"`]+)\1/g)) {
      if (q === "`" && k.includes("${")) {
        const prefix = k.slice(0, k.indexOf("${"));
        assert.ok(keys.some((x) => x.startsWith(prefix)), `${f}: no key starts with ${prefix}`);
      } else assert.ok(keys.includes(k) || keys.includes(`${k}.other`), `${f}: missing key ${k}`);
    }
  }
});

test("id.ts coverage (I18N_STRICT=1 fails on gaps)", () => {
  const missing = Object.keys(en).filter((k) => !(k in id));
  console.log(`id.ts: ${Object.keys(en).length - missing.length}/${Object.keys(en).length} keys`);
  if (process.env.I18N_STRICT === "1") assert.deepEqual(missing, []);
});

test("translate: params, fallback to English, plural, language pick", () => {
  assert.equal(translate("id", "set.general.secs", { n: 30 }), "30 detik");
  assert.equal(translate("en", "set.general.secs", { n: 30 }), "30 seconds");
  assert.equal(translate("id", "no.such.key"), "no.such.key");
  assert.equal(resolveLang("system", ["fr-FR", "id-ID"]), "id");
  assert.equal(resolveLang("system", ["en-ID", "id-ID"]), "en");
  assert.equal(resolveLang("system", ["fr-FR"]), "en");
  assert.equal(resolveLang("id", ["en-US"]), "id");
  assert.equal(translateN("en", "no.such", 1), "no.such.other");
});

test("errorText: code + at + then from core/src/diag.rs, free text as is", () => {
  const d = { code: "int_range", args: { at: "ui.font_size", lo: "8", hi: "32", then: "default", default: "13" } };
  assert.equal(errorText("en", d), "ui.font_size: has to be a whole number 8–32; using the default 13");
  assert.equal(errorText("id", d), "ui.font_size: harus bilangan bulat 8–32; memakai bawaan 13");
  assert.equal(errorText("id", "target_busy"), "Target lagi kerja; kirim paksa kalau yakin");
  assert.equal(errorText("en", "Error: target_busy"), "The target is working; force send if you're sure");
  assert.equal(errorText("en", "font size salah"), "font size salah");
  assert.equal(errorText("en", "account_exists", { id: "kantor" }), 'account "kantor" already exists');
});
