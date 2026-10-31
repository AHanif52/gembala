<script lang="ts">
  // Pengaturan (FR-42): layar dan config.toml setara. Tiap ubahan ditulis daemon ke file (satu penulis),
  // lalu kembali sebagai `config.changed`, jadi layar selalu mencerminkan isi file.
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getVersion } from "@tauri-apps/api/app";
  import { call } from "./daemon";
  import type { Agent, Config, Ui } from "./config";

  let { cfg, onclose, onquitall }: { cfg: Config; onclose: () => void; onquitall: () => void } = $props();

  const SECTIONS = [
    ["umum", "Umum"],
    ["tampilan", "Tampilan"],
    ["agent", "Agent"],
    ["pintasan", "Pintasan"],
    ["keamanan", "Keamanan"],
    ["diagnostik", "Diagnostik"],
  ] as const;
  let sec = $state<(typeof SECTIONS)[number][0]>("tampilan");
  let failed = $state("");
  let keys = $state<[string, string, string][]>([]);
  let keyErrors = $state<string[]>([]);
  let version = $state("");
  let agents = $state<Agent[]>([]);
  let detecting = $state(false);

  /// FR-28: deteksi lewat login shell; "Periksa ulang" memaksa deteksi baru
  async function detect(refresh: boolean) {
    detecting = true;
    agents = await call("agents.detect", { refresh }).catch(() => []);
    detecting = false;
  }
  const ui = $derived(cfg.values.ui);

  onMount(async () => {
    detect(false);
    version = await getVersion();
  });

  // [keys] berubah = menu dibangun ulang di sisi Rust; ambil ulang daftar yang berlaku
  $effect(() => {
    void cfg.values.keys;
    invoke<[[string, string, string][], string[]]>("shortcuts").then(([k, e]) => {
      keys = k;
      keyErrors = e;
    });
  });

  async function set<K extends keyof Ui>(key: K, value: Ui[K]) {
    failed = "";
    const r = await call("config.set", { section: "ui", key, value }).catch((e) => ({ error: String(e) }));
    if (r.error) failed = r.error;
  }

  // tampilkan pintasan dengan simbol Mac
  const pretty = (k: string) =>
    k
      .replace("Cmd", "⌘")
      .replace("Shift", "⇧")
      .replace("Alt", "⌥")
      .replace("Ctrl", "⌃")
      .replace("Enter", "↩")
      .replace(/Left|Right|Up|Down/, (d) => ({ Left: "←", Right: "→", Up: "↑", Down: "↓" })[d]!)
      .replaceAll("+", "");
</script>

