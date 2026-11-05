// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import test from "node:test";
import assert from "node:assert/strict";
import { envPatch, envRows } from "./env.ts";

test("nama env dari daemon jadi baris kosong", () => {
  assert.deepEqual(envRows(["OPENAI_API_KEY", "TOKEN"]), [
    { name: "OPENAI_API_KEY", value: "" },
    { name: "TOKEN", value: "" },
  ]);
});

test("baris kosong tidak dikirim, jadi nilai lama di config.toml tetap", () => {
  assert.deepEqual(envPatch(envRows(["TOKEN"])), {});
});

test("hanya baris berisi yang dikirim, spasi dibuang, karakter = ikut di nilai", () => {
  assert.deepEqual(envPatch([{ name: " TOKEN ", value: " a=b " }, { name: "", value: "x" }, { name: "EMPTY", value: "" }]), { TOKEN: "a=b" });
});

test("nama sama dua kali: yang terakhir menang", () => {
  assert.deepEqual(envPatch([{ name: "A", value: "1" }, { name: "A", value: "2" }]), { A: "2" });
});
