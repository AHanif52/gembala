// @ts-nocheck -- the real mod API is exercised without calling a model.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

test("Command Code mod reports ordered status and never answers permission prompts", async () => {
  const dir = await mkdtemp(join(tmpdir(), "gembala-commandcode-"));
  try {
    const log = join(dir, "events.jsonl");
    const exe = join(dir, "gembala");
    await writeFile(exe, `#!${process.execPath}
import fs from 'node:fs';
let payload = '';
process.stdin.on('data', c => payload += c);
process.stdin.on('end', () => fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify({args: process.argv.slice(2), payload: JSON.parse(payload)}) + '\\n'));
`, { mode: 0o700 });
    const source = await readFile(new URL("../../../core/src/commandcode.mjs", import.meta.url), "utf8");
    const mod = join(dir, "gembala.mjs");
    await writeFile(mod, source.replace("__EXE__", JSON.stringify(exe)).replace("__HINT__", JSON.stringify("Gembala hint")));
    const hooks = {};
    const events = {};
    const { default: load } = await import(pathToFileURL(mod).href);
    load({ hooks: h => Object.assign(hooks, h), on: (name, f) => events[name] = f });
    assert.equal(hooks.appendSystemPrompt(), "Gembala hint");
    await hooks.onSessionStart();
    events.run_start({ sessionId: "session-123" });
    assert.equal(hooks.onPermissionRequest({ toolName: "shell_command" }), undefined);
    events.tool_running({});
    await hooks.onRunEnd({ result: { finalText: "Done" } });
    const lines = (await readFile(log, "utf8")).trim().split("\n").map(JSON.parse);
    assert.deepEqual(lines.map(l => l.args), ["start", "busy", "permission", "busy", "end"].map(event => ["hook", "commandcode", event, "-"]));
    assert.equal(lines[4].payload.session_id, "session-123");
    assert.equal(lines[4].payload.last_assistant_message, "Done");
    await hooks.onSessionStart();
    const last = JSON.parse((await readFile(log, "utf8")).trim().split("\n").at(-1));
    assert.equal(last.payload.session_id, undefined);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
