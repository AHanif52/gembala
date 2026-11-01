<script lang="ts">
  // Pengaturan (FR-42): layar dan config.toml setara. Tiap ubahan ditulis daemon ke file (satu penulis),
  // lalu kembali sebagai `config.changed`, jadi layar selalu mencerminkan isi file.
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getVersion } from "@tauri-apps/api/app";
  import { call } from "./daemon";
  import { pretty, type Agent, type Config, type Profile, type Ui } from "./config";

  let {
    cfg,
    meta,
    onclose,
    onquitall,
  }: { cfg: Config; meta: Record<number, { name: string; agent: string; profile: string; status?: string }>; onclose: () => void; onquitall: () => void } = $props();
  let daemon = $state<{ daemon: string; proto: number } | null>(null);
  // FR-29: ubah profil dari layar; satu baris = satu argumen / satu env
  type Form = { id: string; name: string; command: string; args: string; cwd: string; env: string; secret: string; summarize: boolean; notify: boolean; isNew: boolean };
  let form = $state<Form | null>(null);
  const BUILTIN = ["shell", "claude", "codex", "opencode", "omp"];

  function edit(p: Profile, copy = false) {
    let id = p.id;
    if (copy) for (let n = 2; cfg.values.profiles.some((x) => x.id === id); n++) id = `${p.id}-${n}`;
    form = {
      id,
      name: copy ? `${p.name} ${id.split("-").at(-1)}` : p.name,
      command: p.command,
      args: p.args.join("\n"),
      cwd: p.cwd,
      env: p.env.map((k) => `${k}=`).join("\n"), // nilai env tidak dikirim ke UI; isi ulang bila mengubah
      secret: p.secret_env.map((x) => `${x.name}=${x.ref}`).join("\n"),
      summarize: p.summarize,
      notify: p.notify,
      isNew: copy,
    };
  }

  const pairs = (text: string) =>
    Object.fromEntries(text.split("\n").map((l) => l.trim()).filter((l) => l.includes("=")).map((l) => [l.slice(0, l.indexOf("=")).trim(), l.slice(l.indexOf("=") + 1).trim()]));

  async function saveForm() {
    if (!form) return;
    failed = "";
    // hanya env yang diisi; yang dibiarkan kosong tetap memakai nilai lama di file
    const env = Object.fromEntries(Object.entries(pairs(form.env)).filter(([, v]) => v));
    const profile = {
      id: form.id.trim(),
      name: form.name.trim(),
      command: form.command.trim(),
      args: form.args.split("\n").map((a) => a.trim()).filter(Boolean),
      cwd: form.cwd.trim(),
      env,
      secret_env: pairs(form.secret),
      summarize: form.summarize,
      notify: form.notify,
    };
    const r = await call("profile.save", { profile }).catch((e) => ({ error: String(e) }));
    if (r.error) failed = r.error;
    else form = null;
  }

  async function resetProfile(id: string) {
    failed = "";
    const r = await call("profile.reset", { id }).catch((e) => ({ error: String(e) }));
    if (r.error) failed = r.error;
  }
  let resetAsk = $state(false);
  let copied = $state("");

  /// FR-49: info laporan bug tanpa env, folder, atau isi layar (SEC-12)
  async function copyInfo() {
    const rows = Object.values(meta).map((m) => `${m.name || m.profile} (${m.agent || "shell"}) status=${m.status || "-"}`);
    const text = [`Gembala ${version} · daemon ${daemon?.daemon} · protokol ${daemon?.proto}`, `${navigator.userAgent}`, `pane: ${rows.length}`, ...rows].join("\n");
    await navigator.clipboard.writeText(text).then(() => (copied = "Tersalin"), () => (copied = "Gagal menyalin"));
  }

  /// FR-48: hapus config, tata letak, hook, dan rahasia Keychain milik Gembala, lalu keluar
  async function reset() {
    await call("daemon.reset").catch(() => {});
    await invoke("forget_secret").catch(() => {});
    await invoke("quit");
  }

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
  let keyEdit = $state<{ id: string; combo: string } | null>(null);

  /// FR-39: kombinasi ditulis sebagai teks (WKWebView tidak meneruskan semua kombinasi ⌘ ke halaman)
  async function saveKey(id: string, combo: string) {
    failed = "";
    keyEdit = null;
    const r = await call("keys.set", { action: id, combo: combo.trim() }).catch((e) => ({ error: String(e) }));
    if (r.error) failed = r.error;
  }
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
    daemon = await call("daemon.info").catch(() => null);
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

  async function set<K extends keyof Ui>(key: K, value: Ui[K], section = "ui") {
    failed = "";
    const r = await call("config.set", { section, key, value }).catch((e) => ({ error: String(e) }));
    if (r.error) failed = r.error;
  }
  const setNotify = (key: string, value: boolean | number) => set(key as keyof Ui, value as never, "notify");

