<script lang="ts">
  // Pengaturan (FR-42): layar dan config.toml setara. Tiap ubahan ditulis daemon ke file (satu penulis),
  // lalu kembali sebagai `config.changed`, jadi layar selalu mencerminkan isi file.
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getVersion } from "@tauri-apps/api/app";
  import { call } from "./daemon";
  import { envRows, envPatch, type EnvRow } from "./env";
  import { pretty, type Account, type Agent, type Config, type Profile, type Ui } from "./config";

  let {
    cfg,
    meta,
    onclose,
    onquitall,
    onlogin,
  }: {
    cfg: Config;
    meta: Record<number, { name: string; agent: string; profile: string; status?: string }>;
    onclose: () => void;
    onquitall: () => void;
    onlogin: (id: string, name: string) => void; // FR-61: buka pane akun itu untuk login
  } = $props();

  // FR-61: akun Claude Code (folder config sendiri lewat CLAUDE_CONFIG_DIR); Utama selalu ada
  type AccRow = { account: Account & { main?: boolean }; status: { used: boolean; email: string | null } };
  const SHARE_DEFAULT = ["CLAUDE.md", "settings.json", "skills", "commands", "agents", "plugins"];
  let accRows = $state<AccRow[]>([]);
  let shareOpts = $state<string[]>([]);
  let accForm = $state({ name: "", dir: "", share: [...SHARE_DEFAULT], error: "", info: "" });
  let accOpen = $state(false); // form tertutup; akun Utama sudah jadi bawaan
  let removeAsk = $state("");
  let accErr = $state("");
  const slug = (n: string) => n.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 32);
  async function loadAccounts() {
    const r = await call("account.list", {}).catch(() => null);
    if (r) [accRows, shareOpts, models, efforts] = [r.accounts, r.share, r.models ?? [], r.efforts ?? []];
  }
  // FR-61 (8) varian A: setelan sendiri (model, effort), Ikut Utama/Sendiri per butir, dan yang selalu milik akun
  let models = $state<string[]>([]);
  let efforts = $state<string[]>([]);
  let accDetail = $state(""); // id akun yang halamannya terbuka
  let ownAsk = $state(""); // "<id>:<butir>" menunggu konfirmasi Sendiri untuk settings.json/plugins
  const FOLLOW: { label: string; sub: string; items: string[] }[] = [
    { label: "Instruksi global", sub: "CLAUDE.md", items: ["CLAUDE.md"] },
    { label: "Pengaturan Claude", sub: "settings.json: hook, izin, plugin aktif (termasuk MCP bersama)", items: ["settings.json"] },
    { label: "Skill, perintah, subagent", sub: "skills, commands, agents", items: ["skills", "commands", "agents"] },
    { label: "Plugin dan MCP bersama", sub: "plugins", items: ["plugins"] },
    { label: "Riwayat sesi dan memori proyek", sub: "projects: /resume dan lanjut sesi lintas akun; Sendiri = mulai kosong", items: ["projects"] },
  ];
  async function updateAccount(params: Record<string, unknown>) {
    const r = await call("account.update", params).catch((e) => ({ error: String(e) }));
    accErr = r?.error ?? "";
    loadAccounts();
  }
  async function setFollow(a: Account, items: string[], follow: boolean) {
    const key = `${a.id}:${items[0]}`;
    // settings.json/plugins jadi Sendiri: hook, izin, plugin, MCP bersama berhenti ikut Utama, jadi tanya dulu
    if (!follow && (items.includes("settings.json") || items.includes("plugins")) && ownAsk !== key) return void (ownAsk = key);
    ownAsk = "";
    for (const item of items) await updateAccount({ id: a.id, item, follow });
  }
  $effect(() => {
    void cfg.values.accounts; // muat ulang saat config berubah
    loadAccounts();
  });
  async function createAccount() {
    const r = await call("account.create", { name: accForm.name.trim(), dir: accForm.dir.trim() || undefined, share: accForm.share }).catch((e) => ({ error: String(e) }));
    if (r.error && !r.id) return void (accForm.error = r.error);
    accForm = { name: "", dir: "", share: [...SHARE_DEFAULT], error: r.error ?? "", info: r.skipped?.length ? `Sudah ada, tidak ditimpa: ${r.skipped.join(", ")}` : "" };
    accOpen = !!(accForm.error || accForm.info);
    loadAccounts();
  }
  async function removeAccount(id: string) {
    if (removeAsk !== id) return void (removeAsk = id);
    removeAsk = "";
    const r = await call("account.remove", { id }).catch((e) => ({ error: String(e) }));
    accErr = r?.error ?? "";
    loadAccounts();
  }
  let daemon = $state<{ daemon: string; proto: number; msg?: { rate_per_min: number; hop_max: number } } | null>(null);
  // FR-29: ubah profil dari layar; satu baris = satu argumen / satu env
  type Form = {
    id: string;
    name: string;
    agent: string;
    command: string;
    args: string;
    cwd: string;
    env: EnvRow[];
    secret: string;
    summarize: boolean;
    notify: boolean;
    permission: string;
    policy: string; // SEC-5 / msg.rs Policy: auto, confirm, deny
    account: string;
    isNew: boolean;
  };
  let form = $state<Form | null>(null);
  const BUILTIN = ["shell", "claude", "codex", "opencode", "omp"];

  function edit(p: Profile, copy = false) {
    let id = p.id;
    if (copy) for (let n = 2; cfg.values.profiles.some((x) => x.id === id); n++) id = `${p.id}-${n}`;
    form = {
      id,
      name: copy ? `${p.name} ${id.split("-").at(-1)}` : p.name,
      agent: p.agent,
      account: p.account ?? "",
      command: p.command,
      args: p.args.join("\n"),
      cwd: p.cwd,
      env: envRows(p.env), // nilai env tidak dikirim ke UI; isi ulang bila ingin mengubah
      secret: p.secret_env.map((x) => `${x.name}=${x.ref}`).join("\n"),
      summarize: p.summarize,
      notify: p.notify,
      permission: p.permission ?? "default",
      policy: p.policy ?? "auto",
      isNew: copy,
    };
  }

  /// Gembala.pen 30: "+ Profil" menambah profil kosong; id-nya dibuat unik karena id dipakai di config.toml
  function blank() {
    let id = "profil";
    for (let n = 2; cfg.values.profiles.some((x) => x.id === id); n++) id = `profil-${n}`;
    form = {
      id,
      name: "Profil baru",
      agent: "shell",
      account: "",
      command: "",
      args: "",
      cwd: "",
      env: [],
      secret: "",
      summarize: true,
      notify: true,
      permission: "default",
      policy: "auto",
      isNew: true,
    };
  }

  const pairs = (text: string) =>
    Object.fromEntries(text.split("\n").map((l) => l.trim()).filter((l) => l.includes("=")).map((l) => [l.slice(0, l.indexOf("=")).trim(), l.slice(l.indexOf("=") + 1).trim()]));

  async function saveForm() {
    if (!form) return;
    failed = "";
    // hanya env yang diisi; yang dibiarkan kosong tetap memakai nilai lama di file
    const profile = {
      id: form.id.trim(),
      name: form.name.trim(),
      command: form.command.trim(),
      args: form.args.split("\n").map((a) => a.trim()).filter(Boolean),
      cwd: form.cwd.trim(),
      env: envPatch(form.env),
      secret_env: pairs(form.secret),
      summarize: form.summarize,
      notify: form.notify,
      permission: form.permission,
      policy: form.policy,
      account: form.account,
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
    ["toolbar", "Toolbar"],
    ["keamanan", "Keamanan"],
    ["proses", "Proses"],
    ["diagnostik", "Diagnostik"],
  ] as const;
  // FR-60: proses milik Gembala, diperbarui tiap 2 detik selama bagian ini terbuka
  type Proc = { pid: number; ppid: number; rss_kb: number; cpu: number; role: string; name: string };
  const ROLE: Record<string, [string, string]> = {
    ui: ["Jendela Gembala", "Proses utama aplikasi"],
    "webkit-content": ["Tampilan", "Menggambar terminal dan sidebar; di Activity Monitor bernama tauri://localhost"],
    "webkit-gpu": ["Grafis", "Bagian WebKit yang menggambar ke layar"],
    "webkit-net": ["Pemuat tampilan", "Bagian bawaan WebKit untuk memuat tampilan dari dalam app; koneksi ke internet ditolak CSP"],
    daemon: ["Penjaga pane", "Menjalankan shell dan agent; tetap hidup saat jendela ditutup hanya bila masih ada yang jalan"],
  };
  let procs = $state<Proc[]>([]);
  let panePid = $state<Record<number, string>>({});
  let stopAsk = $state(false);
  async function loadProcs() {
    procs = await invoke<Proc[]>("proc_list").catch(() => []);
    const ids: number[] = await call("pane.list").catch(() => []);
    const infos = await Promise.all(ids.map((pane) => call("pane.info", { pane }).catch(() => null)));
    panePid = Object.fromEntries(infos.filter((i) => i?.pid).map((i) => [i.pid, i.pane_name]));
  }
  $effect(() => {
    if (sec !== "proses") return;
    loadProcs();
    const t = setInterval(loadProcs, 2000);
    return () => clearInterval(t);
  });
  /// nama pane dari shell induk terdekat
  function paneOf(p: Proc): string {
    const by = new Map(procs.map((x) => [x.pid, x]));
    for (let q: Proc | undefined = p; q; q = by.get(q.ppid)) if (panePid[q.pid]) return panePid[q.pid];
    return "";
  }
  const mb = (kb: number) => `${Math.round(kb / 1024)} MB`;
  // satu baris per pane (total RAM/CPU semua prosesnya), rincian dibuka per pane; pane berisi shell saja di bawah
  const SHELLS = ["zsh", "bash", "fish", "sh", "login"];
  const AGENT_BIN = ["claude", "codex", "opencode", "omp"];
  const paneGroups = $derived.by(() => {
    const g = new Map<string, Proc[]>();
    for (const p of procs.filter((p) => p.role === "pane")) {
      const k = paneOf(p) || "pane lain";
      g.set(k, [...(g.get(k) ?? []), p]);
    }
    return [...g].map(([name, ps]) => {
      const run = ps.find((p) => AGENT_BIN.includes(p.name))?.name ?? ps.find((p) => !SHELLS.includes(p.name))?.name;
      return { name, ps, run: run ?? "shell (diam)", idle: !run, rss: ps.reduce((s, p) => s + p.rss_kb, 0), cpu: ps.reduce((s, p) => s + p.cpu, 0) };
    }).sort((a, b) => +a.idle - +b.idle || b.rss - a.rss);
  });
  let openGroups = $state<string[]>([]);
  let sec = $state<(typeof SECTIONS)[number][0]>("umum");
  let failed = $state("");
  let keys = $state<[string, string, string][]>([]);
  let keyErrors = $state<string[]>([]);
  let keyEdit = $state<{ id: string; combo: string } | null>(null);
  // FR-55: editor toolbar agent ala Warp (seret antar sisi, × buang, daftar tersedia); ditulis ke [toolbar]
  const CHIP_NAME: Record<string, string> = {
    attach: "＋ Lampirkan file",
    diff: "▤ Statistik diff",
    rich_input: "▭ Rich Input",
    cwd: "▢ Folder kerja",
    branch: "⎇ Branch git",
    settings: "⚙ Pengaturan",
  };
  const TB_DEFAULT = { left: ["attach", "diff", "rich_input"], right: ["cwd", "branch", "settings"] };
  type Zone = "left" | "right";
  const tb = $derived({ left: [...(cfg.values.toolbar?.left ?? TB_DEFAULT.left)], right: [...(cfg.values.toolbar?.right ?? TB_DEFAULT.right)] });
  const spare = $derived(Object.keys(CHIP_NAME).filter((c) => !tb.left.includes(c) && !tb.right.includes(c)));
  async function saveTb(next: Record<Zone, string[]>) {
    failed = "";
    for (const z of ["left", "right"] as Zone[]) {
      const r = await call("config.set", { section: "toolbar", key: z, value: next[z] }).catch((e) => ({ error: String(e) }));
      if (r.error) failed = r.error;
    }
  }
  let tbDrag = $state<{ z: Zone; i: number } | null>(null);
  /// seret chip: pointer (drag HTML bentrok dengan drop file Tauri), target = chip atau zona di bawah kursor
  function tbStart(e: PointerEvent, z: Zone, i: number) {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".tbx")) return;
    tbDrag = { z, i };
    const up = (u: PointerEvent) => {
      window.removeEventListener("pointerup", up);
      const from = tbDrag;
      tbDrag = null;
      const hit = document.elementFromPoint(u.clientX, u.clientY)?.closest<HTMLElement>("[data-tbz]");
      if (!from || !hit) return;
      const to = hit.dataset.tbz as Zone;
      const at = hit.dataset.tbi ? +hit.dataset.tbi : tb[to].length;
      const next = { left: [...tb.left], right: [...tb.right] };
      const [chip] = next[from.z].splice(from.i, 1);
      next[to].splice(from.z === to && at > from.i ? at - 1 : at, 0, chip);
      saveTb(next);
    };
    window.addEventListener("pointerup", up);
  }

  // urutan dan nama sama dengan menu app (lib.rs)
  const GROUPS: [string, string][] = [["tab.", "Tab"], ["pane.", "Pane"], ["term.", "Tampilan"], ["msg.", "Pesan"]];

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
  // Gembala.pen 31: halaman akun, dibuka dari baris Claude Code; nav Agent kembali ke halaman induk
  let agentPage = $state<"main" | "akun">("main");
  /// Jenis hook yang dipasang daemon (core/src/profile.rs `hook_files`); sumber status pane tiap agent.
  const HOOK: Record<string, string> = {
    claude: "hook resmi",
    codex: "hook notify",
    omp: "hook omp-gembala.ts",
    opencode: "plugin opencode-gembala.js",
  };
  /// Baris Akun ditampilkan untuk profil yang menjalankan Claude: agent claude, atau perintahnya claude.
  const runsClaude = (f: Form) => f.agent === "claude" || /^claude(\s|-|$)/.test(f.command.trim());
  /// env: satu baris = satu NAMA dan nilai; baris tanpa nilai dibuang saat simpan (nilai lama tetap)
  function envAdd() {
    if (form) form.env = [...form.env, { name: "", value: "" }];
  }
  function envDel(i: number) {
    if (form) form.env = form.env.filter((_, j) => j !== i);
  }

  /// FR-28: deteksi lewat login shell; "Periksa ulang" memaksa deteksi baru
  async function detect(refresh: boolean) {
    detecting = true;
    agents = await call("agents.detect", { refresh }).catch(() => []);
    detecting = false;
  }
  const ui = $derived(cfg.values.ui);
  /// Gembala.pen 36: angka rem datang dari daemon (core/src/msg.rs lewat daemon.info), bukan diketik ulang di markup
  const rate = $derived(daemon?.msg?.rate_per_min);
  const hops = $derived(daemon?.msg?.hop_max);
  /// Folder log audit ada di sebelah config.toml (core/src/daemon.rs Audit::open); daemon yang menentukan path-nya.
  const auditDir = $derived(cfg.path.replace(/\/[^/]+$/, "") + "/audit");
  /// Pintasan tombol darurat ikut [keys]; cadangannya sama dengan MSG_KEYS di ui/src-tauri/src/lib.rs.
  const stopKey = $derived(pretty(keys.find(([id]) => id === "msg.stop")?.[2] || "Cmd+Shift+."));
  /// Folder di sebelah config.toml dibuka dari Finder; daftar yang boleh ada di REVEAL (ui/src-tauri/src/lib.rs).
  async function reveal(sub: string) {
    failed = "";
    const r = await invoke("reveal_dir", { config: cfg.path, sub }).catch((e) => String(e));
    if (typeof r === "string" && r) failed = r;
  }

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
    {@const claude = runsClaude(form)}
    <!-- FR-29 + SEC-5: kartu profil terbuka, gaya baris Gembala.pen 30 -->
    <div class="pcard">
      <div class="row">
        <span class="grow">Nama</span>
        <input class="tin" bind:value={form.name} aria-label="Nama profil" />
      </div>
      {#if form.isNew}
        <div class="row">
          <span class="grow">Id<small>Huruf kecil, angka, dan tanda minus; dipakai di config.toml</small></span>
          <input class="tin mono" bind:value={form.id} spellcheck="false" aria-label="Id profil" />
        </div>
      {/if}
      <div class="row">
        <span class="grow">Perintah</span>
        <input class="tin mono" bind:value={form.command} placeholder="kosong = shell" spellcheck="false" aria-label="Perintah" />
      </div>
      <div class="row col">
        <span>Argumen<small>satu per baris</small></span>
        <textarea class="tin mono" bind:value={form.args} rows="2" spellcheck="false" aria-label="Argumen"></textarea>
      </div>
      <div class="row">
        <span class="grow">Folder<small>kosong = ikut pane yang fokus</small></span>
        <input class="tin mono" bind:value={form.cwd} placeholder="kosong = ikut pane yang fokus" spellcheck="false" aria-label="Folder" />
      </div>
      {#if claude && cfg.values.accounts?.length}
        <div class="row">
          <span class="grow">Akun<small>Dipakai bila tidak dipilih lain saat membuka pane (FR-61)</small></span>
          <select aria-label="Akun bawaan profil" bind:value={form.account}
            ><option value="">Utama</option>{#each cfg.values.accounts.filter((a) => a.agent === "claude") as a (a.id)}<option value={a.id}>{a.name}</option>{/each}</select
          >
        </div>
      {/if}
      <div class="row">
        <span class="grow">Kebijakan penerima<small>Pesan dari agent lain ke pane ini: otomatis jalan, tanya dulu, atau tolak</small></span>
        <span class="seg" role="group" aria-label="Kebijakan penerima">
          {#each [["auto", "otomatis"], ["confirm", "konfirmasi"], ["deny", "tolak"]] as [v, t]}
            <button class:on={form.policy === v} onclick={() => form && (form.policy = v)}>{t}</button>
          {/each}
        </span>
      </div>
      <div class="row">
        <span class="grow">Mode izin<small>Gembala hanya menambah flag launch; izin tetap dijaga agent. OpenCode belum punya flag.</small></span>
        <span class="seg" role="group" aria-label="Mode izin agent">
          {#each [["default", "Bawaan agent"], ["auto", "Auto"]] as [v, t]}
            <button class:on={form.permission === v} onclick={() => form && (form.permission = v)}>{t}</button>
          {/each}
        </span>
      </div>
      <div class="row">
        <span class="grow">Ringkas output saat agent keluar</span>
        <input type="checkbox" role="switch" bind:checked={form.summarize} aria-label="Ringkas output saat agent keluar" />
      </div>
      <div class="row">
        <span class="grow">Notifikasi untuk pane dari profil ini</span>
        <input type="checkbox" role="switch" bind:checked={form.notify} aria-label="Notifikasi untuk pane dari profil ini" />
      </div>
      <div class="row col">
        <span>Env<small>Nama dan nilai; nilai tidak ditampilkan ulang, isi lagi bila ingin mengubah</small></span>
        {#each form.env as e, i (i)}
          <div class="envrow">
            <input class="tin mono" bind:value={e.name} placeholder="NAMA" spellcheck="false" aria-label="Nama env {i + 1}" />
            <input class="tin mono" bind:value={e.value} placeholder="nilai" spellcheck="false" aria-label="Nilai env {i + 1}" />
            <button class="mini" aria-label="Buang env {e.name || i + 1}" onclick={() => envDel(i)}>×</button>
          </div>
        {/each}
        <button class="mini addrow" onclick={envAdd}>+ Env</button>
      </div>
      <div class="row col">
        <span>Rujukan rahasia<small><code>NAMA=op://vault/item/field</code> atau <code>NAMA=keychain:service/akun</code></small></span>
        <textarea class="tin mono" bind:value={form.secret} rows="2" spellcheck="false" aria-label="Rujukan rahasia"></textarea>
      </div>
      <div class="row foot">
        {#if !form.isNew}
          <button class="link" onclick={() => form && resetProfile(form.id)}>{BUILTIN.includes(form.id) ? "Kembalikan ke bawaan" : "Hapus profil"}</button>
        {/if}
        <span class="acts">
          <button class="mini" onclick={() => (form = null)}>Batal</button>
          <button class="mini pri" onclick={saveForm}>Simpan</button>
        </span>
      </div>
    </div>
  {/if}
{/snippet}

<div class="veil" role="presentation" onpointerdown={(e) => e.target === e.currentTarget && onclose()}>
  <div class="modal" role="dialog" aria-label="Pengaturan">
    <nav>
      {#each SECTIONS as [k, t]}
        <button class:on={k === sec} onclick={() => ((sec = k), (agentPage = "main"))}>{t}</button>
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
            <span>Saat ⌘Q<small>Biarkan jalan = app ditutup, agent lanjut di latar dan muncul lagi saat app dibuka</small></span>
            <select aria-label="Saat ⌘Q" value={ui.on_quit} onchange={(e) => set("on_quit", e.currentTarget.value as Ui["on_quit"])}>
              <option value="keep">Biarkan agent jalan</option>
              <option value="stop">Hentikan semua</option>
            </select>
          </div>
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
              {#each [...new Set([ui.font_family, "Hack", "SF Mono", "Menlo", "Monaco"])] as f}<option>{f}</option>{/each}
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
            <span>Option sebagai Meta<small>Yang bukan Meta mengetik simbol (mis. Option+Shift+2 = €)</small></span>
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
          <div class="row">
            <span>Input editor<small>Kotak ketik ala Warp menggantikan prompt shell (zsh); berlaku untuk pane baru</small></span>
            <input type="checkbox" role="switch" aria-label="Input editor" checked={ui.input_editor} onchange={(e) => set("input_editor", e.currentTarget.checked)} />
          </div>
        </section>
      {:else if sec === "agent"}
        {#if agentPage === "akun"}
          <!-- Gembala.pen 31: halaman akun dibuka dari baris agent; nav Agent kembali ke halaman induk -->
          <button class="back" onclick={() => (agentPage = "main")}>‹ Agent</button>
          <h4>Akun Claude Code<span class="hint">tiap akun punya folder dan login sendiri-sendiri</span>{#if !accOpen}<button class="mini" onclick={() => (accOpen = true)}>+ Tambah akun</button>{/if}</h4>
        <section>
          {#each accRows as r (r.account.id)}
            <div class="row">
              <span class="grow"
                ><span class="name">{r.account.name}{#if r.account.main}<span class="badge">bawaan</span>{/if}</span><small
                  >{r.status.email ?? "email tidak terbaca"} · {r.account.dir}{r.account.model || r.account.effort
                    ? ` · ${[r.account.model, r.account.effort].filter(Boolean).join(" · ")}`
                    : ""}{r.account.main ? " · nggak bisa dihapus" : ""}</small
                ></span
              >
              <span class="acts">
                {#if !r.status.used}<button class="mini" onclick={() => onlogin(r.account.id, r.account.name)}>Login</button>{/if}
                {#if !r.account.main}
                  <button class="mini pri" aria-expanded={accDetail === r.account.id} onclick={() => (accDetail = accDetail === r.account.id ? "" : r.account.id)}>{accDetail === r.account.id ? "Tutup" : "Ubah"}</button>
                  <button class="mini" onclick={() => removeAccount(r.account.id)}>{removeAsk === r.account.id ? "Yakin?" : "Hapus"}</button>
                {/if}
                <span class="state">{r.status.used ? "sudah dipakai" : "belum login"}</span>
              </span>
            </div>
            {#if accDetail === r.account.id}
              {@const a = r.account}
              <!-- FR-61 (8), mockup-akun-setelan varian A -->
              <div class="accd">
                <span class="grp">Setelan sendiri <small>dipakai tiap Claude akun ini dijalankan dari Gembala</small></span>
                <div class="row sub">
                  <span class="grow">Model<small>Bawaan = ikut settings.json</small></span>
                  <select aria-label="Model akun {a.name}" value={a.model ?? ""} onchange={(e) => updateAccount({ id: a.id, model: e.currentTarget.value })}>
                    <option value="">Bawaan</option>{#each models as m (m)}<option value={m}>{m}</option>{/each}
                  </select>
                </div>
                <div class="row sub">
                  <span class="grow">Effort</span>
                  <select aria-label="Effort akun {a.name}" value={a.effort ?? ""} onchange={(e) => updateAccount({ id: a.id, effort: e.currentTarget.value })}>
                    <option value="">Bawaan</option>{#each efforts as x (x)}<option value={x}>{x}</option>{/each}
                  </select>
                </div>
                <span class="grp">Ikut Utama <small>satu sumber; ubah di Utama, akun ini ikut. Sendiri = mulai dari salinan isi Utama saat ini</small></span>
                {#each FOLLOW as f (f.label)}
                  {@const on = f.items.every((i) => a.share.includes(i))}
                  <div class="row sub">
                    <span class="grow">{f.label}<small>{f.sub}</small></span>
                    <span class="seg" role="group" aria-label={f.label}>
                      <button class:on onclick={() => !on && setFollow(a, f.items, true)}>Ikut Utama</button>
                      <button class:on={!on} onclick={() => on && setFollow(a, f.items, false)}>{ownAsk === `${a.id}:${f.items[0]}` ? "Yakin?" : "Sendiri"}</button>
                    </span>
                  </div>
                  {#if ownAsk === `${a.id}:${f.items[0]}`}<span class="m">Hook, izin, plugin aktif, dan MCP bersama tidak lagi ikut Utama (mulai dari salinan sekarang). Klik Yakin? untuk lanjut</span>{/if}
                {/each}
                <div class="row sub">
                  <span class="grow">Folder dipercaya<small>disalin dari Utama sebelum Claude jalan (satu arah)</small></span>
                  <span class="seg" role="group" aria-label="Folder dipercaya">
                    <button class:on={a.trust !== false} onclick={() => a.trust === false && updateAccount({ id: a.id, trust: true })}>Ikut Utama</button>
                    <button class:on={a.trust === false} onclick={() => a.trust !== false && updateAccount({ id: a.id, trust: false })}>Sendiri</button>
                  </span>
                </div>
                <span class="grp">Selalu milik akun ini <small>tidak bisa dibagi</small></span>
                <div class="row sub"><span class="grow">🔒 Login dan token<small>disimpan Claude di Keychain; Gembala tidak membaca</small></span></div>
                <div class="row sub"><span class="grow">🔒 Connector claude.ai<small>Google, Figma, Docs: tersambung ke akun claude.ai masing-masing</small></span></div>
                <div class="row sub"><span class="grow">🔒 Riwayat prompt (panah atas)<small>history.jsonl per akun</small></span></div>
              </div>
            {/if}
          {/each}
          {#if removeAsk}<span class="m">Sesi dan pengaturan akun itu hilang; isi Utama aman. Login tetap di Keychain: /logout dulu bila ingin dicabut</span>{/if}
          {#if accErr}<span class="m err" role="alert">{accErr}</span>{/if}
          {#if accOpen}
          <div class="form">
            <label>Tambah akun · nama <input bind:value={accForm.name} placeholder="mis. Kantor" /></label>
            <label>Folder <small>Di bawah folder home; dibuat dengan izin 0700</small><input bind:value={accForm.dir} placeholder={`~/.claude-${slug(accForm.name) || "nama"}`} spellcheck="false" /></label>
            <span class="lbl">Bawa dari Utama <small>Tautan, bukan salinan. Login, token, dan MCP pengguna tidak pernah dibawa; MCP diatur per akun (claude mcp add di pane akun itu) atau per proyek lewat .mcp.json</small></span>
            {#each shareOpts as s (s)}
              <label class="inline"><input type="checkbox" role="switch" checked={accForm.share.includes(s)} onchange={(e) => (accForm.share = e.currentTarget.checked ? [...accForm.share, s] : accForm.share.filter((x) => x !== s))} /> {s === "projects" ? "projects (memori + riwayat sesi)" : s}</label>
            {/each}
            {#if accForm.error}<span class="m err" role="alert">{accForm.error}</span>{/if}
            {#if accForm.info}<span class="m">{accForm.info}</span>{/if}
            <span class="acts"><button class="mini" onclick={() => (accOpen = false)}>Batal</button><button class="mini" disabled={!accForm.name.trim()} onclick={createAccount}>Buat akun</button></span>
          </div>
          {/if}
        </section>
        {:else}
          <h4>Terpasang<span class="hint">dicari lewat login shell</span>
            <button class="mini" disabled={detecting} onclick={() => detect(true)}>{detecting ? "Memeriksa…" : "Periksa ulang"}</button></h4>
          <section>
            {#each agents as a (a.agent)}
              <div class="row">
                <span
                  >{a.name}<small
                    >{a.installed
                      ? `${a.version ?? "versi tidak terbaca"} · ${a.tested ? "teruji" : "belum dites, statusnya ditebak dari layar"}${HOOK[a.agent] ? ` · ${HOOK[a.agent]}` : ""}`
                      : `Tidak ditemukan di PATH login shell. Pasang: ${a.install}`}</small
                  ></span
                >
                <span class="acts">
                  {#if a.agent === "claude" && a.installed}<button class="mini" onclick={() => (agentPage = "akun")}>Akun ({accRows.length})…</button>{/if}
                  <span class="state" class:warn={a.installed && !a.tested}>{a.installed ? (a.tested ? "✓ terpasang" : "! belum diuji") : "✗ tidak ada"}</span>
                </span>
              </div>
            {:else}
              <div class="row"><span>Memeriksa…</span></div>
            {/each}
          </section>
          <h4>Profil<button class="mini" onclick={blank}>+ Profil</button></h4>
          <section>
            {#each cfg.values.profiles as p (p.id)}
              {@const open = !!form && form.id === p.id && !form.isNew}
              <div class="row">
                <span class="grow"
                  >{p.name}<small
                    >{[p.command ? [p.command, ...p.args].join(" ") : "$SHELL -l", `folder ${p.cwd || "ikut pane yang fokus"}`, p.agent === "shell" ? "" : p.policy]
                      .filter(Boolean)
                      .join(" · ")}</small
                  ></span
                >
                <span class="acts">
                  {#if open}
                    <button class="mini pri" onclick={() => (form = null)}>Tutup</button>
                  {:else}
                    <button class="mini" onclick={() => edit(p)}>Ubah</button>
                    <button class="mini" onclick={() => edit(p, true)}>Duplikat</button>
                  {/if}
                </span>
              </div>
              {#if open}{@render editor()}{/if}
            {/each}
            {#if form?.isNew}{@render editor()}{/if}
          </section>
          <p class="note">
            Ubah profil pas pane-nya lagi jalan? Kepakenya di pane baru; pane lama tetap pakai isian lama sampai ditutup. Id yang sama
            menimpa bawaan di <code>[[profile]]</code>; rahasia hanya sebagai rujukan <code>op://…</code> atau
            <code>keychain:service/akun</code> di <code>secret_env</code>.
          </p>
        {/if}
      {:else if sec === "pintasan"}
        {#if keyErrors.length}
          <div class="errs" role="alert">
            <b>Masalah di [keys]</b>
            {#each keyErrors as e}<span>{e}</span>{/each}
          </div>
        {/if}
        <!-- FR-39: dikelompokkan seperti menu; Tab 1–9 satu baris; id aksi di tooltip -->
        {#each GROUPS as [prefix, title]}
          <h4 class="khead">{title}</h4>
          <section>
            {#each keys.filter(([id]) => id.startsWith(prefix) && !/^tab\.goto\.[2-9]$/.test(id)) as [id, label, key] (id)}
              {@const goto = id === "tab.goto.1"}
              {@const custom = Object.values(cfg.values.keys ?? {}).includes(id)}
              <div class="krow" title={id}>
                <span class="klabel">{goto ? "Pindah ke tab 1–9" : label}</span>
                {#if keyEdit?.id === id}
                  <!-- svelte-ignore a11y_autofocus -->
                  <input class="keyin" bind:value={keyEdit.combo} autofocus aria-label="Kombinasi untuk {label}" spellcheck="false" placeholder="mis. cmd+shift+d · Enter simpan"
                    onkeydown={(e) => {
                      if (e.key === "Enter") saveKey(id, keyEdit?.combo ?? "");
                      if (e.key === "Escape") (e.stopPropagation(), (keyEdit = null));
                    }} />
                {:else}
                  <span class="kacts">
                    {#if custom}<button class="link" title="Kembali ke pintasan bawaan" onclick={() => saveKey(id, "")}>Bawaan</button>{/if}
                    {#if !goto}<button class="link" onclick={() => (keyEdit = { id, combo: key.toLowerCase() })}>Ubah</button>{/if}
                  </span>
                  <kbd class:custom>{goto ? "⌘1 … ⌘9" : key ? pretty(key) : "—"}</kbd>
                {/if}
              </div>
            {/each}
          </section>
        {/each}
        <h4 class="khead">Lainnya</h4>
        <section>
          <div class="krow"><span class="klabel">Salin / tempel</span><kbd>⌘C ⌘V</kbd></div>
          <div class="krow"><span class="klabel">Pengaturan</span><kbd>⌘,</kbd></div>
          <div class="krow"><span class="klabel">Shift+Tab, Ctrl, dan tombol lain</span><span class="m">diteruskan ke terminal</span></div>
        </section>
        <p class="note">
          ⌘ milik aplikasi; tombol lain milik terminal, jadi kombinasi wajib memakai <code>cmd</code>. "Ubah" menulis ke bagian
          <code>[keys]</code> di config.toml (mis. <code>"cmd+t" = "pane.split.right"</code>); berlaku tanpa restart, bentrok ditandai di atas.
        </p>
      {:else if sec === "toolbar"}
        <!-- FR-55: seret chip untuk ubah urutan atau pindah sisi; × buang; chip tersedia bisa ditambah lagi -->
        <div class="tbzones">
          {#each [["left", "Kiri"], ["right", "Kanan"]] as [z, title] (z)}
            <div class="tbzone-wrap">
              <h4 class="khead">{title}</h4>
              <div class="tbzone" data-tbz={z} role="list" aria-label="Chip sisi {title}">
                {#each tb[z as Zone] as c, i (c)}
                  <span class="tbchip" class:lift={tbDrag?.z === z && tbDrag.i === i} data-tbz={z} data-tbi={i} role="listitem"
                    onpointerdown={(e) => tbStart(e, z as Zone, i)}>
                    {CHIP_NAME[c] ?? c}
                    <button class="tbx" aria-label="Buang {CHIP_NAME[c] ?? c}" onclick={() => saveTb({ ...tb, [z]: tb[z as Zone].filter((x) => x !== c) })}>×</button>
                  </span>
                {:else}
                  <span class="m">Kosong · seret chip ke sini</span>
                {/each}
              </div>
            </div>
          {/each}
        </div>
        <h4 class="khead">Tersedia</h4>
        <div class="tbzone spare">
          {#each spare as c (c)}
            <button class="tbchip" onclick={() => saveTb({ ...tb, left: [...tb.left, c] })}>{CHIP_NAME[c]} <span class="m">＋</span></button>
          {:else}
            <span class="m">Semua chip sudah dipakai</span>
          {/each}
        </div>
        <p class="note">
          Chip yang datanya tidak ada (mis. bukan repo git) disembunyikan di toolbar.
          <button class="link" onclick={() => saveTb(TB_DEFAULT)}>Kembalikan bawaan</button>
        </p>
      {:else if sec === "proses"}
        <p class="note">
          Ini semua proses yang dijalankan Gembala. Gembala tidak menghubungi internet (CSP hanya mengizinkan kanal internal app),
          dan socket-nya hanya bisa dibuka akun macOS ini. Cek sendiri di Activity Monitor › View › All Processes, Hierarchically.
        </p>
        <section>
          {#each procs.filter((p) => p.role !== "pane") as p (p.pid)}
            <div class="row">
              <span>{ROLE[p.role]?.[0] ?? p.role} · <code>{p.name}</code><small>{ROLE[p.role]?.[1] ?? ""}</small></span>
              <span class="m">PID {p.pid} · {mb(p.rss_kb)} · CPU {p.cpu.toFixed(1)}%</span>
            </div>
          {:else}
            <div class="row"><span>Memuat…</span></div>
          {/each}
          <div class="row">
            <span>Total Gembala<small>Tanpa proses di pane</small></span>
            <span class="m">{mb(procs.filter((p) => p.role !== "pane").reduce((s, p) => s + p.rss_kb, 0))}</span>
          </div>
        </section>
        <h4>Proses di pane <small>Total {mb(paneGroups.reduce((s, g) => s + g.rss, 0))}</small></h4>
        <section>
          {#each paneGroups as g (g.name)}
            {@const open = openGroups.includes(g.name)}
            <button class="row grp" aria-expanded={open} onclick={() => (openGroups = open ? openGroups.filter((x) => x !== g.name) : [...openGroups, g.name])}>
              <span>{open ? "▾" : "▸"} {g.name}<small>{g.run}</small></span>
              <span class="m">{mb(g.rss)} · CPU {g.cpu.toFixed(1)}%</span>
            </button>
            {#if open}
              {#each g.ps as p (p.pid)}
                <div class="row sub">
                  <span><code>{p.name}</code></span>
                  <span class="m">PID {p.pid} · {mb(p.rss_kb)} · CPU {p.cpu.toFixed(1)}%</span>
                </div>
              {/each}
            {/if}
          {:else}
            <div class="row"><span>Belum ada pane</span></div>
          {/each}
        </section>
        <section>
          {#if stopAsk}
            <div class="row" role="alert">
              <span>Hentikan daemon, semua pane, dan agent di dalamnya, lalu tutup Gembala?</span>
              <span><button class="mini" onclick={() => (stopAsk = false)}>Batal</button> <button class="mini" onclick={async () => (await call("daemon.stop").catch(() => {}), await invoke("quit"))}>Hentikan</button></span>
            </div>
          {:else}
            <div class="row">
              <span>Hentikan semua<small>Tidak ada proses Gembala yang tersisa setelah ini</small></span>
              <button class="mini" onclick={() => (stopAsk = true)}>Hentikan…</button>
            </div>
          {/if}
        </section>
      {:else if sec === "keamanan"}
        <!-- Gembala.pen 36: nilai diambil dari aturan daemon, bukan diketik ulang; pengecualian lintas workspace
             belum ada di daemon (msg.rs same_workspace), jadi tombol "+ Pengecualian" tidak dibuat -->
        <h4>Agent ngobrol sama agent</h4>
        <section>
          <div class="row">
            <span>Agent boleh nyuruh agent lain<small>Dalam workspace yang sama; beda workspace ditolak. Tiap profil punya kebijakan penerima sendiri (otomatis, konfirmasi, tolak)</small></span>
            <span class="state">sesuai kebijakan profil</span>
          </div>
          <div class="row">
            <span>Agent ngintip tab shell lu<small>Tab shell baru kebuka untuk agent kalau lu centang "boleh dibaca agent" di laci pesan (⌘J)</small></span>
            <span class="state">nggak boleh</span>
          </div>
          <div class="row">
            <span>Rem otomatis<small>{rate ?? "—"} pesan per menit per pengirim, maksimal {hops ?? "—"} lompatan antar agent</small></span>
            <span class="state">selalu nyala</span>
          </div>
          <div class="row">
            <span>Tombol darurat<small>Henti atau lanjutkan semua pesan sekaligus</small></span>
            <kbd>{stopKey}</kbd>
          </div>
          <div class="row">
            <span>Cap "dari siapa"<small>Tiap kiriman diberi label pengirim, dan agent menjawab di layar lewat id itu</small></span>
            <span class="m nowrap">[Gembala · dari shell-6]</span>
          </div>
        </section>
        <h4>Catatan kiriman<span class="hint">buat ngecek siapa ngirim apa ke siapa</span></h4>
        <section>
          <div class="row">
            <span>Disimpan di<small>Satu file per hari, isinya potongan pesan yang sudah disaring</small></span>
            <span class="m">{auditDir}</span>
          </div>
          <div class="row">
            <span>Umur catatan<small>Yang lebih tua dibuang saat daemon start</small></span>
            <span class="m">14 hari</span>
          </div>
          <div class="row">
            <span>Buka folder log</span>
            <button class="mini" onclick={() => reveal("audit")}>Buka</button>
          </div>
        </section>
        <h4>Di balik layar<span class="hint">dicek otomatis, nggak perlu diatur</span></h4>
        <section>
          <div class="row"><span>Pintu masuk agent<small>Socket cuma bisa dibuka akun macOS lu</small></span><span class="m">$TMPDIR/gembala</span></div>
          <div class="row"><span>Tombol-tombol di app<small>Setujui/Tolak cuma bisa dari jendela ini, bukan dari agent</small></span><span class="m">terkunci</span></div>
          <div class="row"><span>Terminal baru<small>Nggak kebawa jejak sesi agent lain</small></span><span class="m">bersih</span></div>
          <div class="row"><span>Link yang diklik<small>Alamat aslinya ditunjukin dulu sebelum dibuka; hanya http dan https</small></span><span class="m">tanya dulu</span></div>
          <div class="row"><span>Internet<small>App nggak ngontak server mana pun</small></span><span class="m">mati</span></div>
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
  </div>
</div>

<style>
  .veil {
    position: fixed;
    inset: 0;
    z-index: 10;
    display: grid;
    place-items: center;
    background: rgba(0, 0, 0, 0.5); /* terminal di belakang benar-benar mundur */
  }
  /* FR-39: daftar pintasan rapi: nama kiri, aksi muncul saat hover, kombinasi rata kanan di kolom tetap */
  /* FR-55: editor toolbar */
  .tbzones {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .tbzone {
    display: flex;
    flex-wrap: wrap;
    align-content: flex-start;
    gap: 6px;
    min-height: 40px;
    padding: 8px;
    border: 1px dashed var(--line);
    border-radius: 6px;
  }
  .tbzone.spare {
    border-style: solid;
  }
  .tbchip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 6px 3px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
    cursor: grab;
    user-select: none;
  }
  .tbchip.lift {
    opacity: 0.4;
  }
  .tbx {
    padding: 0 2px;
    border: 0;
    background: none;
    color: var(--tx-red, #ef4444);
    font: 13px system-ui, -apple-system, sans-serif;
    cursor: pointer;
  }
  .khead {
    margin: 14px 0 6px;
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .khead:first-of-type {
    margin-top: 0;
  }
  .krow {
    display: grid;
    grid-template-columns: 1fr auto 110px;
    align-items: center;
    gap: 10px;
    min-height: 32px;
    padding: 0 12px;
  }
  .krow + .krow {
    border-top: 1px solid var(--line);
  }
  .klabel {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .krow kbd,
  .krow > .m,
  .krow .keyin {
    grid-column: 3;
    justify-self: end;
  }
  .krow .keyin {
    grid-column: 2 / 4;
    width: 220px;
  }
  .krow kbd.custom {
    border-color: #3b82f6;
  }
  .kacts {
    display: flex;
    gap: 8px;
    visibility: hidden;
  }
  .krow:hover .kacts,
  .kacts:focus-within {
    visibility: visible;
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--muted);
    font: 12px system-ui, -apple-system, sans-serif;
    cursor: pointer;
  }
  .link:hover {
    color: var(--fg);
    text-decoration: underline;
  }
  .modal {
    display: grid;
    grid-template-columns: 132px 1fr;
    grid-template-rows: 1fr;
    width: min(780px, calc(100vw - 32px));
    height: calc(100vh - 80px); /* Gembala.pen 28: 40 px dari tepi atas dan bawah */
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
    border-radius: 6px;
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
    padding: 14px 22px 22px;
  }
  section {
    border: 1px solid var(--line);
    border-radius: 8px;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 9px 12px;
  }
  .row + .row {
    border-top: 1px solid var(--line);
  }
  button.grp {
    width: 100%;
    font: inherit;
    color: inherit;
    text-align: left;
    background: none;
    border: 0;
    cursor: pointer;
  }
  .row.sub {
    min-height: 32px;
    padding-left: 30px;
  }
  /* FR-61 (8): halaman akun, tiga kelompok */
  .accd {
    display: flex;
    flex-direction: column;
    padding-bottom: 6px;
    border-top: 1px solid var(--line);
  }
  .accd .grp {
    padding: 10px 12px 2px 30px;
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .accd .grp small {
    margin-left: 6px;
    font-weight: 400;
    letter-spacing: 0;
    text-transform: none;
  }
  .row > span:first-child:not(.acts) {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  small,
  .note,
  code {
    color: var(--muted);
    font-size: 12px;
  }
  small {
    line-height: 1.35;
  }
  .m,
  kbd {
    font: 12px "SF Mono", Menlo, monospace;
    color: var(--muted);
    overflow-wrap: anywhere;
    text-align: right;
  }
  .m.nowrap {
    overflow-wrap: normal;
    white-space: nowrap;
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
  /* Gembala.pen 28: sakelar ala macOS; knob digambar sebagai background supaya tetap satu elemen
     (pseudo-element tidak dirender di input) */
  input[role="switch"] {
    flex: none;
    width: 28px;
    height: 16px;
    margin: 0;
    border: 0;
    border-radius: 999px;
    background-color: var(--line);
    background-image: radial-gradient(circle 6px, #ffffff 99%, transparent 100%);
    background-repeat: no-repeat;
    background-position: 2px center;
    background-size: 12px 12px;
    appearance: none;
    -webkit-appearance: none;
    cursor: pointer;
  }
  input[role="switch"]:checked {
    background-color: var(--pri-bg, #3b82f6);
    background-position: right 2px center;
  }
  input[role="switch"]:focus-visible {
    outline: 2px solid var(--pri-bg, #3b82f6);
    outline-offset: 2px;
  }
  .seg {
    display: flex;
    flex: none;
  }
  .seg button {
    border-radius: 0;
    white-space: nowrap;
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
  .form .lbl {
    font-size: 12px;
    color: var(--muted);
  }
  .form .lbl small,
  .form label small {
    display: block;
  }
  .err {
    color: var(--tx-red, #ef4444);
  }
  .form label.inline {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .form input:not([type]) {
    padding: 5px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--card);
    color: var(--fg);
    font: 12px "SF Mono", Menlo, monospace;
  }
  /* Gembala.pen 28/30: judul grup tebal di atas kartu, keterangan abu menyambung di kanannya,
     aksi grup (+ Profil, Periksa ulang) menempel di ujung kanan */
  h4 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 18px 0 8px;
    font: 600 13px system-ui, -apple-system, sans-serif;
    color: var(--fg);
  }
  h4:first-child {
    margin-top: 0;
  }
  h4 > .mini {
    margin-left: auto;
    color: var(--fg);
    font-weight: 400;
  }
  h4 > .hint {
    color: var(--muted);
    font-size: 12px;
    font-weight: 400;
  }
  /* Gembala.pen 31: kembali ke halaman induk dari sub-halaman akun */
  .back {
    margin: 0 0 10px -4px;
    padding: 2px 4px;
    border: 0;
    background: none;
    color: var(--muted);
    font: inherit;
    cursor: pointer;
  }
  .back:hover {
    color: var(--fg);
  }
  /* status agent (✓ terpasang / ! belum diuji) dan akun (sudah dipakai) */
  .state {
    color: var(--muted);
    font-size: 12px;
    white-space: nowrap;
  }
  .state.warn {
    color: var(--tx-amber, #f59e0b);
  }
  .name {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .badge {
    padding: 0 5px;
    border: 1px solid var(--line);
    border-radius: 4px;
    color: var(--muted);
    font-size: 10px;
    line-height: 15px;
  }
  /* kartu profil terbuka: baris label–kontrol, bukan formulir bertumpuk */
  .pcard {
    border-top: 1px solid var(--line);
    padding-bottom: 2px;
  }
  .pcard .row {
    padding-left: 24px;
  }
  /* baris penutup kartu profil: aksi utama rata kanan, hapus/kembalikan di kiri */
  .pcard .row.foot {
    justify-content: flex-end;
  }
  .pcard .row.foot .link {
    margin-right: auto;
  }
  .row.col {
    flex-direction: column;
    align-items: stretch;
    gap: 6px;
    padding-top: 10px;
    padding-bottom: 10px;
  }
  .tin {
    box-sizing: border-box;
    min-width: 0;
    width: 220px;
    padding: 4px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }
  .tin.mono {
    font: 12px "SF Mono", Menlo, monospace;
  }
  .row.col .tin {
    width: 100%;
    resize: vertical;
  }
  .envrow {
    display: grid;
    grid-template-columns: 1fr 1fr auto;
    gap: 6px;
    align-items: center;
  }
  .addrow {
    align-self: flex-start;
  }
  .mini.pri {
    border-color: var(--pri-bg, #3b82f6);
    color: var(--pri-bg, #3b82f6);
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
</style>
