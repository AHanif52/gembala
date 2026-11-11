// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { KINDS, blockedKind, clear, enabled, newState, plan, settle, soundOn, where } from "./notice.ts";

const ev = (pane, kind, now, over = {}) => ({
  pane,
  kind,
  now,
  title: `p${pane} · Codex`,
  body: `${kind} · ws`,
  sound: true,
  merged: (n) => ({ title: `${n} done`, body: "ws" }),
  ...over,
});

test("urgensi per jenis", () => {
  for (const k of ["permission", "answer", "password"]) assert.deepEqual([KINDS[k].bounce, KINDS[k].loud], [true, true], k);
  for (const k of ["limit", "failed", "approve"]) assert.deepEqual([KINDS[k].bounce, KINDS[k].loud], [false, true], k);
  assert.deepEqual([KINDS.done.bounce, KINDS.done.loud], [false, false]);
  assert.equal(KINDS.permission.act, "answer");
  assert.equal(KINDS.failed.act, "open");
});

test("jenis dari alasan blocked: hanya 'input' yang nunggu jawaban", () => {
  assert.equal(blockedKind("reason.input"), "answer");
  for (const r of ["reason.permission", "reason.dialog", "reason.trust", "reason.worker", "reason.sandbox", undefined]) assert.equal(blockedKind(r), "permission");
});

test("saklar: on_blocked induk tiga jenis, kunci lama tetap terbaca", () => {
  const old = { on_blocked: true, on_done: true, sound: true }; // config from before FR-87
  for (const k of Object.keys(KINDS)) assert.equal(enabled(k, old), true, k);
  assert.equal(enabled("password", { on_blocked: false }), false);
  assert.equal(enabled("failed", { on_blocked: false }), true);
  assert.equal(enabled("answer", { on_answer: false }), false);
  assert.equal(enabled("permission", { on_answer: false }), true);
  assert.equal(enabled("done", { on_done: false }), false);
  assert.equal(enabled("limit", { on_limit: false }), false);
});

test("suara: mendesak ikut sound, selesai hanya sound_done", () => {
  assert.equal(soundOn("permission", { sound: true }), true);
  assert.equal(soundOn("failed", { sound: false }), false);
  assert.equal(soundOn("done", { sound: true }), false);
  assert.equal(soundOn("done", { sound: true, sound_done: true }), true);
});

test("tab otomatis (label tugas) tidak ikut: tanpa nama tab = hanya workspace", () => {
  assert.equal(where("kantor", ""), "kantor");
  assert.equal(where("kantor", "api"), "kantor › api");
  assert.equal(where("", ""), "");
});

test("satu banner per pane: kejadian baru menarik yang lama", () => {
  const st = newState();
  const a = plan(st, ev(5, "answer", 0));
  assert.deepEqual([a.remove, a.show.title, a.show.bounce], [[], "p5 · Codex", true]);
  const b = plan(st, ev(5, "failed", 70_000));
  assert.deepEqual([b.remove, b.show.bounce], [["p5 · Codex"], false]);
});

test("kejadian sama dalam 60 dtk tidak bunyi lagi, sesudahnya bunyi", () => {
  const st = newState();
  assert.ok(plan(st, ev(5, "permission", 0)).show);
  assert.deepEqual(plan(st, ev(5, "permission", 59_000)), { remove: [] });
  assert.ok(plan(st, ev(5, "permission", 61_000)).show);
  // another pane is its own count
  assert.ok(plan(st, ev(6, "permission", 1_000)).show);
});

test("dijawab = banner ditarik, kejadian sama bunyi lagi; selesai tidak ditarik per pane", () => {
  const st = newState();
  plan(st, ev(5, "permission", 0));
  assert.deepEqual(settle(st, 5), ["p5 · Codex"]);
  assert.deepEqual(settle(st, 5), []);
  assert.ok(plan(st, ev(5, "permission", 10_000)).show);
  plan(st, ev(7, "done", 0));
  assert.deepEqual(settle(st, 7), []);
});

test("beberapa selesai dalam 5 dtk digabung; yang pertama tetap langsung", () => {
  const st = newState();
  const one = plan(st, ev(1, "done", 0));
  assert.equal(one.show.title, "p1 · Codex");
  const two = plan(st, ev(2, "done", 2_000));
  assert.deepEqual([two.show.title, two.show.pane, two.remove], ["2 done", 2, ["p1 · Codex"]]);
  const three = plan(st, ev(3, "done", 4_000));
  assert.deepEqual([three.show.title, three.show.pane], ["3 done", 3]);
  assert.deepEqual(three.remove.sort(), ["2 done"]);
  // the window moves with each addition; 6 s after the last one is a fresh single
  const later = plan(st, ev(4, "done", 10_001));
  assert.equal(later.show.title, "p4 · Codex");
  // a merged banner never bounces, a permission between them is untouched
  assert.equal(three.show.bounce, false);
});

test("jendela dilihat: semua banner ditarik dan mulai segar", () => {
  const st = newState();
  plan(st, ev(1, "done", 0));
  plan(st, ev(2, "done", 1_000));
  plan(st, ev(3, "permission", 1_500));
  assert.deepEqual(clear(st).sort(), ["2 done", "p3 · Codex"]);
  assert.ok(plan(st, ev(3, "permission", 2_000)).show);
  assert.equal(plan(st, ev(9, "done", 2_100)).show.title, "p9 · Codex");
});