</script>

{#snippet editor()}
  {#if form}
    <div class="form">
      <label>Id <input bind:value={form.id} disabled={!form.isNew} spellcheck="false" /></label>
      <label>Nama <input bind:value={form.name} /></label>
      <label>Perintah <input bind:value={form.command} placeholder="kosong = shell" spellcheck="false" /></label>
      <label>Argumen <small>satu per baris</small><textarea bind:value={form.args} rows="2" spellcheck="false"></textarea></label>
      <label>Folder <input bind:value={form.cwd} placeholder="kosong = ikut pane yang fokus" spellcheck="false" /></label>
      <label>Env <small>NAMA=nilai per baris; nilai tidak ditampilkan ulang, isi lagi bila ingin mengubah</small><textarea bind:value={form.env} rows="2" spellcheck="false"></textarea></label>
      <label>Rujukan rahasia <small>NAMA=op://vault/item/field atau NAMA=keychain:service/akun</small><textarea bind:value={form.secret} rows="2" spellcheck="false"></textarea></label>
      <label class="inline"><input type="checkbox" bind:checked={form.summarize} /> Ringkas output saat agent keluar</label>
      <label class="inline"><input type="checkbox" bind:checked={form.notify} /> Notifikasi untuk pane dari profil ini</label>
      <span class="acts"><button class="mini" onclick={() => (form = null)}>Batal</button> <button class="mini" onclick={saveForm}>Simpan</button></span>
    </div>
  {/if}
{/snippet}

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
        </section>
        <h4>Notifikasi</h4>
        <section>
          <div class="row">
            <span>Agent butuh izin<small>Notifikasi, badge Dock, dan Dock melompat saat jendela tidak dilihat; juga pane yang menunggu password. Isinya nama pane dan status saja</small></span>
            <input type="checkbox" role="switch" aria-label="Agent butuh izin" checked={cfg.values.notify.on_blocked} onchange={(e) => setNotify("on_blocked", e.currentTarget.checked)} />
          </div>
          <div class="row">
            <span>Agent atau perintah lama selesai</span>
            <input type="checkbox" role="switch" aria-label="Selesai" checked={cfg.values.notify.on_done} onchange={(e) => setNotify("on_done", e.currentTarget.checked)} />
          </div>
          <div class="row">
            <span>Suara</span>
            <input type="checkbox" role="switch" aria-label="Suara notifikasi" checked={cfg.values.notify.sound} onchange={(e) => setNotify("sound", e.currentTarget.checked)} />
          </div>
          <div class="row">
            <span>Batas perintah lama<small>Perintah shell lebih lama dari ini lalu selesai dianggap perlu dilihat</small></span>
            <select aria-label="Batas perintah lama" value={cfg.values.notify.long_command_secs} onchange={(e) => setNotify("long_command_secs", +e.currentTarget.value)}>
              {#each [10, 30, 60, 120, 300] as n}<option value={n}>{n} detik</option>{/each}
            </select>
          </div>
        </section>
        <h4>Keluar</h4>
        <section>
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
              <span class="grow"
                >{p.name}<small
                  >{p.command ? [p.command, ...p.args].join(" ") : "$SHELL -l"} · folder {p.cwd || "ikut pane yang fokus"}{p.env.length
                    ? ` · env ${p.env.join(", ")}`
                    : ""}{p.secret_env.length ? ` · rujukan ${p.secret_env.map((x) => x.name).join(", ")}` : ""}</small
                ></span
              >
              <span class="acts">
                <button class="mini" onclick={() => edit(p)}>Ubah</button>
                <button class="mini" onclick={() => edit(p, true)}>Duplikat</button>
                <button class="mini" onclick={() => resetProfile(p.id)} title={BUILTIN.includes(p.id) ? "Hapus ubahan di config" : "Hapus profil"}
                  >{BUILTIN.includes(p.id) ? "Kembalikan" : "Hapus"}</button
                >
              </span>
            </div>
            {#if form && form.id === p.id && !form.isNew}{@render editor()}{/if}
          {/each}
          {#if form?.isNew}{@render editor()}{/if}
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
            <div class="row">
              <span class="grow">{label}<small class="m">{id}</small></span>
              {#if keyEdit?.id === id}
                <!-- svelte-ignore a11y_autofocus -->
                <input class="keyin" bind:value={keyEdit.combo} autofocus aria-label="Kombinasi untuk {label}" spellcheck="false" placeholder="mis. cmd+shift+d"
                  onkeydown={(e) => {
                    if (e.key === "Enter") saveKey(id, keyEdit?.combo ?? "");
                    if (e.key === "Escape") (e.stopPropagation(), (keyEdit = null));
                  }} />
              {:else}
                <kbd>{key ? pretty(key) : "—"}</kbd>
              {/if}
              <span class="acts">
                <button class="mini" onclick={() => (keyEdit = { id, combo: key.toLowerCase() })}>Ubah</button>
                <button class="mini" title="Kembali ke pintasan bawaan" onclick={() => saveKey(id, "")}>Bawaan</button>
              </span>
            </div>
          {/each}
          <div class="row"><span>Salin / tempel</span><kbd>⌘C ⌘V</kbd></div>
          <div class="row"><span>Pengaturan</span><kbd>⌘,</kbd></div>
          <div class="row"><span>Shift+Tab, Ctrl, dan tombol lain</span><span class="m">diteruskan ke terminal</span></div>
        </section>
        <p class="note">
          ⌘ milik aplikasi; tombol lain milik terminal, jadi kombinasi wajib memakai <code>cmd</code>. "Ubah" menulis ke bagian
          <code>[keys]</code> di config.toml (mis. <code>"cmd+t" = "pane.split.right"</code>); berlaku tanpa restart, bentrok ditandai di atas.
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
          <div class="row"><span>App, daemon, protokol</span><span class="m">{version} · {daemon?.daemon ?? "?"} · {daemon?.proto ?? "?"}</span></div>
          <div class="row">
            <span>Rilis baru<small>Dibuka di browser; app tidak mengecek sendiri</small></span>
            <button class="mini" onclick={() => invoke("open_url", { url: "https://github.com/AHanif52/gembala/releases" })}>Halaman rilis</button>
          </div>
          <div class="row"><span>File config</span><span class="m">{cfg.path}</span></div>
          <div class="row"><span>Status config</span><span class="m">{cfg.errors.length ? `${cfg.errors.length} masalah, lihat atas` : "bersih"}</span></div>
          <div class="row"><span>Folder socket<small>Bukan symlink, milik kamu, izin 0700; dicek daemon saat start</small></span><span class="m">$TMPDIR/gembala</span></div>
        </section>
        <h4>Agent per pane</h4>
        <section>
          {#each Object.values(meta) as m}
            <div class="row">
              <span>{m.name || m.profile}<small>{["claude", "codex", "opencode", "omp"].includes(m.agent) ? "status dari hook resmi" : m.agent === "custom" ? "status dari pola layar" : "shell; agent yang diketik manual dibaca dari pola layar"}</small></span>
              <span class="m">{m.status || "-"}</span>
            </div>
          {:else}
            <div class="row"><span>Belum ada pane</span></div>
          {/each}
        </section>
        <h4>Bantuan</h4>
        <section>
          <div class="row">
            <span>Salin info untuk laporan bug<small>Versi dan status pane saja; env, folder, dan isi layar tidak ikut</small></span>
            <span><span class="m">{copied}</span> <button class="mini" onclick={copyInfo}>Salin</button></span>
          </div>
          {#if resetAsk}
            <div class="row" role="alert">
              <span>Hapus config, tata letak, hook, dan rahasia Keychain milik Gembala, lalu hentikan daemon dan semua pane? Data agent tidak disentuh.</span>
              <span><button class="mini" onclick={() => (resetAsk = false)}>Batal</button> <button class="mini" onclick={reset}>Hapus</button></span>
            </div>
          {:else}
            <div class="row">
              <span>Reset Gembala<small>Hapus semua data Gembala; data agent tidak disentuh</small></span>
              <button class="mini" onclick={() => (resetAsk = true)}>Reset…</button>
            </div>
          {/if}
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
  .keyin {
    width: 150px;
    padding: 3px 6px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: 12px "SF Mono", Menlo, monospace;
  }
  .grow {
    min-width: 0;
  }
  .acts {
    display: flex;
    gap: 4px;
    flex-shrink: 0;
  }
  .form {
    display: grid;
    gap: 8px;
    padding: 10px 12px;
    border-top: 1px solid var(--line);
    background: var(--bg);
  }
  .form label {
    display: grid;
    gap: 4px;
    font-size: 12px;
    color: var(--muted);
  }
  .form label.inline {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .form input:not([type]),
  .form textarea {
    padding: 5px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--card);
    color: var(--fg);
    font: 12px "SF Mono", Menlo, monospace;
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
