// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import en from "./en.ts";
import id from "./id.ts";
import { errorText, resolveLang, translate, translateN, waitingReasonKey } from "./index.ts";

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

// heuristic: Indonesian UI words left in strings or template text (comments and <style> skipped); should stay empty
const ID_WORDS = /\b(yang|dan|tidak|nggak|udah|belum|untuk|dari|pakai|buka|tutup|simpan|batal|hapus|kirim|ubah|salin|sudah|bisa|lagi|semua|baru|saat|bila|atau|gagal|tersalin|berjalan|jalan|lewat|dengan|masih|ini|itu|ada|kosong|butuh|izin|selesai|bekerja|pilih|ganti|nama|warna|pindah|lainnya|sebelumnya|berikutnya)\b/i;
const ALLOW: string[] = [];
test("no Indonesian UI text left outside id.ts", () => {
  const left = [];
  for (const f of files(SRC).filter((f) => !f.includes("/i18n/") && !f.endsWith(".test.ts"))) {
    let text = readFileSync(f, "utf8").replace(/<!--[\s\S]*?-->|\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ""));
    const style = text.indexOf("<style");
    if (style >= 0) text = text.slice(0, style);
    text.split("\n").forEach((line, i) => {
      const code = line.replace(/(^|\s)\/\/.*$/, "");
      for (const part of code.match(/"[^"]*"|`[^`]*`|'[^']*'|>[^<>{}]*</g) ?? [])
        if (ID_WORDS.test(part) && !ALLOW.some((a) => part.includes(a))) left.push(`${f.split("/src/")[1]}:${i + 1}: ${part.slice(0, 80)}`);
    });
  }
  assert.deepEqual(left, []);
});

test("native waiting reasons are restricted to the fixed dictionary", () => {
  for (const key of ["reason.permission", "reason.input", "reason.dialog", "reason.trust", "reason.worker", "reason.sandbox"]) {
    assert.equal(waitingReasonKey(id[key]), key);
  }
  assert.equal(waitingReasonKey("private customer data"), undefined);
  assert.equal(waitingReasonKey(""), undefined);
});
