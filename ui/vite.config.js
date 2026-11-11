import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
// @ts-expect-error type error without @types/node package
import process from "node:process";
// @ts-expect-error type error without @types/node package
import { execSync } from "node:child_process";
const host = process.env.TAURI_DEV_HOST;
// GESEKAN 11 Nov ("itu debug mode bukan?"): a debug or dev build names itself and its branch in the window; release = ""
const debugBranch = () => {
  try {
    return execSync("git rev-parse --abbrev-ref HEAD").toString().trim();
  } catch {
    return "?";
  }
};
const debugTag = process.env.TAURI_ENV_DEBUG === "true" ? `debug · ${debugBranch()}` : "";

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [sveltekit()],
  define: { __GEMBALA_DEBUG__: JSON.stringify(debugTag) },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || "127.0.0.1",
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
