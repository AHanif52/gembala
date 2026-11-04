<script lang="ts">
  // Laci pesan (Fase 2): panel status agent (FR-10), kartu menunggu (FR-12), antrean, riwayat,
  // kebijakan penerima (SEC-5), mute (SEC-10), izin baca pane shell (SEC-21). Semua aksi lewat kanal UI.
  import { call } from "./daemon";

  type Row = { pane: number; name: string; agent: string; status?: string; policy?: string; readable?: boolean };
  let {
    data,
    rows,
    onchange,
    onclose,
  }: { data: any; rows: Row[]; onchange: () => void; onclose: () => void } = $props();

  const KIND: Record<string, string> = {
    sent: "terkirim",
    queued: "antre",
    confirm: "menunggu persetujuan",
    rejected: "ditolak",
    reply: "balasan",
    expired: "kedaluwarsa",
  };
  const REASON: Record<string, string> = {
    target_busy: "target sedang bekerja",
    target_blocked: "target butuh izin",
    target_unknown: "status target tidak diketahui",
    acl_denied: "aturan akses",
    rate_limited: "lebih dari 10 pesan/menit",
    hop_limit: "lebih dari 3 hop",
    paused: "dijeda",
    queue_full: "antrean penuh",
    expired: "lewat 10 menit",
    rejected_by_user: "ditolak kamu",
    too_large: "lebih dari 32 KB",
    target_gone: "pane sudah ditutup",
  };

  const act = (method: string, params: object) => call(method, params).then(onchange, onchange);
  const muted = $derived(new Set<string>(data?.muted ?? []));
  /// FR-10: tugas terakhir per agent = pesan terakhir yang terkirim ke agent itu
  const lastTask = $derived(
    Object.fromEntries((data?.history ?? []).filter((h: any) => h.kind === "sent").map((h: any) => [h.msg.to, h.msg.preview])),
  );
</script>

<aside class="drawer" aria-label="Laci pesan">
  <header>
    <span>Pesan antar agent</span>
    <button class="mini" class:on={data?.paused} onclick={() => act("msg.pause", { on: !data?.paused })}
      >{data?.paused ? "Lanjutkan semua" : "Henti semua"} <span class="k">Cmd+Shift+.</span></button
    >
    <button class="mini" onclick={onclose} aria-label="Tutup laci">Esc</button>
  </header>
  <div class="body">
    <h4>Agent di workspace ini</h4>
    <section>
      {#each rows as r (r.pane)}
        <div class="row">
          <span class="grow"
            ><b>{r.name}</b><small>{r.agent} · {r.status || "tidak dilacak"}{lastTask[r.name] ? ` · tugas terakhir: ${lastTask[r.name]}` : ""}</small></span
          >
          {#if r.agent === "shell"}
            <label class="sw"><input type="checkbox" checked={r.readable} onchange={(e) => act("pane.readable", { pane: r.pane, on: e.currentTarget.checked })} /> boleh dibaca agent</label>
          {:else}
            <select aria-label="Kebijakan penerima {r.name}" value={r.policy} onchange={(e) => act("pane.policy", { pane: r.pane, policy: e.currentTarget.value })}>
              <option value="auto">otomatis</option>
              <option value="confirm">konfirmasi</option>
              <option value="deny">tolak</option>
            </select>
            <label class="sw"><input type="checkbox" checked={muted.has(r.name)} onchange={(e) => act("msg.mute", { pane: r.pane, on: e.currentTarget.checked })} /> mute</label>
          {/if}
        </div>
      {:else}
        <div class="row"><span class="m">Belum ada pane</span></div>
      {/each}
    </section>

    {#if data?.pending?.length}
      <h4>Menunggu persetujuan</h4>
      <section>
        {#each data.pending as m (m.id)}
          <div class="msg">
            <span><b>{m.from}</b> → <b>{m.to}</b> <span class="m">{m.id}</span></span>
            <pre>{m.body}</pre>
            <span class="acts">
              <button class="mini" onclick={() => act("msg.reject", { id: m.id })}>Tolak</button>
              <button class="mini" onclick={() => act("msg.approve", { id: m.id })}>Setujui</button>
            </span>
          </div>
        {/each}
      </section>
    {/if}

    {#if data?.queued?.length}
      <h4>Antrean</h4>
      <section>
        {#each data.queued as m (m.id)}
          <div class="row"><span class="grow"><b>{m.from}</b> → <b>{m.to}</b><small>{m.preview}</small></span></div>
        {/each}
      </section>
    {/if}

    <h4>Riwayat</h4>
    <section>
      {#each [...(data?.history ?? [])].reverse() as h, i (i)}
        <div class="row">
          <span class="grow"
            ><span class="kind {h.kind}">{KIND[h.kind] ?? h.kind}</span> <b>{h.msg.from}</b> → <b>{h.msg.to}</b>{#if h.reason}<span class="m">
                · {REASON[h.reason] ?? h.reason}</span
              >{/if}<small>{h.msg.preview}</small></span
          >
          <span class="m">{h.msg.hop ? `hop ${h.msg.hop}` : ""}</span>
        </div>
      {:else}
        <div class="row"><span class="m">Belum ada pesan</span></div>
      {/each}
    </section>
  </div>
</aside>

<style>
  .drawer {
    position: absolute;
    inset: 0 0 0 auto;
    width: min(44%, 560px);
    z-index: 5;
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
    gap: 8px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
  }
  header span:first-child {
    flex: 1;
    font-weight: 600;
  }
  .body {
    overflow: auto;
    padding: 4px 12px 12px;
  }
  h4 {
    margin: 14px 0 6px;
    color: var(--muted);
    font: 600 12px system-ui, -apple-system, sans-serif;
  }
  section {
    border: 1px solid var(--line);
    border-radius: 6px;
  }
  .row,
  .msg {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
  }
  .msg {
    flex-direction: column;
    align-items: stretch;
  }
  .row + .row,
  .msg + .msg {
    border-top: 1px solid var(--line);
  }
  .grow {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  small,
  .m,
  .k {
    color: var(--muted);
    font-size: 12px;
  }
  small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  pre {
    margin: 0;
    max-height: 160px;
    overflow: auto;
    white-space: pre-wrap;
    font: 12px "SF Mono", Menlo, monospace;
  }
  .acts {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
  .kind {
    font-size: 11px;
    font-weight: 600;
    color: var(--muted);
  }
  .kind.rejected,
  .kind.expired {
    color: var(--tx-red, #dc2626);
  }
  .kind.sent,
  .kind.reply {
    color: var(--tx-green, #16a34a);
  }
  .kind.confirm {
    color: var(--tx-amber, #d97706);
  }
  .sw {
    display: flex;
    align-items: center;
    gap: 4px;
    color: var(--muted);
    font-size: 12px;
    white-space: nowrap;
  }
  select,
  .mini {
    padding: 3px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .mini.on {
    border-color: #d97706;
    color: var(--tx-amber, #d97706);
  }
</style>
