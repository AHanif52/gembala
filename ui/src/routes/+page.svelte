<script lang="ts">
  // Fase 1 M1: grid pane satu tab (FR-1, 37, 38, 50). Tab, profil, dan pengaturan menyusul (M3–M7).
  import { onMount } from "svelte";
  import Tile from "$lib/Tile.svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { call } from "$lib/daemon";
  import { dividers, neighbor, nudge, panes, rects, remove, setRatio, split, type Arrow, type Dir, type Divider, type Node } from "$lib/layout";

  let tree = $state<Node | null>(null);
  let focus = $state(0);
  let zoomed = $state(false);
  let confirm = $state<{ pane: number; name: string } | null>(null);
  let quitAsk = $state(0); // jumlah pane yang masih menjalankan proses (FR-46)
  let error = $state("");
  let box: HTMLDivElement;

  const layout = $derived(tree ? rects(tree) : new Map());
  const lines = $derived(tree && !zoomed ? dividers(tree) : []);

  const newPane = async (): Promise<number> => (await call("pane.create", {})).pane;

  async function splitFocused(dir: Dir) {
    if (!tree) return;
    const id = await newPane();
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

  /// Aksi dari menu "Pane" (key equivalent native, lihat lib.rs). `⌘` milik app, sisanya ke terminal (FR-39).
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

  let last = { id: "", at: 0 };
  function run(id: string) {
    if (id === "app.quit_all") return askQuitAll();
    if (!tree || confirm || quitAsk) return;
    // satu tombol bisa sampai lewat menu dan keydown sekaligus; jangan jalan dua kali
    if (id === last.id && performance.now() - last.at < 150) return;
    last = { id, at: performance.now() };
    const [, group, arg] = id.split(".");
    if (group === "split") splitFocused(arg === "down" ? "col" : "row");
    else if (group === "focus" && !zoomed) focus = neighbor(tree, focus, arg as Arrow) ?? focus;
    else if (group === "resize" && !zoomed) tree = nudge(tree, focus, arg as Arrow);
    else if (group === "zoom") zoomed = !zoomed;
    else if (id === "pane.proc.kill") call("pane.kill", { pane: focus });
    else if (id === "pane.proc.close") askClose(focus);
  }

  /// Kartu konfirmasi tutup (dan ⌘⇧↩) yang menangkap tombol; lainnya milik terminal.
  function onKey(e: KeyboardEvent) {
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
    (async () => {
      try {
        // ponytail: pane lama dari daemon dijajarkan ke kanan; pulihkan tata letak asli = M5 (FR-45)
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
      <Tile {pane} focused={pane === focus} {onexit} />
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
  {#if zoomed}<span class="zoom">maksimal · ⌘⇧↩</span>{/if}
</div>

<style>
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: #101114;
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
    background: rgba(16, 17, 20, 0.45);
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
    background: linear-gradient(to right, transparent 3px, #2a2b31 3px, #2a2b31 4px, transparent 4px);
  }
  .divider.col {
    margin-top: -3px;
    height: 7px !important;
    cursor: row-resize;
    background: linear-gradient(to bottom, transparent 3px, #2a2b31 3px, #2a2b31 4px, transparent 4px);
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
    border: 1px solid #2a2b31;
    border-radius: 6px;
    background: #17181c;
    color: #d4d4d8;
    font: 13px system-ui, -apple-system, sans-serif;
    z-index: 3;
  }
  .keys,
  .zoom {
    color: #8b8b93;
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
