// Bentuk `config.get` dari daemon (TECH bagian 5). Nilai sudah divalidasi daemon; yang rusak = bawaan.

export type Ui = {
  language: "system" | "id" | "en";
  theme: "system" | "light" | "dark";
  font_family: string;
  font_size: number;
  line_height: number;
  cursor: "bar" | "block" | "underline";
  cursor_blink: boolean;
  option_as_meta: "none" | "left" | "right" | "both";
  scrollback_lines: number;
  screen_reader: boolean;
  input_editor: boolean;
  command_timeline: boolean; // FR-58: linimasa blok perintah di pane shell
  on_quit: "keep" | "stop";
};

export type Profile = {
  id: string;
  name: string;
  agent: string;
  command: string;
  args: string[];
  cwd: string;
  env: string[];
  secret_env: { name: string; ref: string }[];
  summarize: boolean;
  notify: boolean;
  policy?: string; // SEC-5: pesan dari agent lain — auto | confirm | deny
  permission: string; // FR-64: default | auto
  account?: string; // FR-61: akun bawaan profil; kosong = Utama
};

/// FR-61: `[[account]]` dari config.toml
export type Account = { id: string; agent: string; name: string; dir: string; share: string[]; model?: string; effort?: string; trust?: boolean };

export type Agent = {
  agent: string;
  name: string;
  installed: boolean;
  path: string | null;
  version: string | null;
  tested: boolean;
  install: string;
};

/// FR-78: `[addons.plugin.*]` and `[workspace.*.mcp]` that passed validation (core/src/addon.rs `to_json`)
export type Addons = {
  plugins: { id: string; path: string; scope: "off" | "all" | "workspaces"; workspaces: string[] }[];
  mcp: { workspace: string; file: string; strict: boolean; servers: string[] }[];
};

/// FR-78: add-ons a pane of `agent` in workspace `ws` gets, by name (same rules as `addon::args`)
export function activeAddons(a: Addons | undefined, agent: string, ws: string): string[] {
  const mcp = a?.mcp.find((m) => ws && m.workspace === ws);
  const mcpName = mcp ? [`MCP: ${mcp.servers.join(", ") || "kosong"}${mcp.strict && agent === "claude" ? " (cuma ini)" : ""}`] : [];
  if (agent === "claude") return [...(a?.plugins ?? []).filter((p) => p.scope === "all" || (p.scope === "workspaces" && p.workspaces.includes(ws))).map((p) => p.id), ...mcpName];
  return agent === "codex" ? mcpName : [];
}

export type Config = {
  values: {
    ui: Ui;
    limits: { max_panes: number };
    notify: { on_blocked: boolean; on_done: boolean; long_command_secs: number; sound: boolean };
    toolbar: { left: string[]; right: string[] };
    keys: Record<string, string>;
    profiles: Profile[];
    accounts?: Account[];
    addons?: Addons;
  };
  errors: string[];
  path: string;
};

// sama dengan bawaan di core/src/config.rs; dipakai sampai balasan pertama daemon datang
export const UI_DEFAULT: Ui = {
  language: "system",
  theme: "system",
  font_family: "Hack",
  font_size: 13,
  line_height: 1.2,
  cursor: "bar",
  cursor_blink: true,
  option_as_meta: "left",
  scrollback_lines: 5000,
  screen_reader: false,
  input_editor: true,
  command_timeline: true,
  on_quit: "keep",
};

// show shortcuts with Mac symbols (`Cmd+Shift+[` → ⌘⇧[); config keys may be lowercase (`cmd+e` → ⌘E)
const SYM: Record<string, string> = { Cmd: "⌘", Shift: "⇧", Alt: "⌥", Ctrl: "⌃", Enter: "↩", Left: "←", Right: "→", Up: "↑", Down: "↓" };
export const pretty = (k: string) =>
  k
    .split("+")
    .map((s) => s.charAt(0).toUpperCase() + s.slice(1))
    .map((s) => SYM[s] ?? s)
    .join("");
