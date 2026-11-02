// @ts-nocheck -- dijalankan `npm test` (node --test), bukan bagian build app
import { test } from "node:test";
import assert from "node:assert/strict";
import { safeResume } from "./resume.ts";

test("perintah lanjut dari daemon lolos", () => {
  for (const c of [
    "claude --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb",
    "codex resume 01a19b1c-2cfb-7290-b33f-af39083bb39b",
    "codex resume --last",
    "opencode --continue",
    "omp --continue",
    "omp --resume 01a19b03-01a0-709b-87c6-c94faead61ab",
  ])
    assert.equal(safeResume(c), c);
});

test("perintah lain dari layout.json ditolak", () => {
  for (const c of [
    "claude --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb; rm -rf ~",
    "claude --resume x",
    "omp --resume a$(id)",
    "codex resume --last\nrm -rf ~",
    "bash -c id",
    "",
    null,
    42,
  ])
    assert.equal(safeResume(c), null);
});
