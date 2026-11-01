// @ts-nocheck -- dijalankan `npm test` (node --test), bukan bagian build app
import { test } from "node:test";
import assert from "node:assert/strict";
import { encodeKey, onCsi } from "./kitty.ts";

const k = (key, code, mods = {}) => ({ key, code, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...mods });

test("tumpukan flag: tanya, push, set, pop", () => {
  const s = [];
  assert.equal(onCsi(s, "?", []), "\x1b[?0u");
  onCsi(s, ">", [31]); // app minta semua flag, yang didukung cuma 1
  assert.equal(onCsi(s, "?", []), "\x1b[?1u");
  onCsi(s, "=", [1, 3]); // and-not
  assert.equal(onCsi(s, "?", []), "\x1b[?0u");
  onCsi(s, "=", [1, 2]); // or
  onCsi(s, "<", []);
  assert.deepEqual(s, []);
});

test("tombol ambigu jadi CSI-u, sisanya legacy", () => {
  assert.equal(encodeKey(k("Enter", "Enter", { ctrlKey: true }), false), "\x1b[13;5u");
  assert.equal(encodeKey(k("Enter", "Enter", { shiftKey: true }), false), "\x1b[13;2u");
  assert.equal(encodeKey(k("Enter", "Enter"), false), null);
  assert.equal(encodeKey(k("Escape", "Escape"), false), "\x1b[27u");
  assert.equal(encodeKey(k("Tab", "Tab", { shiftKey: true }), false), "\x1b[9;2u");
  assert.equal(encodeKey(k("x", "KeyX", { ctrlKey: true }), false), "\x1b[120;5u");
  assert.equal(encodeKey(k("S", "KeyS", { ctrlKey: true, shiftKey: true }), false), "\x1b[115;6u");
  assert.equal(encodeKey(k("a", "KeyA"), false), null);
  assert.equal(encodeKey(k("A", "KeyA", { shiftKey: true }), false), null);
  assert.equal(encodeKey(k("c", "KeyC", { metaKey: true }), false), null);
  // Option bukan Meta = teks komposisi milik macOS; Option sebagai Meta = Alt
  assert.equal(encodeKey(k("å", "KeyA", { altKey: true }), false), null);
  assert.equal(encodeKey(k("å", "KeyA", { altKey: true }), true), "\x1b[97;3u");
  assert.equal(encodeKey(k("ArrowUp", "ArrowUp", { ctrlKey: true }), false), null);
});
