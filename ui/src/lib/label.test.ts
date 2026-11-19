// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { cleanTitle, mentionLabel } from "./label.ts";

test("judul agent jadi label", () => {
  assert.equal(cleanTitle("✳ Fix login redirect"), "Fix login redirect");
  assert.equal(cleanTitle("π > Test Gov KAI Straight tier approval", "reimbursement_backbone"), "Test Gov KAI Straight tier approval");
  assert.equal(cleanTitle("OC | Refactor auth"), "Refactor auth");
  assert.equal(cleanTitle("⌘ Fix auth"), "Fix auth");
  assert.equal(cleanTitle("⠙ Review BE"), "Review BE");
  assert.equal(cleanTitle("x".repeat(80)).length, 60);
});

test("judul generik diabaikan", () => {
  for (const t of ["✳ Claude Code", "OpenCode", "⌘ Command Code", "commandcode", "cmdc", "π >", "⠋ gtitle", "", undefined]) assert.equal(cleanTitle(t, "gtitle"), "");
  assert.equal(cleanTitle("π > gtitle", "gtitle"), "");
});

test("label mention selalu tampilkan nama pane", () => {
  assert.equal(mentionLabel("agy-1", undefined, "Antigravity CLI"), "agy-1 · Antigravity CLI");
  assert.equal(mentionLabel("agy-2", "", "Antigravity CLI"), "agy-2 · Antigravity CLI");
  assert.equal(mentionLabel("shell-2", "CR Reboundary di dev-javan", "Shell"), "shell-2 · CR Reboundary di dev-javan");
  assert.equal(mentionLabel("claude", undefined, undefined), "claude");
  assert.equal(mentionLabel("claude", undefined, "Claude"), "claude");
});
