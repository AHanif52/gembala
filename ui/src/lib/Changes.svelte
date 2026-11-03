<script lang="ts">
  // Panel perubahan ala Warp (FR-22): baca saja, menempel di kanan (terminal menyempit). Diff disamarkan daemon (SEC-12).
  import { invoke } from "@tauri-apps/api/core";
  let {
    status,
    diff,
    width,
    onclose,
    onreload,
    onwidth,
    pname = "",
    turns = [],
    turn = null,
    ask = null,
    onturn = () => {},
    onrestore = () => {},
    onconfirm = () => {},
    oncancel = () => {},
  }: {
    status: any;
    diff: { repo: boolean; diff?: string; truncated?: boolean; untracked?: string[] } | null;
    width: number;
    onclose: () => void;
    onreload: () => void;
    onwidth: (w: number) => void;
    // FR-70: giliran agent pane ini (terbaru dulu), giliran terpilih, kartu kembalikan
    pname?: string;
    turns?: { i: number; start: number; end: number; title: string; files: number; add: number; del: number; skipped: string[] }[];
    turn?: number | null;
    ask?: { i: number; files: { kind: string; path: string }[]; others: string[]; head_moved: boolean; skipped: string[] } | null;
    onturn?: (i: number | null) => void;
    onrestore?: (i: number) => void;
    onconfirm?: () => void;
    oncancel?: () => void;
  } = $props();
  const hm = (t: number) => new Date(t * 1000).toLocaleTimeString("id-ID", { hour: "2-digit", minute: "2-digit" });
  const askTurn = $derived(ask ? turns.find((t) => t.i === ask.i) : undefined);

  function grip(e: PointerEvent) {
    const move = (m: PointerEvent) => onwidth(Math.min(innerWidth * 0.7, Math.max(320, innerWidth - m.clientX)));
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    e.preventDefault();
  }

  type File = { path: string; lines: string[]; add: number; del: number };

  const files = $derived.by((): File[] => {
    const out: File[] = [];
    for (const block of (diff?.diff ?? "").split(/^(?=diff --git )/m)) {
      const head = block.match(/^diff --git a\/(.+?) b\/(.+)$/m);
      if (!head) continue;
      const lines = block.split("\n").filter((l) => /^[ +\-@]/.test(l) && !/^(\+\+\+|---) /.test(l));
      out.push({
        path: head[2],
        lines,
        add: lines.filter((l) => l.startsWith("+")).length,
        del: lines.filter((l) => l.startsWith("-")).length,
      });
    }
    return out;
  });

  /// Baris tidak berubah lebih dari 6 berturut-turut diringkas (FR-22)
  function compact(lines: string[]): (string | number)[] {
    const out: (string | number)[] = [];
    let run: string[] = [];
    const flush = () => {
      if (run.length > 6) out.push(...run.slice(0, 2), run.length - 4, ...run.slice(-2));
      else out.push(...run);
      run = [];
    };
    for (const l of lines) {
      if (l.startsWith(" ")) run.push(l);
      else {
        flush();
        out.push(l);
      }
    }
    flush();
    return out;
  }

  let copied = $state("");
  const copy = (text: string, what: string) =>
    invoke("copy_text", { text }).then(() => (copied = `${what} tersalin`), () => (copied = "Gagal menyalin"));
</script>

