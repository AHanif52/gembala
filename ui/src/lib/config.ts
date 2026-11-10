// Shape of `config.get` from the daemon (TECH section 5). Values are already validated by the daemon; broken = default.

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
  command_timeline: boolean; // FR-58: command block timeline in shell panes
  codex_status: boolean; // Codex footer drawn like Claude's status; off = the user's own Codex footer
  on_quit: "keep" | "stop";
  helper_idle_close: number; // FR-86: minutes a replied helper may sit quiet before it's closed; 0 = never
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
  policy?: string; // SEC-5: messages from other agents — auto | confirm | deny
  permission: string; // FR-64: default | auto
  account?: string; // FR-61: the profile's default account; empty = Utama
};

/// FR-61: `[[account]]` from config.toml
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

/// FR-78: add-ons a pane of `agent` in workspace `ws` gets, by name (same rules as `addon::args`); `w` = words in the UI language
export function activeAddons(a: Addons | undefined, agent: string, ws: string, w = { none: "none", only: "only these" }): string[] {
  const mcp = a?.mcp.find((m) => ws && m.workspace === ws);
  const mcpName = mcp ? [`MCP: ${mcp.servers.join(", ") || w.none}${mcp.strict && agent === "claude" ? ` (${w.only})` : ""}`] : [];
  if (agent === "claude") return [...(a?.plugins ?? []).filter((p) => p.scope === "all" || (p.scope === "workspaces" && p.workspaces.includes(ws))).map((p) => p.id), ...mcpName];
  return agent === "codex" ? mcpName : [];
}

export type Config = {
  values: {
    ui: Ui;
    limits: { max_panes: number };
    notify: { on_blocked: boolean; on_done: boolean; long_command_secs: number; sound: boolean };
    toolbar: { left: string[]; right: string[] };
    telegram?: { away_idle_mins: number; snippet: boolean; snippet_off: string[]; lines: number }; // FR-80
    update?: { check: boolean }; // FR-82
    harness?: { max_rounds: number; total_mins: number; fallback_on: string[]; plan_gate: boolean; token_saver: boolean; notify_gate: boolean; notify_blocked: boolean; notify_done: boolean }; // FR-74
    keys: Record<string, string>;
    profiles: Profile[];
    accounts?: Account[];
    addons?: Addons;
  };
  errors: ({ code: string; args?: Record<string, string> } | string)[]; // core/src/diag.rs; text = older daemon
  path: string;
};

// same as the defaults in core/src/config.rs; used until the daemon's first reply arrives
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
  codex_status: true,
  on_quit: "keep",
  helper_idle_close: 15,
};

// show shortcuts with Mac symbols (`Cmd+Shift+[` → ⌘⇧[); config keys may be lowercase (`cmd+e` → ⌘E)
const SYM: Record<string, string> = { Cmd: "⌘", Shift: "⇧", Alt: "⌥", Ctrl: "⌃", Enter: "↩", Left: "←", Right: "→", Up: "↑", Down: "↓" };
export const pretty = (k: string) =>
  k
    .split("+")
    .map((s) => s.charAt(0).toUpperCase() + s.slice(1))
    .map((s) => SYM[s] ?? s)
    .join("");
