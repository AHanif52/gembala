// Tauri sidecar (FR-98): the daemon binary copied to `src-tauri/binaries/gembala-<target-triple>`, the name Tauri looks
// for, plus the macOS helper app. Plain Node instead of `cp`/`sh` so the same script runs on macOS, Linux (and Windows
// later). Run from `ui/` by `npm run sidecar`.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ui = dirname(dirname(fileURLToPath(import.meta.url)));
const triple = execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(/^host: (.+)$/m)?.[1]?.trim();
if (!triple) throw new Error("rustc -vV: no host line");
const cygwin = process.platform === "win32";
const src = join(ui, "..", "target", "release", cygwin ? "gembala.exe" : "gembala");
const dest = join(ui, "src-tauri", "binaries", `gembala-${triple}${cygwin ? ".exe" : ""}`);
mkdirSync(dirname(dest), { recursive: true });
copyFileSync(src, dest);
// FR-20: the helper app is the macOS way of leaving the app's coalition; Linux spawns the daemon with `setsid`
if (process.platform === "darwin") execFileSync("sh", [join(ui, "src-tauri", "helper", "build.sh")], { cwd: ui, stdio: "inherit" });