<aside class="panel" aria-label="Panel perubahan" style:width="{width}px">
  <div class="grip" onpointerdown={grip} role="separator" aria-orientation="vertical" aria-label="Lebar panel"></div>
  <header>
    {#if status?.repo}
      <span class="repo">{status.root?.split("/").at(-1)}</span>
      <span class="m">{status.branch ?? "HEAD terlepas"} · {status.files} file <span class="add">+{status.add}</span> <span class="del">−{status.del}</span></span>
    {:else}
      <span class="repo">Perubahan</span>
    {/if}
    <span class="m">{copied}</span>
    <button class="mini" onclick={onreload} title="Muat ulang" aria-label="Muat ulang diff">↻</button>
    <button class="mini" onclick={onclose} title="Tutup ⌘⇧+ atau Esc" aria-label="Tutup panel">×</button>
  </header>
  {#if turns.length}
    <div class="turns">
      <span class="m">Giliran {pname} · disimpan di luar repo, 7 hari</span>
      <button class="trow" class:on={turn == null} onclick={() => onturn(null)}>
        <i class="tdot"></i><span>Semua sejak commit</span><span class="m"></span>
        <span class="m">{status?.files ?? 0} file · <span class="add">+{status?.add ?? 0}</span> <span class="del">−{status?.del ?? 0}</span></span>
      </button>
      {#each turns as t (t.i)}
        <button class="trow" class:on={turn === t.i} onclick={() => onturn(t.i)}>
          <i class="tdot"></i><span>{hm(t.start)} → {hm(t.end)}</span><span class="m ttl">{t.title}</span>
          <span class="m">{t.files} file{#if t.files} · <span class="add">+{t.add}</span> <span class="del">−{t.del}</span>{/if}</span>
        </button>
      {/each}
      {#if turn != null}
        <button class="mini undo" onclick={() => onrestore(turn!)}>↶ Kembalikan ke sebelum giliran ini</button>
      {/if}
    </div>
  {/if}
  {#if ask}
    <div class="ask" role="alertdialog" aria-label="Kembalikan giliran">
      <b>Kembalikan ke sebelum giliran {askTurn ? hm(askTurn.start) : ""}?</b>
      <span class="m">File di folder kerja balik ke kondisi saat itu. Percakapan agent tidak ikut mundur; file yang di-ignore tidak disentuh.</span>
      <div class="flist">
        {#each ask.files.slice(0, 40) as f (f.path)}<div><span class={f.kind}>{f.kind}</span> {f.path}</div>{/each}
        {#if ask.files.length > 40}<div class="m">… {ask.files.length - 40} file lagi</div>{/if}
      </div>
      {#if ask.others.length}<span class="warn">Hati-hati: {ask.others.join(", ")} juga mengubah folder ini sejak giliran itu</span>{/if}
      {#if ask.head_moved}<span class="warn">HEAD sudah berubah sejak giliran itu (commit, checkout, atau rebase)</span>{/if}
      {#if ask.skipped.length}<span class="m">Dilewati (≥ 100 MB): {ask.skipped.join(", ")}</span>{/if}
      <span class="acts">
        <button class="mini" onclick={oncancel}>Batal</button>
        <button class="mini undo" onclick={onconfirm}>Kembalikan</button>
      </span>
    </div>
  {/if}
  <div class="body">
    {#if !diff}
      <p class="m">Memuat…</p>
    {:else if !diff.repo}
      <p class="m">Folder ini bukan repo git</p>
    {:else if !files.length && !diff.untracked?.length}
      <p class="m">Tidak ada perubahan</p>
    {:else}
      {#each files as f (f.path)}
        <details open={files.length <= 5}>
          <summary>
            <span class="path">{f.path}</span>
            <span class="m"><span class="add">+{f.add}</span> <span class="del">−{f.del}</span></span>
            <button class="mini" onclick={(e) => (e.preventDefault(), copy(f.path, "Path"))}>Salin path</button>
          </summary>
          <pre>{#each compact(f.lines) as l}{#if typeof l === "number"}<span class="skip">⋯ {l} baris sama</span>{"\n"}{:else}<span
                  class:add={l.startsWith("+")}
                  class:del={l.startsWith("-")}
                  class:hunk={l.startsWith("@")}>{l}</span>{"\n"}{/if}{/each}</pre>
        </details>
      {/each}
      {#if diff.untracked?.length}
        <details>
          <summary><span class="path">File baru belum ditambahkan</span><span class="m">{diff.untracked.length}</span></summary>
          <pre>{diff.untracked.join("\n")}</pre>
        </details>
      {/if}
      {#if diff.truncated}<p class="m">Diff dipotong di 200 KB.</p>{/if}
      {#if diff.diff}<button class="mini" onclick={() => copy(diff?.diff ?? "", "Diff")}>Salin diff</button>{/if}
    {/if}
  </div>
</aside>

<style>
  .panel {
    position: fixed;
    inset: 0 0 0 auto;
    z-index: 4;
    display: flex;
    flex-direction: column;
    border-left: 1px solid var(--line);
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
  }
  .grip {
    position: absolute;
    inset: 0 auto 0 -3px;
    width: 7px;
    cursor: col-resize;
  }
  .grip:hover {
    background: linear-gradient(to right, transparent 3px, var(--muted) 3px, var(--muted) 4px, transparent 4px);
  }
  header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
  }
  .repo {
    font-weight: 600;
  }
  header .m:first-of-type {
    flex: 1;
  }
  .body {
    overflow: auto;
    padding: 8px 12px;
  }
  details {
    margin-bottom: 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
  }
  summary {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 10px;
    cursor: default;
  }
  .path {
    flex: 1;
    overflow-wrap: anywhere;
    font: 12px "SF Mono", Menlo, monospace;
  }
  pre {
    margin: 0;
    padding: 6px 10px;
    border-top: 1px solid var(--line);
    overflow-x: auto;
    font: 12px/1.45 "SF Mono", Menlo, monospace;
  }
  .m,
  .skip,
  .hunk {
    color: var(--muted);
    font-size: 12px;
  }
  .add {
    color: #16a34a;
  }
  .del {
    color: #dc2626;
  }
  /* baris diff berlatar penuh ala Warp */
  pre span.add,
  pre span.del {
    display: inline-block;
    min-width: 100%;
  }
  pre span.add {
    background: rgba(22, 163, 74, 0.12);
  }
  pre span.del {
    background: rgba(220, 38, 38, 0.12);
  }
  /* FR-70: daftar giliran (pen layar 12) */
  .turns {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
  }
  .turns > .m {
    margin-bottom: 4px;
  }
  .trow {
    display: grid;
    grid-template-columns: 8px auto 1fr auto;
    align-items: center;
    gap: 8px;
    padding: 4px 6px;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: 12px;
    text-align: left;
    cursor: pointer;
  }
  .trow.on {
    background: var(--line);
  }
  .tdot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #4a4a4a;
  }
  .trow.on .tdot {
    background: #3b82f6;
  }
  .ttl {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mini.undo {
    align-self: flex-start;
    margin-top: 6px;
    border-color: rgba(217, 119, 6, 0.5);
    background: none;
    color: #fbbf24;
  }
  .ask {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 8px 12px;
    padding: 12px;
    border: 1px solid rgba(217, 119, 6, 0.5);
    border-radius: 6px;
    background: var(--card);
  }
  .flist {
    max-height: 30vh;
    overflow: auto;
    padding: 6px 8px;
    border-radius: 4px;
    background: var(--bg);
    font: 12px "SF Mono", Menlo, monospace;
  }
  .flist .ubah {
    color: #d97706;
  }
  .flist .hapus {
    color: #f87171;
  }
  .warn {
    padding: 6px 8px;
    border-radius: 4px;
    background: rgba(217, 119, 6, 0.1);
    color: #fbbf24;
    font-size: 12px;
  }
  .acts {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .mini {
    padding: 2px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
  }
</style>
