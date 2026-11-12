// Loaded only for this session; the user's Command Code settings stay untouched.
import { execFile } from "node:child_process";

export default function (cmd) {
  let session;
  let pending = Promise.resolve();
  const hook = (event, last) => {
    const payload = JSON.stringify({ session_id: session, last_assistant_message: last?.slice(0, 4000) });
    // Keep start/permission/end in order even when a turn finishes quickly.
    pending = pending.then(() => new Promise((resolve) => {
      const child = execFile(__EXE__, ["hook", "commandcode", event, "-"], { timeout: 2000 }, () => resolve());
      child.stdin?.on("error", () => {});
      child.stdin?.end(payload);
    }));
    return pending;
  };
  cmd.hooks({
    appendSystemPrompt: () => process.env.GEMBALA_COMMANDCODE_HINT || __HINT__,
    onSessionStart: () => { session = undefined; return hook("start"); },
    // Observe only: undefined leaves the permission decision with Command Code and its user.
    onPermissionRequest: () => { void hook("permission"); },
    onRunEnd: ({ result }) => hook("end", result.finalText),
  });
  cmd.on("run_start", (event) => { session = event.sessionId; void hook("busy"); });
  cmd.on("tool_running", () => { void hook("busy"); });
}
