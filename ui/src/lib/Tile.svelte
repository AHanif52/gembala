<script lang="ts">
  // One pane = one xterm attached to the daemon (FR-1, FR-2), plus FR-34 to 36, 41.
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { Terminal, type IMarker, type ITheme } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { SearchAddon } from "@xterm/addon-search";
  import { WebLinksAddon } from "@xterm/addon-web-links";
  import { Unicode11Addon } from "@xterm/addon-unicode11";
  import "@xterm/xterm/css/xterm.css";
  import { call, b64, unb64 } from "./daemon";
  import { errText, i18n, t } from "./i18n/state.svelte";
  import { safeResume } from "./resume";
  import { encodeKey, onCsi } from "./kitty";
  import { codexStatus, type CodexStatus } from "./codex-status";
  import type { Ui } from "./config";

  let {
    pane,
    focused,
    ui,
    zoom,
    onexit,
    onlink,
    onpasted,
    online,
    onkey,
    onprompt,
    ontype,
    onfocusin,
    onused,
    onblock,
    ondone = () => {},
    codexPane = false,
  }: {
    pane: number;
    focused: boolean;
    ui: Ui;
    zoom: number;
    onexit: (pane: number) => void;
    onlink: (url: string) => void;
    onpasted: (path: string) => void;
    online: (line: string) => void; // FR-57: text typed since the last Enter
    onkey: (key: string) => boolean; // FR-57: true = the key is used by the @ list, not passed to the terminal
    onprompt: (at: boolean) => void; // FR-59: shell waiting at the prompt (OSC 133 marker B until C)
    ontype: (text: string) => boolean; // FR-59: true = letters taken by the input editor, not passed to the shell
    onfocusin: () => void; // FR-59: the terminal has keyboard focus (the page moves it to the editor when shown)
    onused: () => void; // FR-59: the screen already had text at attach (pane isn't new)
    onblock: (kind: "cmd" | "out" | "reuse" | "copy" | "run" | "output", text: string) => void; // FR-58: block action buttons; FR-56: timeline
    ondone?: (cmd: string, code: number) => void; // FR-99 P1: a shell command finished (OSC 133 D) with its exit code
    codexPane?: boolean; // Codex runs in this pane: its footer is drawn as the status overlay
  } = $props();

  // ponytail: two fixed palettes; a 16-color palette from config comes later if asked for
  // Warp's default Dark theme palette; without a palette xterm uses its built-in ANSI colors (dark blue barely readable)
  const DARK: ITheme = {
    background: "#000000", foreground: "#ffffff", cursor: "#ffffff", selectionBackground: "#404040",
    black: "#616161", red: "#ff8272", green: "#b4fa72", yellow: "#fefdc2", blue: "#a5d5fe", magenta: "#ff8ffd", cyan: "#d0d1fe", white: "#f1f1f1",
    brightBlack: "#8e8e8e", brightRed: "#ffc4bd", brightGreen: "#d6fcb9", brightYellow: "#fefdd5", brightBlue: "#c1e3fe", brightMagenta: "#ffb1fe", brightCyan: "#e5e6fe", brightWhite: "#feffff",
  };
  // ANSI palette for light backgrounds (GitHub Light): xterm's built-in colors are too bright and unreadable on light;
  // background/text = Latte palette (FR-34), same as the light theme's --bg/--fg
  const LIGHT: ITheme = {
    background: "#eff1f5", foreground: "#4c4f69", cursor: "#4c4f69", selectionBackground: "#ccd0da",
    black: "#24292f", red: "#cf222e", green: "#116329", yellow: "#7d4e00", blue: "#0969da", magenta: "#8250df", cyan: "#1b7c83", white: "#6e7781",
    brightBlack: "#57606a", brightRed: "#a40e26", brightGreen: "#1a7f37", brightYellow: "#633c01", brightBlue: "#218bff", brightMagenta: "#a475f9", brightCyan: "#3192aa", brightWhite: "#8c959f",
  };
  const MATCH = {
    matchBackground: "#854d0e",
    activeMatchBackground: "#ca8a04",
    matchOverviewRuler: "#854d0e",
    activeMatchColorOverviewRuler: "#ca8a04",
  };

  let el: HTMLDivElement;
  let term: Terminal;
  let fit: FitAddon;
  let search: SearchAddon;
  let finding = $state(false);
  let query = $state("");
  let found = $state("");
  let input = $state<HTMLInputElement>();
  const FIND_H = 32; // pen 04: find bar height; the terminal moves down below it
  const enc = new TextEncoder();
  let codex = $state<CodexStatus | null>(null);
  $effect(() => {
    void [codexPane, ui.codex_status]; // agent started/exited or the switch flipped: no new output needed to update
    if (ready) updateCodexStatus();
  });
  const meters = (c: CodexStatus) =>
    ([["tile.codex.ctx", c.context], ["tile.codex.5h", c.fiveHour], ["tile.codex.week", c.weekly]] as const).filter((m): m is [typeof m[0], number] => m[1] !== undefined);
  let codexRow = $state(-1);
  let codexKey = "";

  $effect(() => {
    if (focused && ready && !finding) term.focus();
  });

  const light = matchMedia("(prefers-color-scheme: light)");
  let systemLight = $state(light.matches);
  const isLight = $derived(ui.theme === "light" || (ui.theme === "system" && systemLight));

  // FR-34: [ui] from config applies right away without a restart; ⌘=/⌘- zoom on top of the config size
  let ready = $state(false); // term isn't state (big xterm object); the effects below wait for it
  $effect(() => {
    if (!ready) return;
    const o = term.options;
    o.fontFamily = `'${ui.font_family.replace(/['"\\]/g, "")}', 'SF Mono', Menlo, 'Symbols Nerd Font Mono', monospace`;
    o.fontSize = Math.max(6, ui.font_size + zoom);
    o.lineHeight = ui.line_height;
    o.cursorStyle = ui.cursor;
    o.cursorBlink = ui.cursor_blink;
    o.cursorInactiveStyle = "none"; // like Warp: an unfocused terminal (or while the input editor is in use) has no cursor
    o.scrollback = ui.scrollback_lines;
    o.screenReaderMode = ui.screen_reader;
    o.macOptionIsMeta = ui.option_as_meta === "left" || ui.option_as_meta === "both";
    o.theme = isLight ? LIGHT : DARK;
    // agents paint truecolor picked for dark terminals (pale blue/orange): on white, xterm darkens them to WCAG AA
    o.minimumContrastRatio = isLight ? 4.5 : 1;
    if (el.offsetParent) fit.fit();
  });

  /// FR-36: ⌘F
  export async function openFind() {
    finding = true;
    await tick();
    input?.focus();
    input?.select();
  }

  function find(back = false) {
    if (!query) return void search.clearDecorations();
    const opts = { decorations: MATCH, incremental: !back };
    if (back) search.findPrevious(query, opts);
    else search.findNext(query, opts);
  }

  function closeFind() {
    finding = false;
    search.clearDecorations();
    term.focus();
  }

  function onFindKey(e: KeyboardEvent) {
    if (e.key === "Enter") find(e.shiftKey);
    if (e.key === "Escape") closeFind();
  }

  /// FR-57: the typed line, only letters typed directly; special keys (arrows, Enter, pastes with ESC) clear it.
  /// ponytail: doesn't know the TUI's real prompt content; enough to spot `@` at the start of a new line
  let line = "";
  function track(d: string) {
    if (d === "\x7f") line = line.slice(0, -1);
    else if (/[\x00-\x1f]/.test(d)) line = "";
    else line += d;
    online(line);
  }
  export function resetLine() {
    line = "";
    online(line);
  }
  /// FR-57: the page changed the prompt content (e.g. `@@q` replaced by a pane name), the tracked line follows
  export function setLine(f: (line: string) => string) {
    line = f(line);
    online(line);
  }
  export const focus = () => term.focus();
  /// The page re-attaches every pane of a daemon it reconnected to (criterion 17); filled in `onMount`
  let reattachNow: (() => void) | null = null;
  export function reattach() {
    reattachNow?.();
  }

  /// FR-58: command block timeline from the shell's OSC 133 markers (A prompt, C command runs + base64 text, D done + exit code).
  /// Only blocks from the stream since attach; the attach snapshot holds no markers.
  type Block = { a: IMarker; c?: IMarker; d?: IMarker; cmd: string; dir?: string; code?: number; t0?: number; ms?: number; live?: { dispose(): void } };
  let blocks: Block[] = [];
  let kitty: number[] = []; // stack of kitty keyboard protocol flags the app in this pane asked for
  /// FR-58: Settings > Appearance > "Command timeline". Off = shell panes become plain terminals (no line, dot,
  /// block head, Rerun/Edit/Copy buttons). OSC 133 markers are still tracked, so ⌘↑/⌘↓ block jumps still work.
  const timeline = $derived(ui.command_timeline !== false);
  // the switch applies right away: shell block decorations already drawn disappear/return without reloading the pane
  $effect(() => {
    if (!ready) return;
    const on = timeline;
    void i18n.lang; // decorations hold text: redraw on a language switch
    blocks.forEach((b) => (b.live?.dispose(), (b.live = undefined)));
    hover = null;
    sticky = null;
    if (on) {
      blocks.forEach((b) => b.cmd && (b.live = decorate(b)));
      updateSticky(); // the sticky block head comes back right away without waiting for a scroll
    }
  });
  // session block heads and "Previous session" lines are drawn in the UI language too
  $effect(() => {
    void i18n.lang;
    if (ready) decorateSummary();
  });
  function onMark(data: string) {
    const [k, arg = ""] = data.split(";", 2);
    const last = blocks.at(-1);
    if (k === "B") onprompt(true);
    if (k === "C") onprompt(false);
    if (k === "A") {
      kitty = []; // shell back at the prompt: keyboard flags of an app that crashed don't carry over to the shell
      const a = term.registerMarker(0);
      if (a) blocks.push({ a, cmd: "", dir: data.slice(2) }); // `A;<folder>`: folder boleh berisi `;`
      if (blocks.length > 1000) blocks.shift()?.a.dispose(); // ponytail: last 1000 blocks, the rest goes with the scrollback
    } else if (k === "C") {
      // a prompt printed before the UI attached has no A marker (snapshot): the block starts at the command line,
      // or the blank line above it (editor prompt) so it still gets a dot and a head
      let b = last && !last.c ? last : undefined;
      if (!b) {
        const buf = term.buffer.active;
        const a = term.registerMarker(buf.getLine(buf.baseY + buf.cursorY - 2)?.translateToString(true).trim() ? -1 : -2);
        if (!a) return true;
        blocks.push((b = { a, cmd: "" }));
      }
      b.c = term.registerMarker(0);
      b.t0 = Date.now();
      try {
        b.cmd = new TextDecoder().decode(unb64(arg));
      } catch {}
      if (b.cmd && timeline) b.live = decorate(b); // blue dot pulsing while running
    } else if (k === "D" && last?.c && !last.d) {
      last.d = term.registerMarker(0);
      last.code = Number(arg) || 0;
      if (last.t0) last.ms = Date.now() - last.t0;
      last.live?.dispose();
      last.live = timeline ? decorate(last) : undefined; // finished decorations are tracked too so they can be removed when the switch is turned off
      ondone(last.cmd, last.code);
    }
    return true; // the marker draws nothing
  }
  const dur = (ms: number) => t("tile.secs", { n: ms < 10_000 ? (ms / 1000).toLocaleString(i18n.lang, { minimumFractionDigits: 1, maximumFractionDigits: 1 }) : Math.round(ms / 1000) });
  /// session block heads are written by the daemon as `1 mnt 15 dtk` (core/src/history.rs `dur`): units in the UI language
  const durText = (s: string) => s.replace(/(\d+) (dtk|mnt|jam)/g, (_, n, u) => t(u === "dtk" ? "tile.secs" : u === "mnt" ? "tile.mins" : "tile.hours", { n }));
  /// pen: "~/…/gembala/app" (last two folders)
  const short = (p: string) => {
    const a = p.replace(/^\/Users\/[^/]+/, "~").split("/");
    return a.length > 3 ? `${a[0]}/…/${a.slice(-2).join("/")}` : a.join("/");
  };
  /// shell block timeline (Gembala.pen screen 02): line + dot in the left gutter outside the grid. The editor prompt starts with a blank
  /// line (zsh hook): head `~/…/app · 0.4 s · exit 1` on that line; user's own prompt = duration to the right of the command
  function decorate(b: Block) {
    const height = Math.max(1, (b.d?.line ?? b.a.line) - b.a.line);
    const hd = !term.buffer.active.getLine(b.a.line)?.translateToString(true).trim();
    const d = term.registerDecoration({ marker: b.a, width: term.cols, height, layer: "top" });
    const info = [hd && b.dir ? short(b.dir) : "", !b.d ? (hd ? t("tile.running") : "") : b.ms !== undefined ? dur(b.ms) : ""].filter(Boolean).join(" · ");
    d?.onRender((el) => {
      if (!el.dataset.tl) {
        el.dataset.tl = "1";
        el.className += " tl sh" + (b.code ? " fail" : "") + (b.d ? "" : " run") + (hd ? " hd" : "");
        el.innerHTML = `<i class="tl-dot"></i><span class="tl-meta"></span>`;
        const m = el.querySelector(".tl-meta") as HTMLElement;
        m.textContent = info; // folder from the shell: text, not HTML
        if (b.code) m.append(Object.assign(document.createElement("b"), { textContent: ` · ${t("tile.exit", { code: b.code })}` }));
      }
      // row height can be 0 on first render: position is recomputed every render
      el.style.setProperty("--rh", `${rowHeight()}px`);
    });
    return d;
  }
  /// FR-56/62: agent session summary blocks and "Sesi sebelumnya" history are written by the daemon as text (no markers,
  /// also in the attach snapshot): block head = dim line `<folder> (<duration>)`, so its line is attached from the buffer content
  let summary: { dispose(): void }[] = [];
  function decorateSummary() {
    summary.splice(0).forEach((d) => d.dispose());
    const buf = term.buffer.active;
    if (buf.type === "alternate") return;
    // timeline (mockup-blok-sesi E): "   [icon ]<folder> · <duration>[ · keluar n]"; old format "<folder> (<duration>)" = line only
    const head = /^ {0,3}(?:([✳π◎◇]) )?(\S.* · \d+ (?:dtk|mnt|jam)(?: \d+ (?:dtk|mnt))?)( · keluar (-?\d+))?$/;
    const old = /^\S.* \(\d+ (dtk|mnt|jam)[^)]*\)( · keluar (-?\d+))?$/;
    const text = (y: number) => {
      // wrapped lines (long commands) are joined
      let t = buf.getLine(y)?.translateToString(true) ?? "";
      for (let k = y + 1; buf.getLine(k)?.isWrapped; k++) t += buf.getLine(k)!.translateToString(true);
      return t;
    };
    const heads: { y: number; fail: boolean; tl: boolean; icon?: string; info?: string; code?: string }[] = [];
    const stops: number[] = []; // "Sesi sebelumnya": the block stops here
    for (let y = 0; y < buf.length; y++) {
      // trimRight only drops empty cells; spaces the log really wrote (a head padded to an old width, 9 Nov) stay and broke `$`
      const t = (buf.getLine(y)?.translateToString(true) ?? "").trimEnd();
      if (/^\s*(── )?Sesi sebelumnya/.test(t)) stops.push(y);
      const n = head.exec(t);
      if (n) {
        heads.push({ y, tl: true, fail: !!n[4] && n[4] !== "0", icon: n[1], info: n[2], code: n[4] });
        continue;
      }
      const m = old.exec(t);
      if (m) heads.push({ y, tl: false, fail: !!m[3] && m[3] !== "0" });
    }
    const cursor = buf.baseY + buf.cursorY;
    // "Sesi sebelumnya · <date>" (pen screen 02): small sans text + a line to the right edge, covering its terminal row
    for (const y of stops) {
      const marker = term.registerMarker(y - cursor);
      if (!marker) continue;
      summary.push(marker);
      const label = (buf.getLine(y)?.translateToString(true) ?? "").replace(/[─\s]+$/, "").replace(/^[\s─]+/, "").replace(/^Sesi sebelumnya/, t("tile.prevSession"));
      term.registerDecoration({ marker, width: term.cols, height: 1, layer: "top" })?.onRender((el) => {
        if (!el.dataset.tl) {
          el.dataset.tl = "1";
          el.classList.add("tl-sep");
          el.append(Object.assign(document.createElement("span"), { textContent: label }), document.createElement("i"));
        }
        el.style.setProperty("--rh", `${rowHeight()}px`);
      });
    }
    heads.forEach((h, i) => {
      const end = Math.min(heads[i + 1]?.y ?? buf.length, stops.find((s) => s > h.y) ?? buf.length);
      const marker = term.registerMarker(h.y - cursor);
      if (!marker) return;
      summary.push(marker);
      if (!h.tl) {
        const d = term.registerDecoration({ marker, width: term.cols, height: Math.max(1, end - h.y), layer: "bottom" });
        d?.onRender((el) => (el.classList.add("blk"), el.classList.toggle("fail", h.fail)));
        return;
      }
      // command = bold line after the head; resume command = dim line after it (old format: "↳ …")
      let cmd = "";
      let resume = "";
      let ry = -1;
      for (let y = h.y + 1; y < end; y++) {
        if (buf.getLine(y)?.isWrapped) continue;
        const t = text(y).trim();
        if (!t) continue;
        // a long command hard-wrapped (zle, log replay) = the second row is still the command: take the valid continuation row
        if (!cmd) cmd = t;
        else if (!resume && safeResume(t.replace(/^↳ /, ""))) (resume = t.replace(/^↳ /, "")), (ry = y);
      }
      // button row (pen: below the resume command) = the first blank row after it
      let by = ry + 1;
      while (ry >= 0 && buf.getLine(by)?.isWrapped) by++;
      // the button row needs 2 rows before the next block / "Sesi sebelumnya" line; fewer = buttons right of the head
      // (old logs and the daemon may leave only 1 row, so don't rely on the spacing)
      const roomy = end - by >= 2;
      const agent = !!h.icon;
      const [path = "", ...rest] = (h.info ?? "").split(" · ");
      if (agent && resume) {
        // session block card (pen screen 02): slightly lighter background from the head to the button row
        const ch = roomy ? by - h.y + 2 : Math.max(1, Math.min(by, end) - h.y);
        term.registerDecoration({ marker, width: term.cols, height: ch, layer: "bottom" })?.onRender((el) => el.classList.add("tl-card"));
      }
      const d = term.registerDecoration({ marker, width: term.cols, height: Math.max(1, end - h.y), layer: "top" });
      d?.onRender((el) => {
        // row height can be 0 on first render: position is recomputed every render (CSS uses --rh)
        el.style.setProperty("--rh", `${rowHeight()}px`);
        if (el.dataset.tl) return;
        el.dataset.tl = "1";
        el.classList.add("tl");
        el.classList.toggle("fail", h.fail);
        el.classList.toggle("agent", agent);
        el.classList.toggle("card", agent && !!resume);
        // pen screen 02: agent session block = agent icon in the timeline column; command block = dot
        el.innerHTML = `<i class="tl-dot"></i><div class="tl-hd"><span class="tl-info"></span><b class="tl-x"></b></div><span class="tl-acts"></span>`;
        const dot = el.querySelector(".tl-dot") as HTMLElement;
        if (agent) (dot.textContent = h.icon!), (dot.dataset.a = h.icon!);
        (el.querySelector(".tl-info") as HTMLElement).textContent = [short(path), ...rest.map(durText)].join(" · ");
        const x = el.querySelector(".tl-x") as HTMLElement;
        x.textContent = h.code && h.code !== "0" ? `· ${t("tile.exit", { code: h.code })}` : agent ? `· ${t("tile.exitOk")}` : "";
        x.classList.toggle("ok", !(h.code && h.code !== "0"));
        const acts = el.querySelector(".tl-acts") as HTMLElement;
        const btn = (label: string, cls: string, run: () => void) => {
          const b = Object.assign(document.createElement("button"), { textContent: label, className: cls });
          b.addEventListener("mousedown", (e) => e.preventDefault());
          b.addEventListener("click", (e) => (e.stopPropagation(), run()));
          acts.append(b);
        };
        if (agent && resume) {
          // pen: Resume, View full output, Copy command on their own row below the block (if it fits);
          // doesn't fit: compact buttons on the right, middle of the block (mockup variant E), with a background so they don't cover text
          if (roomy) acts.classList.add("row"), acts.style.setProperty("--k", String(by - h.y));
          else acts.classList.add("side"), acts.style.setProperty("--n", String(Math.max(1, Math.min(by, end) - h.y)));
          btn(t("tile.resume"), "pri", () => onblock("run", resume));
          btn(roomy ? t("tile.fullOutput") : t("tile.output"), "", () => onblock("output", ""));
          btn(roomy ? t("tile.copyCmd") : t("tile.copy"), "", () => onblock("copy", resume));
        } else if (h.fail && cmd) {
          btn(t("tile.copy"), "ghost", () => onblock("copy", cmd));
          btn(t("tile.rerun"), "pri", () => onblock("run", cmd));
        }
      });
    });
  }
  const outputOf = (b: Block) => {
    if (!b.c || !b.d) return "";
    const buf = term.buffer.active;
    const rows: string[] = [];
    for (let y = b.c.line; y < Math.max(b.c.line + 1, b.d.line); y++) rows.push(buf.getLine(y)?.translateToString(true) ?? "");
    return rows.join("\n").replace(/\n+$/, "");
  };
  /// block at screen row `clientY` (right click), or null; timeline off = no block offered
  export function blockAt(clientY: number): { cmd: string; output: string; code?: number } | null {
    if (!timeline) return null;
    const rowsEl = el.querySelector(".xterm-rows");
    if (!rowsEl) return null;
    const r = rowsEl.getBoundingClientRect();
    const y = Math.floor(((clientY - r.top) / r.height) * term.rows) + term.buffer.active.viewportY;
    const b = blocks.findLast((b) => b.a.line <= y && b.d && y < Math.max(b.d.line, b.a.line + 1));
    return b ? { cmd: b.cmd, output: outputOf(b), code: b.code } : null;
  }
  // FR-58: block action buttons on hover, and a sticky block head when its command scrolls off screen
  let hover = $state<{ top: number; b: Block } | null>(null);
  let sticky = $state<Block | null>(null);
  const rowHeight = () => (el.querySelector(".xterm-rows")?.getBoundingClientRect().height ?? 0) / term.rows;
  const blockOn = (y: number) => blocks.findLast((b) => b.cmd && b.a.line <= y && (!b.d || y < Math.max(b.d.line, b.a.line + 1)));
  function onHover(e: MouseEvent) {
    const rows = el.querySelector(".xterm-rows")?.getBoundingClientRect();
    if (!timeline || !rows || term.buffer.active.type === "alternate") return void (hover = null);
    const vy = term.buffer.active.viewportY;
    const b = blockOn(Math.floor((e.clientY - rows.top) / rowHeight()) + vy);
    hover = b ? { top: (b.a.line - vy) * rowHeight() + lift, b } : null;
  }
  // FR-58: content stacks from the bottom up (like Warp): while the buffer isn't full, the xterm screen is shifted down by
  // the blank rows below the cursor; once full (there's scrollback) or a full-screen TUI, it isn't shifted
  let lift = $state(0);
  function anchor() {
    const b = term.buffer.active;
    if (b.type !== "normal" || b.baseY !== 0) return void (lift = 0);
    // the lowest row with text, not only the cursor: omp draws its `/` menu below the input line (9 Nov: hidden under the bar)
    let last = b.cursorY;
    for (let y = term.rows - 1; y > last; y--)
      if (b.getLine(y)?.translateToString(true).trim()) {
        last = y;
        break;
      }
    lift = Math.max(0, term.rows - 1 - last) * rowHeight();
  }
  function updateSticky() {
    const top = term.buffer.active.viewportY;
    sticky = !timeline || term.buffer.active.type === "alternate" ? null : (blocks.findLast((b) => b.cmd && b.a.line < top && (!b.d || b.d.line > top)) ?? null);
  }

  /// Codex panes only (a shell printing a footer-like line keeps its text), and only the lowest 3 rows with text (same
  /// rows as anchor(): a screen that isn't full yet is lifted, not filled): the footer sits under Codex's input box
  function updateCodexStatus() {
    const b = term.buffer.active;
    let status: CodexStatus | null = null;
    let row = -1;
    const top = b.viewportY;
    for (let y = top + term.rows - 1, seen = 0; codexPane && ui.codex_status !== false && y >= top && seen < 3; y--) {
      const text = b.getLine(y)?.translateToString(true) ?? "";
      if (!text.trim()) continue;
      seen++;
      if ((status = codexStatus(text))) {
        row = y - top;
        break;
      }
    }
    const key = status ? `${row}:${JSON.stringify(status)}` : "";
    if (key !== codexKey) [codexKey, codex, codexRow] = [key, status, row];
  }

  /// FR-58: ⌘↑/⌘↓ jump to the start of the block before/after the top screen row
  export function jump(dir: -1 | 1) {
    const top = term.buffer.active.viewportY;
    const b = dir < 0 ? blocks.findLast((b) => b.a.line < top) : blocks.find((b) => b.a.line > top);
    if (b) term.scrollToLine(b.a.line);
    else if (dir > 0) term.scrollToBottom();
  }

  /// FR-37: menu klik kanan
  export const selection = () => term.getSelection();
  export const paste = (text: string) => term.paste(text); // bracketed paste when the TUI asks for it

  /// FR-36: clear screen and scrollback. An idle shell is also asked to redraw its prompt (Ctrl+L);
  /// when a process is running (agent), only the scrollback is dropped: a TUI's screen is redrawn piecewise,
  /// so a screen cleared from outside leaves the agent's view broken.
  export async function clearScreen() {
    const info = await call("pane.info", { pane });
    if (info.busy) return void term.write("\x1b[3J");
    term.clear();
    call("pane.write", { pane, data: b64(new Uint8Array([0x0c])) });
  }

  // xterm's default DOM renderer: measured lighter on RAM and flat per pane compared to WebGL (TECH M1)
  onMount(() => {
    term = new Terminal({
      allowProposedApi: true, // search result decorations and Unicode 11
      // SEC-14: OSC 8 links aren't opened directly; the real URL is shown first
      linkHandler: { activate: (_e, uri) => onlink(uri) },
    });
    fit = new FitAddon();
    search = new SearchAddon();
    term.loadAddon(fit);
    term.loadAddon(search);
    term.loadAddon(new WebLinksAddon((_e, uri) => onlink(uri)));
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11"; // FR-41: emoji and CJK width
    term.open(el);
    term.parser.registerOscHandler(133, onMark);
    // kitty keyboard protocol (flag 1): query answers go to the pane like xterm's own DA answer
    for (const prefix of ["?", ">", "<", "="])
      term.parser.registerCsiHandler({ prefix, final: "u" }, (params) => {
        const reply = onCsi(kitty, prefix, params.map((p) => (Array.isArray(p) ? p[0] : p)));
        if (reply) call("pane.write", { pane, data: b64(enc.encode(reply)) });
        return true;
      });
    term.textarea?.addEventListener("focus", () => onfocusin());
    // listener on the parent: block buttons sit outside the xterm element, the cursor may move onto them
    const host = el.parentElement!;
    const leave = () => (hover = null);
    host.addEventListener("mousemove", onHover);
    host.addEventListener("mouseleave", leave);
    term.onScroll(updateSticky);
    term.onRender(() => (anchor(), updateCodexStatus())); // every write, resize, and alternate screen switch
    ready = true;
    fit.fit();

    const onTheme = () => (systemLight = light.matches);
    light.addEventListener("change", onTheme);
    search.onDidChangeResults(({ resultIndex, resultCount }) => {
      found = resultCount ? t("tile.findCount", { i: resultIndex + 1, n: resultCount }) : query ? t("tile.findNone") : "";
    });

    // FR-35: left and right Option are set separately (default: left Meta, right character, e.g. ⌥⇧2 = €).
    // xterm only has one switch, so it's flipped right when an Option key is pressed.
    const onAlt = (e: KeyboardEvent) => {
      const m = ui.option_as_meta;
      if (e.code === "AltLeft") term.options.macOptionIsMeta = m === "left" || m === "both";
      if (e.code === "AltRight") term.options.macOptionIsMeta = m === "right" || m === "both";
    };
    el.addEventListener("keydown", onAlt, true);

    // FR-54: ⌘V with an image = save to Gembala's temp folder (SEC-34), paste its quoted path.
    // Plain text isn't touched: xterm's paste handler takes it (bracketed paste).
    const EXT: Record<string, string> = { "image/png": "png", "image/jpeg": "jpg", "image/gif": "gif", "image/webp": "webp" };
    const onPaste = async (e: ClipboardEvent) => {
      const item = [...(e.clipboardData?.items ?? [])].find((i) => i.kind === "file" && EXT[i.type]);
      const file = item?.getAsFile();
      if (!item || !file) return;
      e.preventDefault();
      e.stopPropagation();
      try {
        const path = await invoke<string>("save_paste", { data: b64(new Uint8Array(await file.arrayBuffer())), ext: EXT[item.type] });
        onpasted(path);
        const quoted = `'${path.replaceAll("'", "'\\''")}' `;
        call("pane.write", { pane, data: b64(enc.encode(`\x1b[200~${quoted}\x1b[201~`)) });
      } catch (err) {
        term.write(`\r\n\x1b[31m${t("tile.pasteFailed", { why: errText(err) })}\x1b[0m\r\n`);
      }
    };
    el.addEventListener("paste", onPaste, true);

    let attached = false;
    let sized = false; // the attach snapshot is already written at the daemon's size and fitted to the pane
    // a new daemon sends the pane.attached marker after the snapshot: earlier events are already in the snapshot (dropped),
    // later ones aren't. An old daemon without the marker (reply without `sync`) = every event is used as before
    let synced = false;
    const early: any[] = [];
    // resize applies right away, writes are queued: each chunk is awaited first
    const put = (d: Uint8Array) => new Promise<void>((r) => term.write(d, r));
    // FR-2: raw history is written at the size it had when it came out (width decides wrapping and cursor moves), then
    // xterm itself rewraps on resize; the result matches a tab that was never detached. Used by attach
    // and pane.replace (FR-56). `sized` is off meanwhile: a resize to the old size must not reach the PTY
    const replay = async (a: any) => {
      sized = false;
      const segs: { rows: number; cols: number; data: string }[] = a.segments ?? [];
      const raw = segs.map((s) => unb64(s.data));
      const snap = unb64(a.snapshot ?? "");
      for (let i = 0; i < segs.length; i++) {
        term.resize(segs[i].cols, segs[i].rows);
        await put(raw[i]);
      }
      // TECH 19 #10: the tail is written at the daemon grid size (e.g. 80 columns before the UI attached), then fitted to the pane;
      // xterm rewraps long rows, while a daemon grid (vt100) shrunk first would cut them
      if (a.cols && a.rows) term.resize(a.cols, a.rows);
      await put(snap);
      sized = true;
      if (el.offsetParent) fit.fit();
      call("pane.resize", { pane, rows: term.rows, cols: term.cols }).catch(() => {});
      decorateSummary();
      return [...raw, snap];
    };
    const onEvent = (e: any) => {
      // history being rewritten (pane.replace): later events wait
      if (!attached) return void early.push(e);
      if (e.event === "pane.output") term.write(unb64(e.params.data));
      // FR-56: agent exits = screen replaced by the history before the agent + the daemon's summary block
      if (e.event === "pane.replace") {
        // old shell blocks (including the agent command that just exited) no longer apply: without this their dots and durations
        // pile up above the summary block
        blocks.splice(0).forEach((b) => (b.live?.dispose(), b.a.dispose(), b.c?.dispose(), b.d?.dispose()));
        term.reset();
        // an old daemon (before the ring) sends one `data` chunk
        if (!e.params.segments) return void term.write(unb64(e.params.data), decorateSummary);
        attached = false;
        replay(e.params).then(() => ((attached = true), early.splice(0).forEach(onEvent)));
      }
      // the daemon stopped feeding this client output for this pane (it fell behind): re-attach rebuilds the screen
      if (e.event === "pane.lag") void doAttach();
      if (e.event === "pane.exit") onexit(pane);
    };
    const unlisten = listen<any>("daemon", ({ payload }) => {
      if (payload.params?.pane !== pane) return;
      if (payload.event === "pane.attached") return void ((synced = true), early.splice(0));
      if (attached && synced) onEvent(payload);
      else early.push(payload);
    });

    // Attach, and attach again: a reconnected UI channel and a `pane.lag` (the daemon stopped feeding this client, see
    // `send_all`) both rebuild the screen from a fresh snapshot, so the terminal is reset first — output written before
    // that snapshot is already in it.
    let attaching = false;
    const doAttach = async () => {
      if (attaching) return; // a lag noticed during an attach is covered by it
      attaching = true;
      attached = false;
      synced = false;
      early.splice(0);
      term.reset();
      try {
        await unlisten; // listener ready before attach: the marker must not be missed
        // the attach reply and events take different paths in Tauri: events arriving first are held
        const a = await call("pane.attach", { pane });
        if (!a.sync) synced = true;
        // event live tertahan sampai `attached`
        const written = await replay(a);
        if (a.prompt) onprompt(true); // FR-59: a B marker before attach isn't in the snapshot
        const text = written.map((d) => new TextDecoder().decode(d)).join("");
        if (/\S/.test(text.replace(/\x1b\[[0-9;?]*[ -\/]*[@-~]/g, ""))) onused();
        attached = true;
        if (synced) early.splice(0).forEach(onEvent); // not yet: a marker arriving later drops what's held
      } catch (e) {
        sized = true;
        // the pane left while this screen was away (a `pane.lag` or a reconnect whose pane.exit was missed): drop it
        if (String(e).includes("no_such_pane")) return onexit(pane);
        term.write(`\x1b[31m${t("tile.attachFailed", { pane, why: errText(e) })}\x1b[0m\r\n`);
      } finally {
        attaching = false;
      }
    };
    reattachNow = () => void doAttach();
    void doAttach();

    // Shift+Enter = ESC CR (same as Claude Code's /terminal-setup in iTerm2): newline in agent TUIs
    term.attachCustomKeyEventHandler((e) => {
      // FR-57: daftar @ terbuka = Tab/panah/Esc milik daftar. Pola dicek dari
      // baris yang dilacak di sini (sinkron): state Svelte (mention) update-nya
      // async, jadi Tab cepat sesudah "@@" tidak boleh jatuh ke picker milik
      // agent (mis. daftar file/agent omp) sebelum daftar pane sempat kebuka.
      if (e.type === "keydown" && ["Tab", "Enter", "ArrowUp", "ArrowDown", "Escape"].includes(e.key) && !e.metaKey && !e.shiftKey) {
        if (onkey(e.key)) {
          e.preventDefault(); // WebKit's default Tab moves focus out of the terminal; agents hide the cursor when focus is lost
          return false;
        }
        // Race window: "@@" typed but the Svelte mention state hasn't synced yet;
        // Tab/arrows would fall into the agent's own @ picker (files/agents) instead
        // of the pane list. Swallow only navigation keys: Enter (send) and Esc
        // (agent's own cancel) must always pass through, or an empty pane list
        // would strand the prompt with no way to send it.
        if (["Tab", "ArrowUp", "ArrowDown"].includes(e.key) && /(?:^|\s)@@([\w.-]*)$/.test(line)) {
          e.preventDefault();
          return false;
        }
      }
      const ku = (kitty.at(-1) ?? 0) & 1 ? encodeKey(e, !!term.options.macOptionIsMeta) : null;
      if (ku) {
        e.preventDefault(); // Ctrl+Enter: macOS WebKit opens the context menu even after the code was sent
        if (e.type === "keydown") (track(ku), call("pane.write", { pane, data: b64(enc.encode(ku)) }));
        return false;
      }
      // Ctrl+Enter without the kitty protocol = plain Enter; without this macOS WebKit opens the context menu
      if (e.key === "Enter" && e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey) {
        e.preventDefault();
        if (e.type === "keydown") call("pane.write", { pane, data: b64(enc.encode("\r")) });
        return false;
      }
      if (e.key !== "Enter" || !e.shiftKey || e.metaKey || e.ctrlKey || e.altKey) return true;
      if (e.type === "keydown") call("pane.write", { pane, data: b64(enc.encode("\x1b\r")) });
      return false;
    });
    term.onData((d) => {
      // FR-59: editor shown = plain letters go to the editor (focus can be left in the terminal, e.g. after a TUI exits)
      if (!/[\x00-\x1f\x7f]/.test(d) && ontype(d)) return;
      track(d);
      call("pane.write", { pane, data: b64(enc.encode(d)) });
    });
    term.onBinary((d) => call("pane.write", { pane, data: btoa(d) }));
    // the new daemon grid follows the pane size after the snapshot is written (see attach): before that vt100 cuts rows
    term.onResize(({ rows, cols }) => sized && call("pane.resize", { pane, rows, cols }));
    const ro = new ResizeObserver(() => sized && el.offsetParent && fit.fit());
    ro.observe(el);
    if (focused) term.focus();

    return () => {
      host.removeEventListener("mousemove", onHover);
      host.removeEventListener("mouseleave", leave);
      ro.disconnect();
      light.removeEventListener("change", onTheme);
      el.removeEventListener("keydown", onAlt, true);
      el.removeEventListener("paste", onPaste, true);
      unlisten.then((f) => f());
      term.dispose();
    };
  });
</script>

{#snippet acts(b: Block)}
  <span class="bacts-in">
    <button class="ghost" title={t("tile.copyCmd")} onclick={() => onblock("cmd", b.cmd)}>{t("tile.copy")}</button>
    <button class="ghost" title={t("tile.copyOut")} onclick={() => onblock("out", outputOf(b))} disabled={!b.d}>{t("tile.output")}</button>
    <button title={t("tile.editTip")} onclick={() => onblock("reuse", b.cmd)}>{t("tile.edit")}</button>
    {#if b.code}<button class="pri" title={t("tile.rerunTip")} onclick={() => onblock("run", b.cmd)}>{t("tile.rerun")}</button>{/if}
  </span>
{/snippet}

<div class="term" class:finding bind:this={el} style:--lift="{lift}px"></div>
{#if codex}
  <!-- Codex footer drawn like Claude's status: bars = share used, amber from 80%, red from 95% -->
  <div class="codex-status" style:top="{10 + (finding ? FIND_H : 0) + codexRow * rowHeight() + lift}px" style:--row-height="{rowHeight()}px"
    aria-label={[`Codex ${codex.model} ${codex.effort}`, ...meters(codex).map(([k, n]) => `${t(k)} ${n}%`)].join("; ")}>
    <span class="codex-name">◎ {codex.model}</span>{#if codex.effort}<span class="codex-effort">⚡ {codex.effort}</span>{/if}
    {#each meters(codex) as [k, n] (k)}
      <span class="codex-metric" class:warn={n >= 80} class:full={n >= 95}>{t(k)}<i><em style:width="{n}%"></em></i>{n}%</span>
    {/each}
    {#if codex.window}<span>{codex.window}</span>{/if}{#if codex.tokens}<span>{codex.tokens}</span>{/if}{#if codex.dir}<span class="codex-dir">{codex.dir}</span>{/if}
  </div>
{/if}
{#if sticky}
  <!-- FR-58: sticky block head -->
  <div class="sticky" style:top={finding ? `${FIND_H}px` : null} class:fail={!!sticky.code} class:run={!sticky.d}>
    <i class="sdot"></i>
    <span class="scmd">{sticky.cmd}</span>
    <span class="smeta"
      >{sticky.d ? (sticky.ms !== undefined ? dur(sticky.ms) : "") : t("tile.running")}{#if sticky.code}<span class="scode"> · {t("tile.exit", { code: sticky.code })}</span>{/if}</span
    >
    {@render acts(sticky)}
  </div>
{/if}
{#if hover && hover.b !== sticky && hover.top >= 0}
  <div class="bacts" style:top="{hover.top + 10 + (finding ? FIND_H : 0)}px">{@render acts(hover.b)}</div>
{/if}
{#if finding}
  <!-- pen 04: full-width bar under the pane head; buttons keep focus in the input -->
  <div class="find" role="search" style:height="{FIND_H}px">
    <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5l3.5 3.5" /></svg>
    <input
      bind:this={input}
      bind:value={query}
      oninput={() => find()}
      onkeydown={onFindKey}
      placeholder={t("tile.findPh")}
      aria-label={t("tile.findAria")}
      spellcheck="false"
    />
    <span class="count" aria-live="polite">{found}</span>
    <button title={t("tile.prevTip")} aria-label={t("tile.prev")} onpointerdown={(e) => e.preventDefault()} onclick={() => find(true)}
      ><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 10l4-4 4 4" /></svg></button>
    <button title={t("tile.nextTip")} aria-label={t("tile.next")} onpointerdown={(e) => e.preventDefault()} onclick={() => find()}
      ><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 6l4 4 4-4" /></svg></button>
    <button title={t("tile.closeTip")} aria-label={t("tile.closeFind")} onclick={closeFind}
      ><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4.5 4.5l7 7M11.5 4.5l-7 7" /></svg></button>
  </div>
{/if}

<style>
  /* Warp-style edge spacing: text doesn't touch the divider */
  .term {
    position: absolute;
    inset: 10px 8px 6px 22px; /* left 22px = timeline column (pen screen 02) */
    /* z-index of xterm decorations (6-7) stays inside the pane: doesn't cut through the palette, Messages panel, or dividers */
    isolation: isolate;
    /* blank rows shifted down (--lift) don't cover the input editor; the left timeline column stays visible */
    clip-path: inset(0 0 0 -22px);
  }
  /* FR-58: content sticks to the bottom while the buffer isn't full (see anchor()) */
  .term :global(.xterm) {
    transform: translateY(var(--lift, 0px));
  }
  .codex-status {
    position: absolute; z-index: 2; left: 22px; right: 8px; display: flex; align-items: center; gap: 9px;
    height: var(--row-height, 20px); padding: 0 7px; overflow: hidden; border-radius: 3px; background: var(--bg);
    color: var(--muted); font: 11px/1 system-ui, -apple-system, sans-serif; pointer-events: none; white-space: nowrap;
  }
  .codex-name { color: var(--tx-codex, #10a37f); font-weight: 650; }
  .codex-effort { color: var(--tx-purple, #a855f7); }
  .codex-metric { display: inline-flex; align-items: center; gap: 5px; --bar: var(--tx-codex, #10a37f); }
  .codex-metric.warn { --bar: var(--tx-amber, #fbbf24); }
  .codex-metric.full { --bar: var(--tx-red, #f87171); }
  .codex-metric i { width: 36px; height: 4px; overflow: hidden; border-radius: 999px; background: color-mix(in srgb, var(--fg) 16%, transparent); }
  .codex-metric i em { display: block; height: 100%; border-radius: inherit; background: var(--bar); }
  .codex-dir { overflow: hidden; color: var(--tx-purple, #a855f7); text-overflow: ellipsis; }
  /* FR-58: block decorations (created by xterm outside the component, hence :global) */
  .term :global(.blk) {
    box-sizing: border-box;
    /* like Warp: block separators clearly visible; xterm width (px at creation) overridden so it follows a pane that grows */
    width: 100% !important;
    border-top: 1px solid color-mix(in srgb, var(--fg) 16%, transparent);
    pointer-events: none;
  }
  .term :global(.blk.fail) {
    border-left: 3px solid #dc2626;
  }
  /* FR-56 timeline (mockup-blok-sesi E): left line in a blank 3-cell column, a dot per session, buttons on the right */
  .term :global(.tl) {
    width: 100% !important;
    pointer-events: none;
  }
  .term :global(.tl::before) {
    content: "";
    position: absolute;
    left: -12px;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--line);
  }
  .term :global(.tl-dot) {
    position: absolute;
    top: calc(var(--rh, 20px) / 2 - 5px);
    left: -17px;
    width: 7px;
    height: 7px;
    border: 2px solid #16a34a;
    border-radius: 50%;
    background: var(--bg);
  }
  .term :global(.tl.fail .tl-dot) {
    border-color: #dc2626;
    background: #dc2626;
  }
  /* the session block head covers its terminal text row (same background as the pane) */
  .term :global(.tl-hd) {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: var(--rh, 20px);
    display: flex;
    align-items: center;
    gap: 6px;
    background: var(--bg);
    color: var(--muted);
    font: 11px system-ui, -apple-system, sans-serif;
  }
  .term :global(.tl.card .tl-hd) {
    background: color-mix(in srgb, var(--fg) 4%, var(--bg));
  }
  .term :global(.tl-x) {
    flex: 1;
    color: var(--tx-red, #f87171);
    font-weight: normal;
  }
  /* command block buttons: right of the head row; agent session block: own row below (pen screen 02) */
  .term :global(.tl-acts) {
    position: absolute;
    top: 0;
    right: 12px;
    height: var(--rh, 20px);
    display: flex;
    align-items: center;
    gap: 6px;
  }
  /* two terminal rows for the button row: buttons as thick as in the pen (12px font, ±1.5 rows tall) */
  .term :global(.tl-acts.row) {
    top: calc(var(--rh, 20px) * var(--k, 3));
    left: 0;
    right: auto;
    height: calc(var(--rh, 20px) * 2);
    gap: 8px;
  }
  .term :global(.tl-acts.side) {
    height: calc(var(--rh, 20px) * var(--n, 3));
    padding-left: 8px;
    background: color-mix(in srgb, var(--fg) 4%, var(--bg));
  }
  .term :global(.tl-acts.row button) {
    height: max(22px, calc(var(--rh, 20px) * 1.5));
    padding: 0 12px;
    font-size: 12px;
  }
  .term :global(.tl.agent::before) {
    top: calc(var(--rh, 20px) / 2 + 9px);
  }
  .term :global(.tl.agent .tl-dot) {
    width: auto;
    height: auto;
    left: -19px;
    top: calc(var(--rh, 20px) / 2 - 8px);
    border: 0;
    border-radius: 0;
    background: none;
    font: 13px/16px system-ui, -apple-system, sans-serif;
  }
  .term :global(.tl-dot[data-a="✳"]) {
    color: var(--tx-claude, #d97757);
  }
  .term :global(.tl-dot[data-a="π"]) {
    color: var(--tx-purple, #a855f7);
  }
  .term :global(.tl-dot[data-a="◎"]) {
    color: var(--tx-codex, #10a37f);
  }
  .term :global(.tl-dot[data-a="◇"]) {
    color: var(--tx-blue, #60a5fa);
  }
  .term :global(.tl-x.ok) {
    color: var(--muted);
  }
  .term :global(.tl-card) {
    width: calc(100% + 18px) !important;
    margin-left: -18px;
    border-radius: 6px;
    /* see-through: the decoration layer stays above the terminal text, a solid background covers its command */
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    pointer-events: none;
  }
  .term :global(.tl-sep) {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--rh, 20px) !important;
    width: 100% !important;
    background: var(--bg);
    color: color-mix(in srgb, var(--muted) 70%, transparent);
    font: 11px system-ui, -apple-system, sans-serif;
    pointer-events: none;
  }
  .term :global(.tl-sep i) {
    flex: 1;
    height: 1px;
    background: color-mix(in srgb, var(--fg) 12%, transparent);
  }
  /* buttons fit inside the head row (don't spill upward), vertically centered */
  .term :global(.tl-acts button) {
    display: flex;
    align-items: center;
    height: max(14px, calc(var(--rh, 20px) - 3px));
    padding: 0 9px;
    line-height: 1;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--card);
    color: var(--fg);
    font: 11px system-ui, -apple-system, sans-serif;
    pointer-events: auto;
    cursor: pointer;
  }
  .term :global(.tl-acts button.pri) {
    border-color: var(--pri-line, #1d4ed8);
    background: var(--pri-bg, #1e3a8a);
    color: var(--pri-fg, #dbeafe);
  }
  .term :global(.tl-acts button.ghost) {
    border-color: transparent;
    background: none;
    color: var(--muted);
  }
  .term :global(.tl-acts button.ghost:hover) {
    color: var(--fg);
  }
  .term :global(.tl-acts button:not(.ghost):hover) {
    filter: brightness(1.25);
  }
  /* FR-58 shell blocks: line + dot in the left gutter (outside the grid, column 0 is used by the prompt), duration on the right */
  .term :global(.tl.sh:not(.hd)) {
    border-top: 1px solid color-mix(in srgb, var(--fg) 8%, transparent);
  }
  .term :global(.tl.sh.run .tl-dot) {
    border-color: #3b82f6;
    background: #3b82f6;
    animation: tl-pulse 1.4s ease-in-out infinite;
  }
  @keyframes -global-tl-pulse {
    50% {
      opacity: 0.35;
    }
  }
  /* failed: background stays neutral, red only on the dot and "keluar n" (Gembala.pen 02) */
  .term :global(.tl.sh.hd .tl-meta) {
    left: 0;
    right: auto;
  }
  .term :global(.tl-meta) {
    position: absolute;
    top: 0;
    right: 12px;
    line-height: var(--rh, 20px);
    color: var(--muted);
    font: 11px system-ui, -apple-system, sans-serif;
  }
  .term :global(.tl-meta b) {
    color: var(--tx-red, #f87171);
    font-weight: normal;
  }
  /* FR-58: block action buttons (right of the command row) and sticky block head (top of the pane) */
  .bacts,
  .sticky {
    position: absolute;
    z-index: 2;
    font: 11px system-ui, -apple-system, sans-serif;
  }
  .bacts {
    right: 14px;
  }
  .sticky {
    top: 0;
    left: 0;
    right: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 5px 12px 5px 14px;
    border-bottom: 1px solid var(--line);
    background: var(--bg);
    box-shadow: 0 4px 10px rgba(0, 0, 0, 0.25);
  }
  .sticky.fail {
    background: color-mix(in srgb, #dc2626 7%, var(--bg));
  }
  /* dot same as the block timeline: green ring, red on failure, blue pulsing while running */
  .sdot {
    flex: none;
    width: 7px;
    height: 7px;
    border: 2px solid #16a34a;
    border-radius: 50%;
  }
  .sticky.fail .sdot {
    border-color: #dc2626;
    background: #dc2626;
  }
  .sticky.run .sdot {
    border-color: #3b82f6;
    background: #3b82f6;
    animation: tl-pulse 1.4s ease-in-out infinite;
  }
  .smeta {
    color: var(--muted);
    white-space: nowrap;
  }
  .scmd {
    flex: 1;
    overflow: hidden;
    color: var(--fg);
    font: 600 12px "SF Mono", Menlo, monospace;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .scode {
    color: var(--tx-red, #f87171);
  }
  /* Gembala.pen-style block buttons: Copy/Output plain, Edit outlined, Rerun blue */
  .bacts-in {
    display: inline-flex;
    gap: 6px;
    padding: 2px 4px;
    border-radius: 6px;
    background: var(--bg);
  }
  .bacts-in button {
    padding: 2px 8px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--card);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  .bacts-in button.ghost {
    border-color: transparent;
    background: none;
    color: var(--muted);
  }
  .bacts-in button.pri {
    border-color: var(--pri-line, #1d4ed8);
    background: var(--pri-bg, #1e3a8a);
    color: var(--pri-fg, #dbeafe);
  }
  .bacts-in button:hover:not(:disabled) {
    color: var(--fg);
    filter: brightness(1.25);
  }
  .bacts-in button:disabled {
    opacity: 0.4;
  }
  .term.finding {
    top: 42px; /* 10px gap + FIND_H */
  }
  .find {
    position: absolute;
    inset: 0 0 auto 0;
    z-index: 3;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 8px 0 10px;
    border-bottom: 1px solid var(--line);
    background: var(--bg);
    color: var(--muted);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .find svg {
    flex: none;
    width: 12px;
    height: 12px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.4;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .find input {
    flex: 1;
    min-width: 0;
    padding: 3px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    outline: 0;
    background: transparent;
    color: var(--fg);
    font: 12px "SF Mono", Menlo, monospace;
  }
  .find input:focus {
    border-color: var(--tx-blue, #3b82f6);
  }
  .count {
    flex: none;
    white-space: nowrap;
  }
  .find button {
    display: flex;
    padding: 4px;
    border: 0;
    border-radius: 4px;
    background: none;
    color: inherit;
  }
  .find button:hover {
    background: var(--line);
    color: var(--fg);
  }
</style>