<div class="veil" role="presentation" onpointerdown={(e) => e.target === e.currentTarget && onclose()}>
  <div class="modal" role="dialog" aria-label="Pengaturan">
    <nav>
      {#each SECTIONS as [k, t]}
        <button class:on={k === sec} onclick={() => (sec = k)}>{t}</button>
      {/each}
    </nav>
    <div class="body">
      {#if cfg.errors.length}
        <div class="errs" role="alert">
          <b>Sebagian config.toml tidak dipakai</b>
          {#each cfg.errors as e}<span>{e}</span>{/each}
        </div>
      {/if}
      {#if failed}<div class="errs" role="alert"><span>{failed}</span></div>{/if}

      {#if sec === "umum"}
        <section>
          <div class="row"><span>Bahasa</span><span class="m">Indonesia</span></div>
          <div class="row"><span>Daemon<small>Memegang semua pane; tetap jalan selama ada proses di pane</small></span><span class="m">berjalan · protokol 1</span></div>
          <div class="row">
            <span>Keluar dan hentikan semua agent<small>Menghentikan daemon dan semua proses di pane</small></span>
            <button class="mini" onclick={onquitall}>Hentikan…</button>
          </div>
        </section>
      {:else if sec === "tampilan"}
        <section>
          <div class="row">
            <span>Tema</span>
            <span class="seg">
              {#each [["system", "Sistem"], ["dark", "Gelap"], ["light", "Terang"]] as [v, t]}
                <button class:on={ui.theme === v} onclick={() => set("theme", v as Ui["theme"])}>{t}</button>
              {/each}
            </span>
          </div>
          <div class="row">
            <span>Huruf</span>
            <select aria-label="Huruf terminal" value={ui.font_family} onchange={(e) => set("font_family", e.currentTarget.value)}>
              {#each [...new Set([ui.font_family, "SF Mono", "Menlo", "Monaco"])] as f}<option>{f}</option>{/each}
            </select>
          </div>
          <div class="row">
            <span>Ukuran<small>⌘= dan ⌘- mengubah sementara, ⌘0 kembali ke sini</small></span>
            <select aria-label="Ukuran huruf" value={ui.font_size} onchange={(e) => set("font_size", +e.currentTarget.value)}>
              {#each [10, 11, 12, 13, 14, 15, 16, 18, 20] as n}<option value={n}>{n} px</option>{/each}
            </select>
          </div>
          <div class="row">
            <span>Tinggi baris</span>
            <select aria-label="Tinggi baris" value={ui.line_height} onchange={(e) => set("line_height", +e.currentTarget.value)}>
              {#each [1.0, 1.1, 1.2, 1.3, 1.4, 1.5] as n}<option value={n}>{n.toFixed(1)}</option>{/each}
            </select>
          </div>
          <div class="row">
            <span>Kursor</span>
            <span class="seg">
              {#each [["bar", "Garis"], ["block", "Blok"], ["underline", "Garis bawah"]] as [v, t]}
                <button class:on={ui.cursor === v} onclick={() => set("cursor", v as Ui["cursor"])}>{t}</button>
              {/each}
            </span>
          </div>
          <div class="row">
            <span>Kursor berkedip</span>
            <input type="checkbox" role="switch" aria-label="Kursor berkedip" checked={ui.cursor_blink} onchange={(e) => set("cursor_blink", e.currentTarget.checked)} />
          </div>
          <div class="row">
            <span>Option sebagai Meta<small>Yang bukan Meta mengetik simbol (mis. ⌥⇧2 = €)</small></span>
            <select aria-label="Option sebagai Meta" value={ui.option_as_meta} onchange={(e) => set("option_as_meta", e.currentTarget.value as Ui["option_as_meta"])}>
              <option value="left">Kiri saja</option>
              <option value="right">Kanan saja</option>
              <option value="both">Keduanya</option>
              <option value="none">Tidak ada</option>
            </select>
          </div>
          <div class="row">
            <span>Scrollback<small>Baris yang disimpan per pane di layar</small></span>
            <select aria-label="Scrollback" value={ui.scrollback_lines} onchange={(e) => set("scrollback_lines", +e.currentTarget.value)}>
              {#each [1000, 5000, 10000, 50000] as n}<option value={n}>{n.toLocaleString("id-ID")} baris</option>{/each}
            </select>
          </div>
          <div class="row">
            <span>Mode pembaca layar<small>Untuk VoiceOver</small></span>
            <input type="checkbox" role="switch" aria-label="Mode pembaca layar" checked={ui.screen_reader} onchange={(e) => set("screen_reader", e.currentTarget.checked)} />
          </div>
        </section>
      {:else if sec === "agent"}
        <section>
          {#each agents as a (a.agent)}
            <div class="row">
              <span
                >{a.name}<small
                  >{a.installed
                    ? `${a.version ?? "versi tidak terbaca"} · ${a.tested ? "teruji" : "belum diuji, status dari pola layar"}`
                    : `Tidak ditemukan di PATH login shell. Pasang: ${a.install}`}</small
                ></span
              >
              <span class="m">{a.installed ? "✓ terpasang" : "✗ tidak ada"}</span>
            </div>
          {/each}
          <div class="row">
            <span>Periksa ulang<small>Mencari agent lewat login shell, sama dengan terminal biasa</small></span>
            <button class="mini" disabled={detecting} onclick={() => detect(true)}>{detecting ? "Memeriksa…" : "Periksa"}</button>
          </div>
        </section>
        <h4>Profil</h4>
        <section>
          {#each cfg.values.profiles as p (p.id)}
            <div class="row">
              <span
                >{p.name}<small
                  >{p.command ? [p.command, ...p.args].join(" ") : "$SHELL -l"} · folder {p.cwd || "ikut pane yang fokus"}{p.env.length
                    ? ` · env ${p.env.join(", ")}`
                    : ""}{p.secret_env.length ? ` · rujukan ${p.secret_env.map((x) => x.name).join(", ")}` : ""}</small
                ></span
              >
              <span class="m">{p.id}</span>
            </div>
          {/each}
        </section>
        <p class="note">
          Ubah atau tambah lewat <code>[[profile]]</code> di config.toml (id sama = menimpa bawaan). Rahasia hanya sebagai rujukan
          <code>op://…</code> atau <code>keychain:service/akun</code> di <code>secret_env</code>, dibuka shell pane saat agent dijalankan.
        </p>
      {:else if sec === "pintasan"}
        {#if keyErrors.length}
          <div class="errs" role="alert">
            <b>Masalah di [keys]</b>
            {#each keyErrors as e}<span>{e}</span>{/each}
          </div>
        {/if}
        <section>
          {#each keys as [id, label, key]}
            <div class="row"><span>{label}<small class="m">{id}</small></span><kbd>{key ? pretty(key) : "—"}</kbd></div>
          {/each}
          <div class="row"><span>Salin / tempel</span><kbd>⌘C ⌘V</kbd></div>
          <div class="row"><span>Pengaturan</span><kbd>⌘,</kbd></div>
          <div class="row"><span>Shift+Tab, Ctrl, dan tombol lain</span><span class="m">diteruskan ke terminal</span></div>
        </section>
        <p class="note">
          ⌘ milik aplikasi; tombol lain milik terminal. Ubah lewat bagian <code>[keys]</code> di config.toml, mis.
          <code>"cmd+t" = "pane.split.right"</code>; berlaku tanpa restart.
        </p>
      {:else if sec === "keamanan"}
        <section>
          <div class="row"><span>Folder socket<small>Bukan symlink, milik kamu, izin 0700</small></span><span class="m">$TMPDIR/gembala</span></div>
          <div class="row"><span>Kanal UI<small>Rahasia acak per daemon, disimpan di Keychain milik app</small></span><span class="m">terkunci</span></div>
          <div class="row"><span>Env pane<small>Penanda sesi agent yang menjalankan Gembala tidak diteruskan</small></span><span class="m">bersih</span></div>
          <div class="row"><span>Tautan<small>URL asli ditampilkan dulu; hanya http dan https</small></span><span class="m">konfirmasi</span></div>
          <div class="row"><span>Jaringan<small>App tidak menghubungi internet</small></span><span class="m">mati</span></div>
        </section>
      {:else}
        <section>
          <div class="row"><span>Versi app</span><span class="m">{version}</span></div>
          <div class="row"><span>File config</span><span class="m">{cfg.path}</span></div>
          <div class="row"><span>Status config</span><span class="m">{cfg.errors.length ? `${cfg.errors.length} masalah, lihat atas` : "bersih"}</span></div>
        </section>
      {/if}
    </div>
    <footer>
      <span>{cfg.path} · perubahan file berlaku tanpa restart</span>
      <button class="mini" onclick={onclose}>Tutup</button>
    </footer>
  </div>
</div>

<style>
  .veil {
    position: fixed;
    inset: 0;
    z-index: 10;
    display: grid;
    place-items: center;
    background: var(--dim);
  }
  .modal {
    display: grid;
    grid-template-columns: 150px 1fr;
    grid-template-rows: 1fr auto;
    width: min(720px, calc(100vw - 32px));
    height: min(560px, calc(100vh - 32px));
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
    overflow: hidden;
  }
  nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 12px 8px;
    border-right: 1px solid var(--line);
  }
  nav button {
    text-align: left;
    padding: 6px 10px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--muted);
    font: inherit;
  }
  nav button.on {
    background: var(--line);
    color: var(--fg);
  }
  .body {
    overflow: auto;
    padding: 12px 18px;
  }
  section {
    border: 1px solid var(--line);
    border-radius: 6px;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    min-height: 40px;
    padding: 6px 12px;
  }
  .row + .row {
    border-top: 1px solid var(--line);
  }
  .row > span:first-child {
    display: flex;
    flex-direction: column;
  }
  small,
  .note,
  code,
  footer {
    color: var(--muted);
    font-size: 12px;
  }
  .m,
  kbd {
    font: 12px "SF Mono", Menlo, monospace;
    color: var(--muted);
    overflow-wrap: anywhere;
    text-align: right;
  }
  select,
  .seg button,
  .mini {
    padding: 4px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }
  .seg {
    display: flex;
  }
  .seg button {
    border-radius: 0;
  }
  .seg button:first-child {
    border-radius: 4px 0 0 4px;
  }
  .seg button:last-child {
    border-radius: 0 4px 4px 0;
  }
  .seg button.on {
    background: var(--line);
  }
  h4 {
    margin: 16px 0 8px;
    font: 600 12px system-ui, -apple-system, sans-serif;
    color: var(--muted);
  }
  .errs {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-bottom: 12px;
    padding: 10px 12px;
    border: 1px solid #b45309;
    border-radius: 6px;
    font: 12px "SF Mono", Menlo, monospace;
  }
  .errs b {
    font: 600 13px system-ui, -apple-system, sans-serif;
  }
  footer {
    grid-column: 1 / -1;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 12px;
    border-top: 1px solid var(--line);
  }
</style>
