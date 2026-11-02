// Bentuk `config.get` dari daemon (TECH bagian 5). Nilai sudah divalidasi daemon; yang rusak = bawaan.

export type Ui = {
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
};

export type Agent = {
  agent: string;
  name: string;
  installed: boolean;
  path: string | null;
  version: string | null;
  tested: boolean;
  install: string;
};

export type Config = {
  values: {
    ui: Ui;
    limits: { max_panes: number };
    notify: { on_blocked: boolean; on_done: boolean; long_command_secs: number; sound: boolean };
    toolbar: { left: string[]; right: string[] };
    keys: Record<string, string>;
    profiles: Profile[];
  };
  errors: string[];
  path: string;
};

// sama dengan bawaan di core/src/config.rs; dipakai sampai balasan pertama daemon datang
export const UI_DEFAULT: Ui = {
  theme: "system",
  font_family: "SF Mono",
  font_size: 13,
  line_height: 1.2,
  cursor: "bar",
  cursor_blink: true,
  option_as_meta: "left",
  scrollback_lines: 5000,
  screen_reader: false,
  input_editor: true,
  on_quit: "keep",
};

// tampilkan pintasan dengan simbol Mac
export const pretty = (k: string) =>
  k
    .replace("Cmd", "⌘")
    .replace("Shift", "⇧")
    .replace("Alt", "⌥")
    .replace("Ctrl", "⌃")
    .replace("Enter", "↩")
    .replace(/Left|Right|Up|Down/, (d) => ({ Left: "←", Right: "→", Up: "↑", Down: "↓" })[d]!)
    .replaceAll("+", "");
