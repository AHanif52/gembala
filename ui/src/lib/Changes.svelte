<script lang="ts">
  // Panel perubahan ala Warp (FR-22): baca saja. Diff sudah disamarkan daemon (SEC-12).
  let {
    status,
    diff,
    onclose,
  }: {
    status: any;
    diff: { repo: boolean; diff?: string; truncated?: boolean; untracked?: string[] } | null;
    onclose: () => void;
  } = $props();

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
    navigator.clipboard.writeText(text).then(() => (copied = `${what} tersalin`), () => (copied = "Gagal menyalin"));
</script>

<aside class="panel" aria-label="Panel perubahan">
  <header>
    {#if status?.repo}
      <span class="repo">{status.root?.split("/").at(-1)}</span>
      <span class="m">{status.branch ?? "HEAD terlepas"} · {status.files} file <span class="add">+{status.add}</span> <span class="del">−{status.del}</span></span>
    {:else}
      <span class="repo">Perubahan</span>
    {/if}
    <span class="m">{copied}</span>
    <button class="mini" onclick={onclose} aria-label="Tutup panel">Esc</button>
  </header>
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
      <button class="mini" onclick={() => copy(diff?.diff ?? "", "Diff")}>Salin diff</button>
    {/if}
  </div>
</aside>

<style>
  .panel {
    position: absolute;
    inset: 0 0 0 auto;
    width: min(46%, 640px);
    z-index: 4;
    display: flex;
    flex-direction: column;
    border-left: 1px solid var(--line);
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
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
  .mini {
    padding: 2px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
  }
</style>
