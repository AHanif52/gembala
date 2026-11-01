<script lang="ts">
  // Satu pane = satu xterm yang di-attach ke daemon (FR-1, FR-2), plus FR-34 sampai 36, 41.
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { Terminal, type ITheme } from "@xterm/xterm";
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
  .term {
    position: absolute;
    inset: 6px 4px 2px 8px;
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
