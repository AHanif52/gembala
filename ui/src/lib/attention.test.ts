// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { GROUP, frontCard, needPanes, needRows, word } from "./attention.ts";

const at = (s) => s * 1000;
const pane = (p, kind, s, over = {}) => ({ pane: p, kind, at: at(s), ...over });

test("strip: kelompok 1 dulu (agent berhenti), lalu 2, lalu 3; paling lama di atas", () => {
  const rows = needRows({
    panes: [
      pane(9, "permission", 10, { ask: "Bash command · npm test" }),
      pane(20, "answer", 5, { question: "Which database should store login sessions?" }),
      pane(21, "password", 30),
    ],
    messages: [{ id: "m1", at: at(1), from: "OpenCode · Migrasi API", to: "Codex · Audit diff", pane: 9 }],
    opens: [{ id: "o1", at: at(2), from: "Codex · Audit diff", to: "Oh My Pi", pane: 9 }],
    gates: [
      { run: "r1", ws: 0, at: at(0.5), ship: false, name: "Fitur baru" },
      { run: "r2", ws: 2, at: at(0.2), ship: true, name: "Rilis 2" },
    ],
    simple: true,
  });
  assert.deepEqual(
    rows.map((r) => r.kind),
    ["answer", "permission", "password", "message", "open", "ship", "gate"],
  );
  // the oldest gate is not on top: group 3 comes last, oldest first inside it
  assert.deepEqual(rows.map((r) => r.group), [1, 1, 1, 2, 2, 3, 3]);
  assert.equal(GROUP.permission, 1);
});

test("strip: kalimat menurut jenis, kata dari notice.ts", () => {
  const rows = needRows({
    panes: [
      pane(9, "permission", 10, { ask: "Bash command · npm test" }),
      pane(10, "permission", 10),
      pane(20, "answer", 5, { question: "Which database?" }),
      pane(21, "answer", 5),
      pane(22, "trust", 5),
      pane(23, "password", 5),
    ],
    messages: [],
    opens: [],
    gates: [],
    simple: true,
  });
  const by = Object.fromEntries(rows.map((r) => [r.key, r]));
  assert.equal(by["permission:9"].what, "need.permission");
  assert.deepEqual(by["permission:9"].params, { what: "Bash command · npm test" });
  assert.equal(by["permission:9"].act, "answer");
  // a dialog that was never read: no button in the strip, the sentence sends the user to the pane
  assert.equal(by["permission:10"].what, "need.permission.none");
  assert.equal(by["permission:10"].act, "open");
  assert.equal(by["answer:20"].what, "need.answer");
  assert.deepEqual(by["answer:20"].params, { q: "Which database?" });
  assert.equal(by["answer:21"].what, "need.answer.none");
  assert.equal(by["trust:22"].what, "need.trust");
  assert.equal(by["trust:22"].act, "open");
  assert.equal(by["password:23"].what, "need.password");
  assert.equal(by["password:23"].act, "open");
  // the word is FR-87's, so the strip and the bell can't drift
  assert.equal(word("permission"), "notice.permission");
  assert.equal(word("trust"), "notice.permission");
  assert.equal(word("password"), "notice.password");
  assert.equal(word("answer"), "notice.answer");
  assert.equal(word("message"), "notice.approve");
  assert.equal(word("ship"), "notice.approve");
});

test("strip: satu baris per hal yang nunggu, umur kartu pesan dipakai urutannya", () => {
  const rows = needRows({
    panes: [],
    messages: [
      { id: "baru", at: at(50), from: "A", to: "B", pane: 9 },
      { id: "lama", at: at(5), from: "B", to: "C", pane: 10 },
    ],
    opens: [
      { id: "o1", at: at(20), from: "C", to: "Oh My Pi", pane: 11 },
      { id: "o2", at: at(30), from: "D", to: "Codex", pane: 12 },
    ],
    gates: [],
    simple: true,
  });
  assert.deepEqual(rows.map((r) => r.key), ["message:lama", "open:o1", "open:o2", "message:baru"]);
  assert.deepEqual(needPanes(rows), [10, 11, 12, 9]);
  assert.equal(needPanes(rows).length, new Set(needPanes(rows)).size);
  // Lengkap says the fact instead of the sender sentence: same row, same order, different words
  const full = needRows({
    panes: [],
    messages: [{ id: "baru", at: at(50), from: "shell-11", to: "shell-9", pane: 9 }],
    opens: [{ id: "o1", at: at(20), from: "shell-9", to: "Oh My Pi", pane: 9 }],
    gates: [],
    simple: false,
  });
  assert.deepEqual(full.map((r) => r.what), ["need.openFull", "need.messageFull"]);
  // a row without a pane (the harness flow) is skipped by the shortcut
  const g = needRows({ panes: [], messages: [], opens: [], gates: [{ run: "r", ws: 0, at: 0, ship: false, name: "x" }], simple: true });
  assert.deepEqual(needPanes(g), []);
});

test("kartu satu pane: yang paling lama di depan, sisanya lipatan; 'Lihat' memindahkan depan", () => {
  const cards = [
    { id: "open", at: at(30) },
    { id: "msg", at: at(10) },
    { id: "report", at: at(20) },
  ];
  const f = frontCard(cards);
  assert.equal(f.front.id, "msg");
  assert.deepEqual(f.rest.map((c) => c.id), ["report", "open"]);
  const picked = frontCard(cards, "report");
  assert.equal(picked.front.id, "report");
  assert.deepEqual(picked.rest.map((c) => c.id), ["msg", "open"]);
  // a pick that no longer exists falls back to the oldest instead of an empty stack
  assert.equal(frontCard(cards, "gone").front.id, "msg");
  assert.deepEqual(frontCard([], "x"), { front: null, rest: [] });
});
