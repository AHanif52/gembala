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
};

export type Config = {
  values: { ui: Ui; limits: { max_panes: number }; keys: Record<string, string> };
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
};
