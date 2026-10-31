<script lang="ts">
  // Fase 1 M1: grid pane satu tab (FR-1, 37, 38, 50). Tab, profil, dan pengaturan menyusul (M3–M7).
  import { onMount } from "svelte";
  import Tile from "$lib/Tile.svelte";
  import Settings from "$lib/Settings.svelte";
  import { UI_DEFAULT, type Agent, type Config, type Profile } from "$lib/config";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { call } from "$lib/daemon";
  import { dividers, neighbor, nudge, panes, rects, remove, setRatio, split, type Arrow, type Dir, type Divider, type Node } from "$lib/layout";

  let tree = $state<Node | null>(null);
  let focus = $state(0);
  let zoomed = $state(false);
  let confirm = $state<{ pane: number; name: string } | null>(null);
  let quitAsk = $state(0); // jumlah pane yang masih menjalankan proses (FR-46)
  let linkAsk = $state(""); // SEC-14: URL asli ditampilkan sebelum dibuka
  let zoom = $state(0); // FR-34 ⌘=/⌘- sementara di atas ukuran config; ponytail: zoom per pane menyusul
  let cfg = $state<Config | null>(null);
  let settingsOpen = $state(false);
  let picker = $state<{ items: (Profile & { missing: boolean })[]; sel: number } | null>(null);
  const ui = $derived(cfg?.values.ui ?? UI_DEFAULT);

  $effect(() => {
    const r = document.documentElement;
    if (ui.theme === "system") delete r.dataset.theme;
    else r.dataset.theme = ui.theme;
  });
  const tiles: Record<number, Tile> = $state({});
  let error = $state("");
  let box: HTMLDivElement;

  const layout = $derived(tree ? rects(tree) : new Map());
  const lines = $derived(tree && !zoomed ? dividers(tree) : []);

  const newPane = async (): Promise<number> => (await call("pane.create", {})).pane;

  async function splitFocused(dir: Dir, profile?: string) {
    if (!tree) return;
    const id = profile ? (await call("pane.create", { profile, from_pane: focus })).pane : await newPane();
    tree = split(tree, focus, dir, id);
    focus = id;
    zoomed = false;
  }

  /// FR-38: konfirmasi bila ada proses selain shell diam
  async function askClose(pane: number) {
    const info = await call("pane.info", { pane });
    if (info.busy) confirm = { pane, name: info.name ?? "proses" };
    else await call("pane.close", { pane });
  }

  async function onexit(pane: number) {
    if (!tree) return;
    if (confirm?.pane === pane) confirm = null;
    const next = (zoomed ? null : neighbor(tree, pane, "left") ?? neighbor(tree, pane, "up")) ?? null;
    tree = remove(tree, pane);
    zoomed = false;
    if (!tree) {
      const id = await newPane();
      tree = { pane: id };
    }
    if (focus === pane || !panes(tree).includes(focus)) focus = next ?? panes(tree)[0];
  }

  /// FR-46: konfirmasi hanya bila masih ada proses jalan
  async function askQuitAll() {
    const ids: number[] = await call("pane.list");
    const infos = await Promise.all(ids.map((pane) => call("pane.info", { pane })));
    quitAsk = infos.filter((i) => i.busy).length;
    if (!quitAsk) quitAll();
  }

  async function quitAll() {
    quitAsk = 0;
    await call("daemon.stop").catch(() => {});
    await invoke("quit");
  }

  const onlink = (url: string) => (linkAsk = url);

  /// FR-30: pilih profil; agent yang tidak terdeteksi tetap bisa dipilih (pane menampilkan cara pasang)
  async function openPicker() {
    const agents: Agent[] = await call("agents.detect", {}).catch(() => []);
    const missing = new Set(agents.filter((a) => !a.installed).map((a) => a.agent));
    picker = { items: (cfg?.values.profiles ?? []).map((p) => ({ ...p, missing: missing.has(p.agent) })), sel: 0 };
  }

  function pick(i: number) {
    const p = picker?.items[i];
    picker = null;
    if (p) splitFocused("row", p.id);
  }

  async function openLink() {
    const url = linkAsk;
    linkAsk = "";
    await invoke("open_url", { url }).catch(() => {});
  }

  /// Aksi dari menu "Pane" dan "View" (key equivalent native, lihat lib.rs). `⌘` milik app, sisanya ke terminal (FR-39).
  let last = { id: "", at: 0 };
  function run(id: string) {
    if (id === "app.quit_all") return askQuitAll();
    if (id === "app.settings") return void (settingsOpen = !settingsOpen);
    if (settingsOpen || picker) return;
    if (!tree || confirm || quitAsk || linkAsk) return;
    // satu tombol bisa sampai lewat menu dan keydown sekaligus; jangan jalan dua kali
    if (id === last.id && performance.now() - last.at < 150) return;
    last = { id, at: performance.now() };
    const [, group, arg] = id.split(".");
    if (id === "pane.new.pick") openPicker();
    else if (group === "split") splitFocused(arg === "down" ? "col" : "row");
    else if (group === "focus" && !zoomed) focus = neighbor(tree, focus, arg as Arrow) ?? focus;
    else if (group === "resize" && !zoomed) tree = nudge(tree, focus, arg as Arrow);
    else if (group === "zoom") zoomed = !zoomed;
    else if (id === "pane.proc.kill") call("pane.kill", { pane: focus });
    else if (id === "pane.proc.close") askClose(focus);
    else if (id === "term.find") tiles[focus]?.openFind();
    else if (id === "term.clear") tiles[focus]?.clearScreen();
    else if (id === "term.zoom.in") zoom = Math.min(16, zoom + 1);
    else if (id === "term.zoom.out") zoom = Math.max(-6, zoom - 1);
    else if (id === "term.zoom.reset") zoom = 0;
  }

  /// Kartu konfirmasi (tutup, keluar, buka tautan) dan ⌘⇧↩ yang menangkap tombol; lainnya milik terminal.
  function onKey(e: KeyboardEvent) {
    if (picker) {
      const n = picker.items.length;
      if (e.key === "ArrowDown") picker.sel = (picker.sel + 1) % n;
      else if (e.key === "ArrowUp") picker.sel = (picker.sel + n - 1) % n;
      else if (e.key === "Enter") pick(picker.sel);
      else if (e.key === "Escape") picker = null;
      else if (/^[1-9]$/.test(e.key)) pick(+e.key - 1);
      e.preventDefault();
      e.stopPropagation();
      return;
    }
    if (settingsOpen && !quitAsk) {
      if (e.key === "Escape") {
        settingsOpen = false;
        e.preventDefault();
      }
      return; // kontrol di Pengaturan menerima tombol seperti biasa
    }
    if (linkAsk) {
      if (e.key === "Enter") openLink();
      if (e.key === "Escape") linkAsk = "";
      e.preventDefault();
      e.stopPropagation();
      return;
    }
    if (quitAsk) {
      if (e.key === "Enter") quitAll();
      if (e.key === "Escape") quitAsk = 0;
      e.preventDefault();
      e.stopPropagation();
      return;
    }
    // WKWebView memakan ⌘⇧↩ sebelum menu sempat menerimanya
    if (!confirm && e.metaKey && e.shiftKey && e.key === "Enter") {
      run("pane.zoom.toggle");
      e.preventDefault();
      e.stopPropagation();
      return;
    }
    if (!confirm) return;
    if (e.key === "Enter") call("pane.close", { pane: confirm.pane });
    if (e.key === "Enter" || e.key === "Escape") confirm = null;
    e.preventDefault();
    e.stopPropagation();
  }

  function drag(d: Divider, e: PointerEvent) {
    const b = box.getBoundingClientRect();
    const move = (m: PointerEvent) => {
      const at = d.dir === "row" ? (m.clientX - b.left) / b.width : (m.clientY - b.top) / b.height;
      const [start, size] = d.dir === "row" ? [d.rect.x, d.rect.w] : [d.rect.y, d.rect.h];
      if (tree) tree = setRatio(tree, d.path, (at - start) / size);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    e.preventDefault();
  }

  const pct = (v: number) => `${v * 100}%`;

  onMount(() => {
    window.addEventListener("keydown", onKey, true);
    const unlisten = listen<string>("shortcut", ({ payload }) => run(payload));
    const unlistenCfg = listen<any>("daemon", ({ payload }) => {
      if (payload.event === "config.changed") cfg = payload.params;
    });
    (async () => {
      try {
        // ponytail: pane lama dari daemon dijajarkan ke kanan; pulihkan tata letak asli = M5 (FR-45)
        cfg = await call("config.get");
        const ids: number[] = await call("pane.list");
        if (!ids.length) ids.push(await newPane());
        let t: Node = { pane: ids[0] };
        for (const id of ids.slice(1)) t = split(t, panes(t).at(-1)!, "row", id);
        tree = t;
        focus = ids.at(-1)!;
      } catch (e) {
        error = `gagal sambung ke daemon: ${e}`;
      }
    })();
    return () => {
      window.removeEventListener("keydown", onKey, true);
      unlisten.then((f) => f());
      unlistenCfg.then((f) => f());
    };
  });
</script>

<div class="grid" bind:this={box}>
  {#if error}<p class="error">{error}</p>{/if}
  {#each [...layout] as [pane, r] (pane)}
    {@const hidden = zoomed && pane !== focus}
    {@const at = zoomed ? { x: 0, y: 0, w: 1, h: 1 } : r}
    <div
      class="tile"
      class:dim={pane !== focus && layout.size > 1}
      style:display={hidden ? "none" : "block"}
      style:left={pct(at.x)}
      style:top={pct(at.y)}
      style:width={pct(at.w)}
      style:height={pct(at.h)}
      onpointerdown={() => (focus = pane)}
      role="presentation"
    >
      <Tile bind:this={tiles[pane]} {pane} focused={pane === focus} {ui} {zoom} {onexit} {onlink} />
      {#if confirm?.pane === pane}
        <div class="confirm" role="alertdialog" aria-label="Konfirmasi tutup pane">
          <span><b>{confirm?.name}</b> masih jalan. Tutup pane?</span>
          <span class="keys">Enter tutup · Esc batal</span>
        </div>
      {/if}
    </div>
  {/each}
  {#each lines as d (d.path)}
    <div
      class="divider {d.dir}"
      style:left={d.dir === "row" ? pct(d.at) : pct(d.rect.x)}
      style:top={d.dir === "row" ? pct(d.rect.y) : pct(d.at)}
      style:width={d.dir === "row" ? "0" : pct(d.rect.w)}
      style:height={d.dir === "row" ? pct(d.rect.h) : "0"}
      onpointerdown={(e) => drag(d, e)}
      role="separator"
      aria-orientation={d.dir === "row" ? "vertical" : "horizontal"}
    ></div>
  {/each}
  {#if quitAsk}
    <div class="confirm" role="alertdialog" aria-label="Konfirmasi keluar">
      <span><b>{quitAsk} pane</b> masih menjalankan proses. Keluar dan hentikan semua?</span>
      <span class="keys">Enter keluar · Esc batal</span>
    </div>
  {/if}
  {#if linkAsk}
    <div class="confirm" role="alertdialog" aria-label="Konfirmasi buka tautan">
      <span>Buka di browser?</span>
      <code class="url">{linkAsk}</code>
      <span class="keys">Enter buka · Esc batal</span>
    </div>
  {/if}
  {#if picker}
    <div class="confirm picker" role="listbox" aria-label="Pane baru dari profil">
      {#each picker.items as p, i (p.id)}
        <button class="opt" class:on={i === picker.sel} role="option" aria-selected={i === picker.sel} onclick={() => pick(i)}>
          <span class="num">{i < 9 ? i + 1 : ""}</span>
          <span>{p.name}</span>
          <span class="keys">{p.missing ? "tidak terpasang" : p.command || "$SHELL"}</span>
        </button>
      {/each}
      <span class="keys">↑↓ pilih · Enter buka · Esc batal · folder ikut pane yang fokus</span>
    </div>
  {/if}
  {#if settingsOpen && cfg}
    <Settings {cfg} onclose={() => (settingsOpen = false)} onquitall={askQuitAll} />
  {/if}
  {#if zoomed}<span class="zoom">maksimal · ⌘⇧↩</span>{/if}
</div>

<style>
  :global(:root) {
    --bg: #101114;
    --fg: #d4d4d8;
    --muted: #8b8b93;
    --line: #2a2b31;
    --card: #17181c;
    --dim: rgba(16, 17, 20, 0.45);
  }
  /* tema mengikuti sistem kecuali [ui] theme memaksa (data-theme) */
  @media (prefers-color-scheme: light) {
    :global(:root:not([data-theme="dark"])) {
      --bg: #fafafa;
      --fg: #27272a;
      --muted: #71717a;
      --line: #e4e4e7;
      --card: #ffffff;
      --dim: rgba(250, 250, 250, 0.5);
    }
  }
  :global(:root[data-theme="light"]) {
    --bg: #fafafa;
    --fg: #27272a;
    --muted: #71717a;
    --line: #e4e4e7;
    --card: #ffffff;
    --dim: rgba(250, 250, 250, 0.5);
  }
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: var(--bg);
    color: var(--fg);
    overflow: hidden;
  }
  .grid {
    position: fixed;
    inset: 0;
  }
  .tile {
    position: absolute;
    box-sizing: border-box;
  }
  /* redup pakai lapisan tipis, bukan opacity: opacity memaksa layer GPU seukuran pane */
  .tile.dim::after {
    content: "";
    position: absolute;
    inset: 0;
    background: var(--dim);
    pointer-events: none;
  }
  .divider {
    position: absolute;
    z-index: 2;
  }
  /* garis 1px, area tangkap 7px */
  .divider.row {
    margin-left: -3px;
    width: 7px !important;
    cursor: col-resize;
    background: linear-gradient(to right, transparent 3px, var(--line) 3px, var(--line) 4px, transparent 4px);
  }
  .divider.col {
    margin-top: -3px;
    height: 7px !important;
    cursor: row-resize;
    background: linear-gradient(to bottom, transparent 3px, var(--line) 3px, var(--line) 4px, transparent 4px);
  }
  .confirm {
    position: absolute;
    left: 50%;
    top: 40%;
    transform: translate(-50%, -50%);
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 12px 16px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
    z-index: 3;
  }
  .picker {
    min-width: 320px;
    gap: 2px;
  }
  .opt {
    display: grid;
    grid-template-columns: 18px 1fr auto;
    gap: 10px;
    align-items: center;
    padding: 6px 8px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--fg);
    font: inherit;
    text-align: left;
  }
  .opt.on {
    background: var(--line);
  }
  .num {
    color: var(--muted);
    font: 12px "SF Mono", Menlo, monospace;
  }
  .url {
    max-width: 60vw;
    overflow-wrap: anywhere;
    font: 12px "SF Mono", Menlo, monospace;
  }
  .keys,
  .zoom {
    color: var(--muted);
    font-size: 12px;
  }
  .zoom {
    position: absolute;
    right: 12px;
    bottom: 6px;
    font-family: system-ui, -apple-system, sans-serif;
    z-index: 3;
  }
  .error {
    color: #f87171;
    font: 13px Menlo, monospace;
    padding: 8px 12px;
  }
</style>
