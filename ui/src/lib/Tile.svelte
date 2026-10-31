<script lang="ts">
  // Satu pane = satu xterm yang di-attach ke daemon (FR-1, FR-2), plus FR-34 sampai 36, 41.
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { Terminal, type ITheme } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { SearchAddon } from "@xterm/addon-search";
  import { WebLinksAddon } from "@xterm/addon-web-links";
  import { Unicode11Addon } from "@xterm/addon-unicode11";
  import "@xterm/xterm/css/xterm.css";
  import { call, b64, unb64 } from "./daemon";

  let {
    pane,
    focused,
    fontSize,
    onexit,
    onlink,
  }: {
    pane: number;
    focused: boolean;
    fontSize: number;
    onexit: (pane: number) => void;
    onlink: (url: string) => void;
  } = $props();

  // ponytail: dua tema tetap; palet dan font dari config.toml di M3
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
    if (focused && term && !finding) term.focus();
  });

  $effect(() => {
    if (term && term.options.fontSize !== fontSize) {
      term.options.fontSize = fontSize;
      if (el.offsetParent) fit.fit();
    }
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

  /// FR-36: bersihkan layar dan scrollback. Shell diam juga diminta menggambar ulang prompt (Ctrl+L);
  /// bila ada proses jalan (agent), hanya tampilan yang dibersihkan supaya input agent tidak terganggu.
  export async function clearScreen() {
    term.clear();
    const info = await call("pane.info", { pane });
    if (!info.busy) call("pane.write", { pane, data: b64(new Uint8Array([0x0c])) });
  }

  // Renderer DOM bawaan xterm: terukur lebih hemat RAM dan datar per pane dibanding WebGL (TECH M1)
  onMount(() => {
    const light = matchMedia("(prefers-color-scheme: light)");
    term = new Terminal({
      fontFamily: "'SF Mono', Menlo, monospace",
      fontSize,
      cursorStyle: "bar",
      macOptionIsMeta: true,
      allowProposedApi: true, // decorations hasil cari dan Unicode 11
      theme: light.matches ? LIGHT : DARK,
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
    fit.fit();

    const onTheme = () => (term.options.theme = light.matches ? LIGHT : DARK);
    light.addEventListener("change", onTheme);
    search.onDidChangeResults(({ resultIndex, resultCount }) => {
      found = resultCount ? `${resultIndex + 1}/${resultCount}` : query ? "0" : "";
    });

    // FR-35: Option kiri = Meta, kanan = karakter biasa (mis. ⌥⇧2 = €). xterm hanya punya satu
    // saklar, jadi diganti tepat saat tombol Option ditekan, sebelum tombol karakternya.
    const onAlt = (e: KeyboardEvent) => {
      if (e.code === "AltLeft") term.options.macOptionIsMeta = true;
      if (e.code === "AltRight") term.options.macOptionIsMeta = false;
    };
    el.addEventListener("keydown", onAlt, true);

    let attached = false;
    const early: any[] = [];
    const onEvent = (e: any) => {
      if (e.event === "pane.output") term.write(unb64(e.params.data));
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

    term.onData((d) => call("pane.write", { pane, data: b64(enc.encode(d)) }));
    term.onBinary((d) => call("pane.write", { pane, data: btoa(d) }));
    term.onResize(({ rows, cols }) => call("pane.resize", { pane, rows, cols }));
    const ro = new ResizeObserver(() => el.offsetParent && fit.fit());
    ro.observe(el);
    if (focused) term.focus();

    return () => {
      ro.disconnect();
      light.removeEventListener("change", onTheme);
      el.removeEventListener("keydown", onAlt, true);
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
