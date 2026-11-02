// @ts-nocheck -- dijalankan `npm test` (node --test), bukan bagian build app
import { test } from "node:test";
import assert from "node:assert/strict";
import { cleanTitle } from "./label.ts";

test("judul agent jadi label", () => {
  assert.equal(cleanTitle("✳ Fix login redirect"), "Fix login redirect");
  assert.equal(cleanTitle("π > Test Gov KAI Straight tier approval", "reimbursement_backbone"), "Test Gov KAI Straight tier approval");
  assert.equal(cleanTitle("OC | Refactor auth"), "Refactor auth");
  assert.equal(cleanTitle("⠙ Review BE"), "Review BE");
  assert.equal(cleanTitle("x".repeat(80)).length, 60);
});

test("judul generik diabaikan", () => {
  for (const t of ["✳ Claude Code", "OpenCode", "π >", "⠋ gtitle", "", undefined]) assert.equal(cleanTitle(t, "gtitle"), "");
  assert.equal(cleanTitle("π > gtitle", "gtitle"), "");
});
