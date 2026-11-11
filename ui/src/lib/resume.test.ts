// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { safeResume, suggestion } from "./resume.ts";

test("saran editor: perintah lanjut kartu sesi menang atas riwayat", () => {
  // the gray suggestion used to come from history first: typing `claude` offered `claude -c` while the card had --resume
  const hist = ["claude -c", "cargo test", "claude"];
  const zsh = ["claude -c", "claude --resume 11111111-1111-1111-1111-111111111111"];
  const byAgent = [
    "claude --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb",
    "claude-kantor --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb",
    "codex resume 01a19b1c-2cfb-7290-b33f-af39083bb39b",
    "opencode --session ses_4Ab-9",
    "omp --resume 01a19b03-01a0-709b-87c6-c94faead61ab",
  ];
  for (const cmd of byAgent) {
    const typed = cmd.split(" ")[0];
    assert.equal(suggestion(typed, cmd, hist, zsh), cmd.slice(typed.length), cmd);
  }
  // the card wins even when history has a longer command with the same start
  assert.equal(suggestion("claude", "claude --resume x", ["claude --resume y"], []), " --resume x");
});

test("saran editor: tanpa kartu sesi, riwayat aplikasi dulu lalu riwayat zsh", () => {
  assert.equal(suggestion("claude", null, ["claude -c"], ["claude --continue"]), " -c");
  // app session history is newest last, so the newest match comes first
  assert.equal(suggestion("cargo", undefined, ["cargo build", "cargo test"], ["cargo fmt"]), " test");
  assert.equal(suggestion("cargo", null, [], ["cargo fmt"]), " fmt");
  // an entry equal to the typed text is no suggestion; a card that does not match the text falls through
  assert.equal(suggestion("claude -c", "claude --resume x", ["claude -c"], []), "");
  assert.equal(suggestion("codex", "claude --resume x", ["codex resume --last"], []), " resume --last");
});

test("saran editor: baris banyak, awalan @, dan teks kosong tidak disarankan", () => {
  assert.equal(suggestion("", "claude --resume x", ["claude -c"], []), "");
  assert.equal(suggestion("@sh", "claude --resume x", ["@shell-1 halo"], []), "");
  assert.equal(suggestion("multi", "claude --resume x", ["multi\nline"], []), "");
  assert.equal(suggestion("x", "claude --resume x", ["x\ny"], []), "");
});

test("perintah lanjut dari daemon lolos", () => {
  for (const c of [
    "claude --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb",
    "claude-kantor --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb",
    "codex resume 01a19b1c-2cfb-7290-b33f-af39083bb39b",
    "codex resume --last",
    "opencode --continue",
    "opencode --session ses_4Ab-9",
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
    "opencode --session x;id",
    "codex resume --last\nrm -rf ~",
    "bash -c id",
    "",
    null,
    42,
  ])
    assert.equal(safeResume(c), null);
});
