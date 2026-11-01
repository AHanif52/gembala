<script lang="ts">
  // Satu pane = satu xterm yang di-attach ke daemon (FR-1, FR-2), plus FR-34 sampai 36, 41.
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
  }: {
    pane: number;
    focused: boolean;
    ui: Ui;
    zoom: number;
    onexit: (pane: number) => void;
    onlink: (url: string) => void;
    onpasted: (path: string) => void;
    online: (line: string) => void; // FR-57: teks yang diketik sejak Enter terakhir
    onkey: (key: string) => boolean; // FR-57: true = tombol dipakai daftar @, tidak diteruskan ke terminal
  } = $props();

  // ponytail: dua palet tetap; palet 16 warna dari config menyusul bila diminta
  const DARK: ITheme = { background: "#101114", foreground: "#d4d4d8", cursor: "#d4d4d8", selectionBackground: "#3f3f46" };
  const LIGHT: ITheme = { background: "#fafafa", foreground: "#27272a", cursor: "#27272a", selectionBackground: "#d4d4d8" };
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
  const enc = new TextEncoder();

  $effect(() => {
    if (focused && ready && !finding) term.focus();
  });

  const light = matchMedia("(prefers-color-scheme: light)");
  let systemLight = $state(light.matches);
  const isLight = $derived(ui.theme === "light" || (ui.theme === "system" && systemLight));

  // FR-34: [ui] dari config berlaku langsung tanpa restart; zoom ⌘=/⌘- di atas ukuran config
  let ready = $state(false); // term bukan state (objek xterm besar); efek di bawah menunggu ini
  $effect(() => {
    if (!ready) return;
    const o = term.options;
    o.fontFamily = `'${ui.font_family.replace(/['"\\]/g, "")}', 'SF Mono', Menlo, 'Symbols Nerd Font Mono', monospace`;
    o.fontSize = Math.max(6, ui.font_size + zoom);
    o.lineHeight = ui.line_height;
    o.cursorStyle = ui.cursor;
    o.cursorBlink = ui.cursor_blink;
    o.scrollback = ui.scrollback_lines;
    o.screenReaderMode = ui.screen_reader;
    o.macOptionIsMeta = ui.option_as_meta === "left" || ui.option_as_meta === "both";
    o.theme = isLight ? LIGHT : DARK;
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

  /// FR-57: baris ketikan, hanya huruf yang diketik langsung; tombol khusus (panah, Enter, paste ber-ESC) mengosongkannya.
  /// ponytail: tidak tahu isi prompt TUI sebenarnya; cukup untuk mengenali `@` di awal baris baru
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
  export const focus = () => term.focus();

  /// FR-58: blok per perintah dari penanda OSC 133 shell (A prompt, C perintah jalan + teks base64, D selesai + exit code).
  /// Hanya blok dari stream sejak attach; snapshot attach tidak memuat penanda.
  type Block = { a: IMarker; c?: IMarker; d?: IMarker; cmd: string; code?: number };
  let blocks: Block[] = [];
  function onMark(data: string) {
    const [k, arg = ""] = data.split(";", 2);
    const last = blocks.at(-1);
    if (k === "A") {
      const a = term.registerMarker(0);
      if (a) blocks.push({ a, cmd: "" });
      if (blocks.length > 1000) blocks.shift()?.a.dispose(); // ponytail: 1000 blok terakhir, sisanya ikut scrollback
    } else if (k === "C" && last && !last.c) {
      last.c = term.registerMarker(0);
      try {
        last.cmd = new TextDecoder().decode(unb64(arg));
      } catch {}
    } else if (k === "D" && last?.c && !last.d) {
      last.d = term.registerMarker(0);
      last.code = Number(arg) || 0;
      decorate(last);
    }
    return true; // penanda tidak menggambar apa pun
  }
  /// garis tipis di atas tiap blok; gagal = garis kiri merah + latar tipis
  function decorate(b: Block) {
    const height = Math.max(1, (b.d?.line ?? b.a.line) - b.a.line);
    const d = term.registerDecoration({ marker: b.a, width: term.cols, height, layer: "bottom" });
    d?.onRender((el) => {
      el.classList.add("blk");
      el.classList.toggle("fail", b.code !== 0);
      el.title = b.code ? `keluar ${b.code}` : "";
    });
  }
  const outputOf = (b: Block) => {
    if (!b.c || !b.d) return "";
    const buf = term.buffer.active;
    const rows: string[] = [];
    for (let y = b.c.line; y < Math.max(b.c.line + 1, b.d.line); y++) rows.push(buf.getLine(y)?.translateToString(true) ?? "");
    return rows.join("\n").replace(/\n+$/, "");
  };
  /// blok di baris layar `clientY` (klik kanan), atau null
  export function blockAt(clientY: number): { cmd: string; output: string; code?: number } | null {
    const rowsEl = el.querySelector(".xterm-rows");
    if (!rowsEl) return null;
    const r = rowsEl.getBoundingClientRect();
    const y = Math.floor(((clientY - r.top) / r.height) * term.rows) + term.buffer.active.viewportY;
    const b = blocks.findLast((b) => b.a.line <= y && b.d && y < Math.max(b.d.line, b.a.line + 1));
    return b ? { cmd: b.cmd, output: outputOf(b), code: b.code } : null;
  }
  /// FR-58: ⌘↑/⌘↓ lompat ke awal blok sebelum/sesudah baris teratas layar
  export function jump(dir: -1 | 1) {
    const top = term.buffer.active.viewportY;
    const b = dir < 0 ? blocks.findLast((b) => b.a.line < top) : blocks.find((b) => b.a.line > top);
    if (b) term.scrollToLine(b.a.line);
    else if (dir > 0) term.scrollToBottom();
  }

  /// FR-37: menu klik kanan
  export const selection = () => term.getSelection();
  export const paste = (text: string) => term.paste(text); // bracketed paste bila TUI memintanya

  /// FR-36: bersihkan layar dan scrollback. Shell diam juga diminta menggambar ulang prompt (Ctrl+L);
  /// bila ada proses jalan (agent), hanya scrollback yang dibuang: layar milik TUI digambar ulang per bagian,
  /// jadi layar yang dikosongkan dari luar membuat tampilan agent rusak.
  export async function clearScreen() {
    const info = await call("pane.info", { pane });
    if (info.busy) return void term.write("\x1b[3J");
    term.clear();
    call("pane.write", { pane, data: b64(new Uint8Array([0x0c])) });
  }

  // Renderer DOM bawaan xterm: terukur lebih hemat RAM dan datar per pane dibanding WebGL (TECH M1)
  onMount(() => {
    term = new Terminal({
      allowProposedApi: true, // decorations hasil cari dan Unicode 11
      // SEC-14: tautan OSC 8 tidak dibuka langsung; URL asli ditampilkan dulu
      linkHandler: { activate: (_e, uri) => onlink(uri) },
    });
    fit = new FitAddon();
    search = new SearchAddon();
    term.loadAddon(fit);
    term.loadAddon(search);
    term.loadAddon(new WebLinksAddon((_e, uri) => onlink(uri)));
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11"; // FR-41: lebar emoji dan CJK
    term.open(el);
    term.parser.registerOscHandler(133, onMark);
    ready = true;
    fit.fit();

    const onTheme = () => (systemLight = light.matches);
    light.addEventListener("change", onTheme);
    search.onDidChangeResults(({ resultIndex, resultCount }) => {
      found = resultCount ? `${resultIndex + 1}/${resultCount}` : query ? "0" : "";
    });

    // FR-35: Option kiri dan kanan diatur terpisah (bawaan: kiri Meta, kanan karakter, mis. ⌥⇧2 = €).
    // xterm hanya punya satu saklar, jadi diganti tepat saat tombol Option ditekan.
    const onAlt = (e: KeyboardEvent) => {
      const m = ui.option_as_meta;
      if (e.code === "AltLeft") term.options.macOptionIsMeta = m === "left" || m === "both";
      if (e.code === "AltRight") term.options.macOptionIsMeta = m === "right" || m === "both";
    };
    el.addEventListener("keydown", onAlt, true);

    // FR-54: ⌘V berisi gambar = simpan ke folder sementara Gembala (SEC-34), tempel path-nya ber-quote.
    // Teks biasa tidak disentuh: handler paste xterm yang menanganinya (bracketed paste).
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
        term.write(`\r\n\x1b[31mgagal menyimpan gambar: ${err}\x1b[0m\r\n`);
      }
    };
    el.addEventListener("paste", onPaste, true);

    let attached = false;
    const early: any[] = [];
    const onEvent = (e: any) => {
      if (e.event === "pane.output") term.write(unb64(e.params.data));
      // FR-56: agent keluar = layar diganti blok ringkas dari daemon
      if (e.event === "pane.replace") {
        term.reset();
        term.write(unb64(e.params.data));
      }
      if (e.event === "pane.exit") onexit(pane);
    };
    const unlisten = listen<any>("daemon", ({ payload }) => {
      if (payload.params?.pane !== pane) return;
      if (attached) onEvent(payload);
      else early.push(payload);
    });

    (async () => {
      try {
        await call("pane.resize", { pane, rows: term.rows, cols: term.cols });
        // balasan attach dan event lewat jalur berbeda di Tauri: event yang datang duluan ditahan
        const a = await call("pane.attach", { pane });
        term.write(unb64(a.snapshot));
        attached = true;
        early.splice(0).forEach(onEvent);
      } catch (e) {
        term.write(`\x1b[31mgagal attach pane ${pane}: ${e}\x1b[0m\r\n`);
      }
    })();

    // Shift+Enter = ESC CR (sama dengan /terminal-setup Claude Code di iTerm2): baris baru di TUI agent
    term.attachCustomKeyEventHandler((e) => {
      // FR-57: daftar @ terbuka = Tab/panah/Esc milik daftar
      if (e.type === "keydown" && ["Tab", "ArrowUp", "ArrowDown", "Escape"].includes(e.key) && !e.metaKey && onkey(e.key)) return false;
      if (e.key !== "Enter" || !e.shiftKey || e.metaKey || e.ctrlKey || e.altKey) return true;
      if (e.type === "keydown") call("pane.write", { pane, data: b64(enc.encode("\x1b\r")) });
      return false;
    });
    term.onData((d) => {
      track(d);
      call("pane.write", { pane, data: b64(enc.encode(d)) });
    });
    term.onBinary((d) => call("pane.write", { pane, data: btoa(d) }));
    term.onResize(({ rows, cols }) => call("pane.resize", { pane, rows, cols }));
    const ro = new ResizeObserver(() => el.offsetParent && fit.fit());
    ro.observe(el);
    if (focused) term.focus();

    return () => {
      ro.disconnect();
      light.removeEventListener("change", onTheme);
      el.removeEventListener("keydown", onAlt, true);
      el.removeEventListener("paste", onPaste, true);
      unlisten.then((f) => f());
      term.dispose();
    };
  });
</script>

<div class="term" bind:this={el}></div>
{#if finding}
  <div class="find">
    <input
      bind:this={input}
      bind:value={query}
      oninput={() => find()}
      onkeydown={onFindKey}
      placeholder="Cari"
      aria-label="Cari di pane"
      spellcheck="false"
    />
    <span class="count">{found}</span>
  </div>
{/if}

<style>
  /* jarak tepi ala Warp: teks tidak menempel ke pemisah */
  .term {
    position: absolute;
    inset: 10px 8px 6px 14px;
  }
  /* FR-58: dekorasi blok (dibuat xterm di luar komponen, jadi :global) */
  .term :global(.blk) {
    box-sizing: border-box;
    border-top: 1px solid var(--line);
    pointer-events: none;
  }
  .term :global(.blk.fail) {
    border-left: 3px solid #dc2626;
    background: rgba(220, 38, 38, 0.07);
  }
  .find {
    position: absolute;
    top: 8px;
    right: 14px;
    z-index: 3;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .find input {
    width: 180px;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--fg);
    font: inherit;
  }
  .count {
    min-width: 32px;
    text-align: right;
    color: var(--muted);
  }
</style>
