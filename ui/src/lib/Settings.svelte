<script lang="ts">
  // Pengaturan (FR-42): layar dan config.toml setara. Tiap ubahan ditulis daemon ke file (satu penulis),
  // lalu kembali sebagai `config.changed`, jadi layar selalu mencerminkan isi file.
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getVersion } from "@tauri-apps/api/app";
  import { call } from "./daemon";
  import { envRows, envPatch, type EnvRow } from "./env";
  import { pretty, type Account, type Agent, type Config, type Profile, type Ui } from "./config";
  import { errText, loc, t, tn, tOr } from "./i18n/state.svelte";

  let {
    cfg,
    meta,
    onclose,
    onquitall,
    onlogin,
    workspaces = [],
  }: {
    cfg: Config;
    workspaces?: { id: string; name: string }[]; // FR-78: MCP and plugins per workspace
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
  // label: set.acc.follow.<key>, keterangan: set.acc.follow.<key>Sub
  const FOLLOW = [
    { key: "global", items: ["CLAUDE.md"] },
    { key: "settings", items: ["settings.json"] },
    { key: "skills", items: ["skills", "commands", "agents"] },
    { key: "plugins", items: ["plugins"] },
    { key: "projects", items: ["projects"] },
  ] as const;
  async function updateAccount(params: Record<string, unknown>) {
    const r = await call("account.update", params).catch((e) => ({ error: String(e) }));
    accErr = r?.error ? errText(r.error, r.args) : "";
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
    if (r.error && !r.id) return void (accForm.error = errText(r.error, r.args));
    accForm = { name: "", dir: "", share: [...SHARE_DEFAULT], error: r.error ? errText(r.error, r.args) : "", info: r.skipped?.length ? t("set.acc.skipped", { items: r.skipped.join(", ") }) : "" };
    accOpen = !!(accForm.error || accForm.info);
    loadAccounts();
  }
  async function removeAccount(id: string) {
    if (removeAsk !== id) return void (removeAsk = id);
    removeAsk = "";
    const r = await call("account.remove", { id }).catch((e) => ({ error: String(e) }));
    accErr = r?.error ? errText(r.error, r.args) : "";
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
    let id = "profile";
    for (let n = 2; cfg.values.profiles.some((x) => x.id === id); n++) id = `profile-${n}`;
    form = {
      id,
      name: t("set.prof.newName"),
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
    if (r.error) failed = errText(r.error, r.args);
    else form = null;
  }

  async function resetProfile(id: string) {
    failed = "";
    const r = await call("profile.reset", { id }).catch((e) => ({ error: String(e) }));
    if (r.error) failed = errText(r.error, r.args);
  }
  let resetAsk = $state(false);
  let copied = $state("");

  /// FR-49: info laporan bug tanpa env, folder, atau isi layar (SEC-12)
  async function copyInfo() {
    const rows = Object.values(meta).map((m) => `${m.name || m.profile} (${m.agent || "shell"}) status=${m.status || "-"}`);
    // English on purpose: pasted into a GitHub issue
    const text = [`Gembala ${version} · daemon ${daemon?.daemon} · protocol ${daemon?.proto}`, `${navigator.userAgent}`, `panes: ${rows.length}`, ...rows].join("\n");
    await navigator.clipboard.writeText(text).then(() => (copied = t("common.copied")), () => (copied = t("chg.copyFailed")));
  }

  /// FR-48: hapus config, tata letak, hook, dan rahasia Keychain milik Gembala, lalu keluar
  async function reset() {
    await call("daemon.reset").catch(() => {});
    await invoke("forget_secret").catch(() => {});
    await invoke("quit");
  }

  const SECTIONS = ["umum", "tampilan", "agent", "pintasan", "toolbar", "keamanan", "proses", "diagnostik"] as const;
  // FR-60: proses milik Gembala, diperbarui tiap 2 detik selama bagian ini terbuka
  type Proc = { pid: number; ppid: number; rss_kb: number; cpu: number; role: string; name: string };
  // label: set.proc.role.<role>, keterangan: set.proc.role.<role>Sub
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
      const k = paneOf(p) || "";
      g.set(k, [...(g.get(k) ?? []), p]);
    }
    return [...g].map(([name, ps]) => {
      const run = ps.find((p) => AGENT_BIN.includes(p.name))?.name ?? ps.find((p) => !SHELLS.includes(p.name))?.name;
      return { name, ps, run: run ?? "", idle: !run, rss: ps.reduce((s, p) => s + p.rss_kb, 0), cpu: ps.reduce((s, p) => s + p.cpu, 0) };
    }).sort((a, b) => +a.idle - +b.idle || b.rss - a.rss);
  });
  let openGroups = $state<string[]>([]);
  let sec = $state<(typeof SECTIONS)[number]>("umum");
  let failed = $state("");
  let keys = $state<[string, string, string][]>([]);
  let keyErrors = $state<string[]>([]);
  let keyEdit = $state<{ id: string; combo: string } | null>(null);
  // FR-55: editor toolbar agent ala Warp (seret antar sisi, × buang, daftar tersedia); ditulis ke [toolbar]
  const CHIPS = ["attach", "diff", "rich_input", "cwd", "branch", "settings"]; // label: toolbar.chip.<id>
  const TB_DEFAULT = { left: ["attach", "diff", "rich_input"], right: ["cwd", "branch", "settings"] };
  type Zone = "left" | "right";
  const tb = $derived({ left: [...(cfg.values.toolbar?.left ?? TB_DEFAULT.left)], right: [...(cfg.values.toolbar?.right ?? TB_DEFAULT.right)] });
  const spare = $derived(CHIPS.filter((c) => !tb.left.includes(c) && !tb.right.includes(c)));
  async function saveTb(next: Record<Zone, string[]>) {
    failed = "";
    for (const z of ["left", "right"] as Zone[]) {
      const r = await call("config.set", { section: "toolbar", key: z, value: next[z] }).catch((e) => ({ error: String(e) }));
      if (r.error) failed = errText(r.error, r.args);
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
  const GROUPS = [["tab.", "menu.tab"], ["pane.", "menu.pane"], ["term.", "menu.view"], ["msg.", "menu.msg"]] as const;

  /// FR-39: kombinasi ditulis sebagai teks (WKWebView tidak meneruskan semua kombinasi ⌘ ke halaman)
  async function saveKey(id: string, combo: string) {
    failed = "";
    keyEdit = null;
    const r = await call("keys.set", { action: id, combo: combo.trim() }).catch((e) => ({ error: String(e) }));
    if (r.error) failed = errText(r.error, r.args);
  }
  let version = $state("");
  let agents = $state<Agent[]>([]);
  let detecting = $state(false);
  // Gembala.pen 31: halaman akun, dibuka dari baris Claude Code; nav Agent kembali ke halaman induk
  let agentPage = $state<"main" | "akun" | "addon">("main");
  // FR-78 (Gembala.pen 32): add-ons per launch; the daemon writes config.toml and the MCP file, the page follows cfg
  const addons = $derived(cfg.values.addons ?? { plugins: [], mcp: [] });
  const wsName = (id: string) => workspaces.find((w) => w.id === id)?.name ?? id;
  const MCP_TEMPLATE = '{\n  "mcpServers": {\n    "name": { "command": "npx", "args": ["-y", "mcp-package"] }\n  }\n}';
  let addonErr = $state("");
  let mcpEdit = $state<{ ws: string; text: string; strict: boolean; had: boolean; error: string } | null>(null);
  let rtk = $state<string | null | undefined>(undefined);
  $effect(() => {
    if (agentPage === "addon" && rtk === undefined) call("addon.status", {}).then((r) => (rtk = r?.rtk ?? null), () => (rtk = null));
  });
  async function plugin(params: Record<string, unknown>) {
    const r = await call("addon.plugin", params).catch((e) => ({ error: String(e) }));
    addonErr = r?.error ? errText(r.error, r.args) : "";
  }
  async function addPlugin() {
    const path = await invoke<string | null>("pick_folder");
    if (path) plugin({ path });
  }
  async function openMcp(ws: string) {
    const r = await call("addon.mcp.get", { workspace: ws }).catch(() => null);
    const pretty = (t: string) => {
      try {
        return JSON.stringify(JSON.parse(t), null, 2);
      } catch {
        return t;
      }
    };
    mcpEdit = { ws, text: r?.text?.trim() ? pretty(r.text) : MCP_TEMPLATE, strict: r?.strict ?? false, had: !!r?.text, error: "" };
  }
  async function saveMcp(off = false) {
    if (!mcpEdit) return;
    const r = await call("addon.mcp.set", { workspace: mcpEdit.ws, text: off ? "" : mcpEdit.text, strict: mcpEdit.strict }).catch((e) => ({ error: String(e) }));
    if (r?.error) mcpEdit.error = errText(r.error, r.args);
    else mcpEdit = null;
  }
  /// Jenis hook yang dipasang daemon (core/src/profile.rs `hook_files`); sumber status pane tiap agent.
  const HOOK: Record<string, string> = {
    claude: "set.agent.hook.claude",
    codex: "hook notify",
    omp: "hook omp-gembala.ts",
    opencode: "plugin opencode-gembala.js",
  }; // claude's label is translated (tOr), the rest are file names
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
    if (typeof r === "string" && r) failed = errText(r);
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
    if (r.error) failed = errText(r.error, r.args);
  }
  const setNotify = (key: string, value: boolean | number) => set(key as keyof Ui, value as never, "notify");

</script>

{#snippet editor()}
  {#if form}
    {@const claude = runsClaude(form)}
    <!-- FR-29 + SEC-5: kartu profil terbuka, gaya baris Gembala.pen 30 -->
    <div class="pcard">
      <div class="row">
        <span class="grow">{t("set.prof.name")}</span>
        <input class="tin" bind:value={form.name} aria-label={t("set.prof.nameAria")} />
      </div>
      {#if form.isNew}
        <div class="row">
          <span class="grow">Id<small>{t("set.prof.idSub")}</small></span>
          <input class="tin mono" bind:value={form.id} spellcheck="false" aria-label={t("set.prof.idAria")} />
        </div>
      {/if}
      <div class="row">
        <span class="grow">{t("set.prof.command")}</span>
        <input class="tin mono" bind:value={form.command} placeholder={t("set.prof.commandPh")} spellcheck="false" aria-label={t("set.prof.command")} />
      </div>
      <div class="row col">
        <span>{t("set.prof.args")}<small>{t("set.prof.argsSub")}</small></span>
        <textarea class="tin mono" bind:value={form.args} rows="2" spellcheck="false" aria-label={t("set.prof.args")}></textarea>
      </div>
      <div class="row">
        <span class="grow">{t("set.prof.folder")}<small>{t("set.prof.folderSub")}</small></span>
        <input class="tin mono" bind:value={form.cwd} placeholder={t("set.prof.folderSub")} spellcheck="false" aria-label={t("set.prof.folder")} />
      </div>
      {#if claude && cfg.values.accounts?.length}
        <div class="row">
          <span class="grow">{t("set.prof.account")}<small>{t("set.prof.accountSub")}</small></span>
          <select aria-label={t("set.prof.accountAria")} bind:value={form.account}
            ><option value="">{t("acc.main")}</option>{#each cfg.values.accounts.filter((a) => a.agent === "claude") as a (a.id)}<option value={a.id}>{a.name}</option>{/each}</select
          >
        </div>
      {/if}
      <div class="row">
        <span class="grow">{t("set.prof.policy")}<small>{t("set.prof.policySub")}</small></span>
        <span class="seg" role="group" aria-label={t("set.prof.policy")}>
          {#each ["auto", "confirm", "deny"] as const as v}
            <button class:on={form.policy === v} onclick={() => form && (form.policy = v)}>{t(`policy.${v}`)}</button>
          {/each}
        </span>
      </div>
      <div class="row">
        <span class="grow">{t("set.prof.perm")}<small>{t("set.prof.permSub")}</small></span>
        <span class="seg" role="group" aria-label={t("set.prof.permAria")}>
          {#each ["default", "auto"] as const as v}
            <button class:on={form.permission === v} onclick={() => form && (form.permission = v)}>{t(`set.prof.perm.${v}`)}</button>
          {/each}
        </span>
      </div>
      <div class="row">
        <span class="grow">{t("set.prof.summarize")}</span>
        <input type="checkbox" role="switch" bind:checked={form.summarize} aria-label={t("set.prof.summarize")} />
      </div>
      <div class="row">
        <span class="grow">{t("set.prof.notify")}</span>
        <input type="checkbox" role="switch" bind:checked={form.notify} aria-label={t("set.prof.notify")} />
      </div>
      <div class="row col">
        <span>Env<small>{t("set.prof.envSub")}</small></span>
        {#each form.env as e, i (i)}
          <div class="envrow">
            <input class="tin mono" bind:value={e.name} placeholder={t("set.prof.envNamePh")} spellcheck="false" aria-label={t("set.prof.envName", { n: i + 1 })} />
            <input class="tin mono" bind:value={e.value} placeholder={t("set.prof.envValuePh")} spellcheck="false" aria-label={t("set.prof.envValue", { n: i + 1 })} />
            <button class="mini" aria-label={t("set.prof.envDel", { name: e.name || i + 1 })} onclick={() => envDel(i)}>×</button>
          </div>
        {/each}
        <button class="mini addrow" onclick={envAdd}>+ Env</button>
      </div>
      <div class="row col">
        <span>{t("set.prof.secret")}<small><code>{t("set.prof.envNamePh")}=op://vault/item/field</code> {t("common.or")} <code>{t("set.prof.envNamePh")}=keychain:service/{t("set.prof.secretAcct")}</code></small></span>
        <textarea class="tin mono" bind:value={form.secret} rows="2" spellcheck="false" aria-label={t("set.prof.secret")}></textarea>
      </div>
      <div class="row foot">
        {#if !form.isNew}
          <button class="link" onclick={() => form && resetProfile(form.id)}>{BUILTIN.includes(form.id) ? t("set.prof.reset") : t("set.prof.delete")}</button>
        {/if}
        <span class="acts">
          <button class="mini" onclick={() => (form = null)}>{t("common.cancel")}</button>
          <button class="mini pri" onclick={saveForm}>{t("common.save")}</button>
        </span>
      </div>
    </div>
  {/if}
{/snippet}

<div class="veil" role="presentation" onpointerdown={(e) => e.target === e.currentTarget && onclose()}>
  <div class="modal" role="dialog" aria-label={t("set.title")}>
    <nav>
      {#each SECTIONS as k}
        <button class:on={k === sec} onclick={() => ((sec = k), (agentPage = "main"))}>{t(`set.nav.${k}`)}</button>
      {/each}
    </nav>
    <div class="body">
      {#if cfg.errors.length}
        <div class="errs" role="alert">
          <b>{t("set.cfgErrors")}</b>
          {#each cfg.errors as e}<span>{errText(e)}</span>{/each}
        </div>
      {/if}
      {#if failed}<div class="errs" role="alert"><span>{failed}</span></div>{/if}

      {#if sec === "umum"}
        <section>
          <div class="row">
            <span>{t("set.general.language")}<small>{t("set.general.languageSub")}</small></span>
            <select aria-label={t("set.general.language")} value={ui.language ?? "system"} onchange={(e) => set("language", e.currentTarget.value as Ui["language"])}>
              <option value="system">{t("set.general.languageSystem")}</option>
              <option value="id">Indonesia</option>
              <option value="en">English</option>
            </select>
          </div>
          <div class="row"><span>{t("set.general.daemon")}<small>{t("set.general.daemonSub")}</small></span><span class="m">{t("set.general.daemonState")}</span></div>
        </section>
        <h4>{t("set.general.notify")}</h4>
        <section>
          <div class="row">
            <span>{t("set.general.blocked")}<small>{t("set.general.blockedSub")}</small></span>
            <input type="checkbox" role="switch" aria-label={t("set.general.blocked")} checked={cfg.values.notify.on_blocked} onchange={(e) => setNotify("on_blocked", e.currentTarget.checked)} />
          </div>
          <div class="row">
            <span>{t("set.general.done")}</span>
            <input type="checkbox" role="switch" aria-label={t("set.general.doneAria")} checked={cfg.values.notify.on_done} onchange={(e) => setNotify("on_done", e.currentTarget.checked)} />
          </div>
          <div class="row">
            <span>{t("set.general.sound")}</span>
            <input type="checkbox" role="switch" aria-label={t("set.general.soundAria")} checked={cfg.values.notify.sound} onchange={(e) => setNotify("sound", e.currentTarget.checked)} />
          </div>
          <div class="row">
            <span>{t("set.general.long")}<small>{t("set.general.longSub")}</small></span>
            <select aria-label={t("set.general.long")} value={cfg.values.notify.long_command_secs} onchange={(e) => setNotify("long_command_secs", +e.currentTarget.value)}>
              {#each [10, 30, 60, 120, 300] as n}<option value={n}>{t("set.general.secs", { n })}</option>{/each}
            </select>
          </div>
        </section>
        <h4>{t("set.general.quit")}</h4>
        <section>
          <div class="row">
            <span>{t("set.general.onQuit")}<small>{t("set.general.onQuitSub")}</small></span>
            <select aria-label={t("set.general.onQuit")} value={ui.on_quit} onchange={(e) => set("on_quit", e.currentTarget.value as Ui["on_quit"])}>
              <option value="keep">{t("set.general.keep")}</option>
              <option value="stop">{t("set.general.stop")}</option>
            </select>
          </div>
          <div class="row">
            <span>{t("set.general.quitAll")}<small>{t("set.general.quitAllSub")}</small></span>
            <button class="mini" onclick={onquitall}>{t("set.general.quitAllBtn")}</button>
          </div>
        </section>
      {:else if sec === "tampilan"}
        <section>
          <div class="row">
            <span>{t("set.look.theme")}</span>
            <span class="seg">
              {#each ["system", "dark", "light"] as const as v}
                <button class:on={ui.theme === v} onclick={() => set("theme", v)}>{t(`set.look.theme.${v}`)}</button>
              {/each}
            </span>
          </div>
          <div class="row">
            <span>{t("set.look.font")}</span>
            <select aria-label={t("set.look.fontAria")} value={ui.font_family} onchange={(e) => set("font_family", e.currentTarget.value)}>
              {#each [...new Set([ui.font_family, "Hack", "SF Mono", "Menlo", "Monaco"])] as f}<option>{f}</option>{/each}
            </select>
          </div>
          <div class="row">
            <span>{t("set.look.size")}<small>{t("set.look.sizeSub")}</small></span>
            <select aria-label={t("set.look.sizeAria")} value={ui.font_size} onchange={(e) => set("font_size", +e.currentTarget.value)}>
              {#each [10, 11, 12, 13, 14, 15, 16, 18, 20] as n}<option value={n}>{n} px</option>{/each}
            </select>
          </div>
          <div class="row">
            <span>{t("set.look.lineHeight")}</span>
            <select aria-label={t("set.look.lineHeight")} value={ui.line_height} onchange={(e) => set("line_height", +e.currentTarget.value)}>
              {#each [1.0, 1.1, 1.2, 1.3, 1.4, 1.5] as n}<option value={n}>{n.toFixed(1)}</option>{/each}
            </select>
          </div>
          <div class="row">
            <span>{t("set.look.cursor")}</span>
            <span class="seg">
              {#each ["bar", "block", "underline"] as const as v}
                <button class:on={ui.cursor === v} onclick={() => set("cursor", v)}>{t(`set.look.cursor.${v}`)}</button>
              {/each}
            </span>
          </div>
          <div class="row">
            <span>{t("set.look.blink")}</span>
            <input type="checkbox" role="switch" aria-label={t("set.look.blink")} checked={ui.cursor_blink} onchange={(e) => set("cursor_blink", e.currentTarget.checked)} />
          </div>
          <div class="row">
            <span>{t("set.look.meta")}<small>{t("set.look.metaSub")}</small></span>
            <select aria-label={t("set.look.meta")} value={ui.option_as_meta} onchange={(e) => set("option_as_meta", e.currentTarget.value as Ui["option_as_meta"])}>
              <option value="left">{t("set.look.meta.left")}</option>
              <option value="right">{t("set.look.meta.right")}</option>
              <option value="both">{t("set.look.meta.both")}</option>
              <option value="none">{t("set.look.meta.none")}</option>
            </select>
          </div>
          <div class="row">
            <span>Scrollback<small>{t("set.look.scrollSub")}</small></span>
            <select aria-label="Scrollback" value={ui.scrollback_lines} onchange={(e) => set("scrollback_lines", +e.currentTarget.value)}>
              {#each [1000, 5000, 10000, 50000] as n}<option value={n}>{t("set.look.lines", { n: n.toLocaleString(loc()) })}</option>{/each}
            </select>
          </div>
          <div class="row">
            <span>{t("set.look.reader")}<small>{t("set.look.readerSub")}</small></span>
            <input type="checkbox" role="switch" aria-label={t("set.look.reader")} checked={ui.screen_reader} onchange={(e) => set("screen_reader", e.currentTarget.checked)} />
          </div>
          <div class="row">
            <span>Input editor<small>{t("set.look.editorSub")}</small></span>
            <input type="checkbox" role="switch" aria-label="Input editor" checked={ui.input_editor} onchange={(e) => set("input_editor", e.currentTarget.checked)} />
          </div>
        </section>
        <!-- FR-58 (keputusan 4 Nov, mockup-blok-perintah opsi C): satu sakelar global untuk linimasa blok perintah;
             "ringkas output agent" tetap per profil (FR-56), bukan di grup ini -->
        <h4>{t("set.look.blocks")}</h4>
        <section>
          <div class="row">
            <span>{t("set.look.timeline")}<small>{t("set.look.timelineSub")}</small></span>
            <input type="checkbox" role="switch" aria-label={t("set.look.timeline")} checked={ui.command_timeline} onchange={(e) => set("command_timeline", e.currentTarget.checked)} />
          </div>
        </section>
      {:else if sec === "agent"}
        {#if agentPage === "akun"}
          <!-- Gembala.pen 31: halaman akun dibuka dari baris agent; nav Agent kembali ke halaman induk -->
          <button class="back" onclick={() => (agentPage = "main")}>‹ {t("set.nav.agent")}</button>
          <h4>{t("set.acc.title")}<span class="hint">{t("set.acc.hint")}</span>{#if !accOpen}<button class="mini" onclick={() => (accOpen = true)}>{t("set.acc.add")}</button>{/if}</h4>
        <section>
          {#each accRows as r (r.account.id)}
            <div class="row">
              <span class="grow"
                ><span class="name">{r.account.main ? t("acc.main") : r.account.name}{#if r.account.main}<span class="badge">{t("set.acc.default")}</span>{/if}</span><small
                  >{r.status.email ?? t("set.acc.noEmail")} · {r.account.dir}{r.account.model || r.account.effort
                    ? ` · ${[r.account.model, r.account.effort].filter(Boolean).join(" · ")}`
                    : ""}{r.account.main ? ` · ${t("set.acc.cantDelete")}` : ""}</small
                ></span
              >
              <span class="acts">
                {#if !r.status.used}<button class="mini" onclick={() => onlogin(r.account.id, r.account.name)}>Login</button>{/if}
                {#if !r.account.main}
                  <button class="mini pri" aria-expanded={accDetail === r.account.id} onclick={() => (accDetail = accDetail === r.account.id ? "" : r.account.id)}>{accDetail === r.account.id ? t("common.close") : t("common.edit")}</button>
                  <button class="mini" onclick={() => removeAccount(r.account.id)}>{removeAsk === r.account.id ? t("common.sure") : t("common.delete")}</button>
                {/if}
                <span class="state">{r.status.used ? t("set.acc.used") : t("set.acc.notLogged")}</span>
              </span>
            </div>
            {#if accDetail === r.account.id}
              {@const a = r.account}
              <!-- FR-61 (8), mockup-akun-setelan varian A -->
              <div class="accd">
                <span class="grp">{t("set.acc.own")} <small>{t("set.acc.ownSub")}</small></span>
                <div class="row sub">
                  <span class="grow">Model<small>{t("set.acc.modelSub")}</small></span>
                  <select aria-label={t("set.acc.modelAria", { name: a.name })} value={a.model ?? ""} onchange={(e) => updateAccount({ id: a.id, model: e.currentTarget.value })}>
                    <option value="">{t("set.acc.default.opt")}</option>{#each models as m (m)}<option value={m}>{m}</option>{/each}
                  </select>
                </div>
                <div class="row sub">
                  <span class="grow">Effort</span>
                  <select aria-label={t("set.acc.effortAria", { name: a.name })} value={a.effort ?? ""} onchange={(e) => updateAccount({ id: a.id, effort: e.currentTarget.value })}>
                    <option value="">{t("set.acc.default.opt")}</option>{#each efforts as x (x)}<option value={x}>{x}</option>{/each}
                  </select>
                </div>
                <span class="grp">{t("set.acc.follow")} <small>{t("set.acc.followSub")}</small></span>
                {#each FOLLOW as f (f.key)}
                  {@const on = f.items.every((i) => a.share.includes(i))}
                  <div class="row sub">
                    <span class="grow">{t(`set.acc.follow.${f.key}`)}<small>{t(`set.acc.follow.${f.key}Sub`)}</small></span>
                    <span class="seg" role="group" aria-label={t(`set.acc.follow.${f.key}`)}>
                      <button class:on onclick={() => !on && setFollow(a, [...f.items], true)}>{t("set.acc.follow")}</button>
                      <button class:on={!on} onclick={() => on && setFollow(a, [...f.items], false)}>{ownAsk === `${a.id}:${f.items[0]}` ? t("common.sure") : t("set.acc.ownOpt")}</button>
                    </span>
                  </div>
                  {#if ownAsk === `${a.id}:${f.items[0]}`}<span class="m">{t("set.acc.ownWarn")}</span>{/if}
                {/each}
                <div class="row sub">
                  <span class="grow">{t("set.acc.trust")}<small>{t("set.acc.trustSub")}</small></span>
                  <span class="seg" role="group" aria-label={t("set.acc.trust")}>
                    <button class:on={a.trust !== false} onclick={() => a.trust === false && updateAccount({ id: a.id, trust: true })}>{t("set.acc.follow")}</button>
                    <button class:on={a.trust === false} onclick={() => a.trust !== false && updateAccount({ id: a.id, trust: false })}>{t("set.acc.ownOpt")}</button>
                  </span>
                </div>
                <span class="grp">{t("set.acc.always")} <small>{t("set.acc.alwaysSub")}</small></span>
                <div class="row sub"><span class="grow">🔒 {t("set.acc.login")}<small>{t("set.acc.loginSub")}</small></span></div>
                <div class="row sub"><span class="grow">🔒 {t("set.acc.connectors")}<small>{t("set.acc.connectorsSub")}</small></span></div>
                <div class="row sub"><span class="grow">🔒 {t("set.acc.history")}<small>{t("set.acc.historySub")}</small></span></div>
              </div>
            {/if}
          {/each}
          {#if removeAsk}<span class="m">{t("set.acc.removeWarn")}</span>{/if}
          {#if accErr}<span class="m err" role="alert">{accErr}</span>{/if}
          {#if accOpen}
          <div class="form">
            <label>{t("set.acc.formName")} <input bind:value={accForm.name} placeholder={t("set.acc.formNamePh")} /></label>
            <label>{t("set.prof.folder")} <small>{t("set.acc.formDirSub")}</small><input bind:value={accForm.dir} placeholder={`~/.claude-${slug(accForm.name) || t("set.acc.formDirPh")}`} spellcheck="false" /></label>
            <span class="lbl">{t("set.acc.bring")} <small>{t("set.acc.bringSub")}</small></span>
            {#each shareOpts as s (s)}
              <label class="inline"><input type="checkbox" role="switch" checked={accForm.share.includes(s)} onchange={(e) => (accForm.share = e.currentTarget.checked ? [...accForm.share, s] : accForm.share.filter((x) => x !== s))} /> {s === "projects" ? t("set.acc.projects") : s}</label>
            {/each}
            {#if accForm.error}<span class="m err" role="alert">{accForm.error}</span>{/if}
            {#if accForm.info}<span class="m">{accForm.info}</span>{/if}
            <span class="acts"><button class="mini" onclick={() => (accOpen = false)}>{t("common.cancel")}</button><button class="mini" disabled={!accForm.name.trim()} onclick={createAccount}>{t("set.acc.create")}</button></span>
          </div>
          {/if}
        </section>
        {:else if agentPage === "addon"}
          <!-- Gembala.pen 32 (FR-78): add-on per launch; config agent nggak disentuh (SEC-15) -->
          <button class="back" onclick={() => ((agentPage = "main"), (mcpEdit = null))}>‹ {t("set.nav.agent")}</button>
          <h4>{t("set.addon.plugins")}<span class="hint">{t("set.addon.pluginsHint")}</span><button class="mini" onclick={addPlugin}>{t("set.addon.addPlugin")}</button></h4>
          <section>
            {#each addons.plugins as p (p.id)}
              <div class="row">
                <span class="grow">{p.id}<small>{p.path}{p.scope === "workspaces" ? ` · ${p.workspaces.length ? t("set.addon.onlyIn", { names: p.workspaces.map(wsName).join(", ") }) : t("set.addon.noWs")}` : ""}</small></span>
                <span class="acts">
                  <span class="seg" role="group" aria-label="Plugin {p.id}">
                    {#each ["off", "all", "workspaces"] as const as v (v)}
                      <button class:on={p.scope === v} onclick={() => p.scope !== v && plugin({ id: p.id, scope: v })}>{t(`set.addon.scope.${v}`)}</button>
                    {/each}
                  </span>
                  <button class="mini" aria-label={t("set.addon.remove", { id: p.id })} onclick={() => plugin({ id: p.id, remove: true })}>×</button>
                </span>
              </div>
              {#if p.scope === "workspaces"}
                <div class="form">
                  {#each workspaces as w (w.id)}
                    <label class="inline"><input type="checkbox" role="switch" checked={p.workspaces.includes(w.id)}
                      onchange={(e) => plugin({ id: p.id, workspaces: e.currentTarget.checked ? [...p.workspaces, w.id] : p.workspaces.filter((x) => x !== w.id) })} /> {w.name}</label>
                  {/each}
                </div>
              {/if}
            {:else}
              <div class="row"><span>{t("set.addon.none")}<small>{t("set.addon.noneSub")}</small></span></div>
            {/each}
          </section>
          {#if addonErr}<span class="m err" role="alert">{addonErr}</span>{/if}
          <h4>{t("set.addon.mcp")}<span class="hint">{t("set.addon.mcpHint")}</span></h4>
          <section>
            {#each workspaces as w (w.id)}
              {@const m = addons.mcp.find((x) => x.workspace === w.id)}
              <div class="row">
                <span class="grow">{w.name}<small>{m ? `${m.servers.join(", ") || t("set.addon.noServers")} · ${m.strict ? t("set.addon.strictOn") : t("set.addon.strictOff")}` : t("set.addon.globalOnly")}</small></span>
                {#if mcpEdit?.ws !== w.id}<button class="mini" onclick={() => openMcp(w.id)}>{m ? t("common.edit") : t("set.addon.setUp")}</button>{/if}
              </div>
              {#if mcpEdit?.ws === w.id}
                <div class="row col">
                  <small>{t("set.addon.mcpFormat", { env: "${ENV_NAME}" })}</small>
                  <textarea class="tin mono" rows="8" spellcheck="false" aria-label={t("set.addon.mcpAria", { name: w.name })} bind:value={mcpEdit.text}></textarea>
                  <label class="inline"><input type="checkbox" role="switch" bind:checked={mcpEdit.strict} /> {t("set.addon.strict")} <small>{t("set.addon.strictSub")}</small></label>
                  {#if mcpEdit.error}<span class="m err" role="alert">{mcpEdit.error}</span>{/if}
                  <span class="acts">
                    {#if mcpEdit.had}<button class="mini" onclick={() => saveMcp(true)}>{t("set.addon.turnOff")}</button>{/if}
                    <button class="mini" onclick={() => (mcpEdit = null)}>{t("common.cancel")}</button>
                    <button class="mini pri" onclick={() => saveMcp()}>{t("common.save")}</button>
                  </span>
                </div>
              {/if}
            {:else}
              <div class="row"><span>{t("set.addon.noWorkspaces")}</span></div>
            {/each}
          </section>
          <h4>{t("set.addon.tokens")}<span class="hint">{t("set.addon.tokensHint")}</span></h4>
          <section>
            <div class="row">
              <span>RTK<small>{rtk === undefined ? t("set.agent.checking") : rtk ? t("set.addon.rtkOn", { v: rtk }) : t("set.addon.rtkOff")}</small></span>
              <span class="state">{rtk ? `✓ ${t("set.agent.installed")}` : rtk === null ? `✗ ${t("set.agent.missing")}` : ""}</span>
            </div>
          </section>
          <h4>{t("set.addon.others")}</h4>
          <section>
            <div class="row"><span>Codex<small>{t("set.addon.codexSub")}</small></span><span class="state">{t("set.addon.partial")}</span></div>
            <div class="row"><span>Oh My Pi · OpenCode<small>{t("set.addon.noFlag")}</small></span><span class="state">{t("set.addon.unsupported")}</span></div>
          </section>
          <p class="warnbox">⚠ {t("set.addon.warn")}</p>
        {:else}
          <h4>{t("set.agent.installedH")}<span class="hint">{t("set.agent.installedHint")}</span>
            <button class="mini" disabled={detecting} onclick={() => detect(true)}>{detecting ? t("set.agent.checkingBtn") : t("set.agent.recheck")}</button>
            <!-- Add-on lintas agent (Claude plugin + MCP, Codex MCP), jadi di kepala grup, bukan di baris Claude -->
            <button class="mini" onclick={() => (agentPage = "addon")}>Add-on ({addons.plugins.length + addons.mcp.length})…</button></h4>
          <section>
            {#each agents as a (a.agent)}
              <div class="row">
                <span
                  >{a.name}<small
                    >{a.installed
                      ? `${a.version ?? t("set.agent.noVersion")} · ${a.tested ? t("set.agent.tested") : t("set.agent.untested")}${HOOK[a.agent] ? ` · ${tOr(HOOK[a.agent], HOOK[a.agent])}` : ""}`
                      : t("set.agent.notFound", { install: a.install })}</small
                  ></span
                >
                <span class="acts">
                  {#if a.agent === "claude" && a.installed}<button class="mini" onclick={() => (agentPage = "akun")}>{t("set.agent.accounts", { n: accRows.length })}</button>{/if}
                  <span class="state" class:warn={a.installed && !a.tested}>{a.installed ? (a.tested ? `✓ ${t("set.agent.installed")}` : `! ${t("set.agent.notTried")}`) : `✗ ${t("set.agent.missing")}`}</span>
                </span>
              </div>
            {:else}
              <div class="row"><span>{t("set.agent.checkingBtn")}</span></div>
            {/each}
          </section>
          <h4>{t("set.prof.title")}<button class="mini" onclick={blank}>{t("set.prof.add")}</button></h4>
          <section>
            {#each cfg.values.profiles as p (p.id)}
              {@const open = !!form && form.id === p.id && !form.isNew}
              <div class="row">
                <span class="grow"
                  >{p.name}<small
                    >{[p.command ? [p.command, ...p.args].join(" ") : "$SHELL -l", t("set.prof.folderIs", { dir: p.cwd || t("set.prof.followFocus") }), p.agent === "shell" ? "" : tOr(`policy.${p.policy}`, p.policy ?? "")]
                      .filter(Boolean)
                      .join(" · ")}</small
                  ></span
                >
                <span class="acts">
                  {#if open}
                    <button class="mini pri" onclick={() => (form = null)}>{t("common.close")}</button>
                  {:else}
                    <button class="mini" onclick={() => edit(p)}>{t("common.edit")}</button>
                    <button class="mini" onclick={() => edit(p, true)}>{t("set.prof.duplicate")}</button>
                  {/if}
                </span>
              </div>
              {#if open}{@render editor()}{/if}
            {/each}
            {#if form?.isNew}{@render editor()}{/if}
          </section>
          <p class="note">
            {t("set.prof.note1")} <code>[[profile]]</code>{t("set.prof.note2")} <code>op://…</code> {t("common.or")}
            <code>keychain:service/{t("set.prof.secretAcct")}</code> {t("set.prof.note3")} <code>secret_env</code>.
          </p>
        {/if}
      {:else if sec === "pintasan"}
        {#if keyErrors.length}
          <div class="errs" role="alert">
            <b>{t("set.keys.problems")}</b>
            {#each keyErrors as e}<span>{e}</span>{/each}
          </div>
        {/if}
        <!-- FR-39: dikelompokkan seperti menu; Tab 1–9 satu baris; id aksi di tooltip -->
        {#each GROUPS as [prefix, title]}
          <h4 class="khead">{t(title)}</h4>
          <section>
            {#each keys.filter(([id]) => id.startsWith(prefix) && !/^tab\.goto\.[2-9]$/.test(id)) as [id, label, key] (id)}
              {@const goto = id === "tab.goto.1"}
              {@const custom = Object.values(cfg.values.keys ?? {}).includes(id)}
              <div class="krow" title={id}>
                <span class="klabel">{goto ? t("set.keys.goto") : label}</span>
                {#if keyEdit?.id === id}
                  <!-- svelte-ignore a11y_autofocus -->
                  <input class="keyin" bind:value={keyEdit.combo} autofocus aria-label={t("set.keys.comboAria", { label })} spellcheck="false" placeholder={t("set.keys.comboPh")}
                    onkeydown={(e) => {
                      if (e.key === "Enter") saveKey(id, keyEdit?.combo ?? "");
                      if (e.key === "Escape") (e.stopPropagation(), (keyEdit = null));
                    }} />
                {:else}
                  <span class="kacts">
                    {#if custom}<button class="link" title={t("set.keys.defaultTip")} onclick={() => saveKey(id, "")}>{t("set.acc.default.opt")}</button>{/if}
                    {#if !goto}<button class="link" onclick={() => (keyEdit = { id, combo: key.toLowerCase() })}>{t("common.edit")}</button>{/if}
                  </span>
                  <kbd class:custom>{goto ? "⌘1 … ⌘9" : key ? pretty(key) : "—"}</kbd>
                {/if}
              </div>
            {/each}
          </section>
        {/each}
        <h4 class="khead">{t("set.keys.other")}</h4>
        <section>
          <div class="krow"><span class="klabel">{t("set.keys.copyPaste")}</span><kbd>⌘C ⌘V</kbd></div>
          <div class="krow"><span class="klabel">{t("set.title")}</span><kbd>⌘,</kbd></div>
          <div class="krow"><span class="klabel">{t("set.keys.rest")}</span><span class="m">{t("set.keys.restTo")}</span></div>
        </section>
        <p class="note">
          {t("set.keys.note1")} <code>cmd</code>. {t("set.keys.note2")}
          <code>[keys]</code> {t("set.keys.note3")} <code>"cmd+t" = "pane.split.right"</code>{t("set.keys.note4")}
        </p>
      {:else if sec === "toolbar"}
        <!-- FR-55: seret chip untuk ubah urutan atau pindah sisi; × buang; chip tersedia bisa ditambah lagi -->
        <div class="tbzones">
          {#each ["left", "right"] as const as z (z)}
            <div class="tbzone-wrap">
              <h4 class="khead">{t(`set.tb.${z}`)}</h4>
              <div class="tbzone" data-tbz={z} role="list" aria-label={t("set.tb.zoneAria", { side: t(`set.tb.${z}`) })}>
                {#each tb[z as Zone] as c, i (c)}
                  <span class="tbchip" class:lift={tbDrag?.z === z && tbDrag.i === i} data-tbz={z} data-tbi={i} role="listitem"
                    onpointerdown={(e) => tbStart(e, z as Zone, i)}>
                    {tOr(`toolbar.chip.${c}`, c)}
                    <button class="tbx" aria-label={t("set.tb.remove", { chip: tOr(`toolbar.chip.${c}`, c) })} onclick={() => saveTb({ ...tb, [z]: tb[z as Zone].filter((x) => x !== c) })}>×</button>
                  </span>
                {:else}
                  <span class="m">{t("set.tb.empty")}</span>
                {/each}
              </div>
            </div>
          {/each}
        </div>
        <h4 class="khead">{t("set.tb.spare")}</h4>
        <div class="tbzone spare">
          {#each spare as c (c)}
            <button class="tbchip" onclick={() => saveTb({ ...tb, left: [...tb.left, c] })}>{tOr(`toolbar.chip.${c}`, c)} <span class="m">＋</span></button>
          {:else}
            <span class="m">{t("set.tb.allUsed")}</span>
          {/each}
        </div>
        <p class="note">
          {t("set.tb.note")}
          <button class="link" onclick={() => saveTb(TB_DEFAULT)}>{t("set.prof.reset")}</button>
        </p>
      {:else if sec === "proses"}
        <p class="note">
          {t("set.proc.note")}
        </p>
        <section>
          {#each procs.filter((p) => p.role !== "pane") as p (p.pid)}
            <div class="row">
              <span>{tOr(`set.proc.role.${p.role}`, p.role)} · <code>{p.name}</code><small>{tOr(`set.proc.role.${p.role}Sub`, "")}</small></span>
              <span class="m">PID {p.pid} · {mb(p.rss_kb)} · CPU {p.cpu.toFixed(1)}%</span>
            </div>
          {:else}
            <div class="row"><span>{t("common.loading")}</span></div>
          {/each}
          <div class="row">
            <span>{t("set.proc.total")}<small>{t("set.proc.totalSub")}</small></span>
            <span class="m">{mb(procs.filter((p) => p.role !== "pane").reduce((s, p) => s + p.rss_kb, 0))}</span>
          </div>
        </section>
        <h4>{t("set.proc.panes")} <small>{t("set.proc.sum")} {mb(paneGroups.reduce((s, g) => s + g.rss, 0))}</small></h4>
        <section>
          {#each paneGroups as g (g.name)}
            {@const open = openGroups.includes(g.name)}
            <button class="row grp" aria-expanded={open} onclick={() => (openGroups = open ? openGroups.filter((x) => x !== g.name) : [...openGroups, g.name])}>
              <span>{open ? "▾" : "▸"} {g.name || t("set.proc.otherPane")}<small>{g.run || t("set.proc.idleShell")}</small></span>
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
            <div class="row"><span>{t("msg.noPanes")}</span></div>
          {/each}
        </section>
        <section>
          {#if stopAsk}
            <div class="row" role="alert">
              <span>{t("set.proc.stopAsk")}</span>
              <span><button class="mini" onclick={() => (stopAsk = false)}>{t("common.cancel")}</button> <button class="mini" onclick={async () => (await call("daemon.stop").catch(() => {}), await invoke("quit"))}>{t("common.stop")}</button></span>
            </div>
          {:else}
            <div class="row">
              <span>{t("set.general.stop")}<small>{t("set.proc.stopSub")}</small></span>
              <button class="mini" onclick={() => (stopAsk = true)}>{t("set.general.quitAllBtn")}</button>
            </div>
          {/if}
        </section>
      {:else if sec === "keamanan"}
        <!-- Gembala.pen 36: nilai diambil dari aturan daemon, bukan diketik ulang; pengecualian lintas workspace
             belum ada di daemon (msg.rs same_workspace), jadi tombol "+ Pengecualian" tidak dibuat -->
        <h4>{t("set.sec.a2a")}</h4>
        <section>
          <div class="row">
            <span>{t("set.sec.order")}<small>{t("set.sec.orderSub")}</small></span>
            <span class="state">{t("set.sec.orderState")}</span>
          </div>
          <div class="row">
            <span>{t("set.sec.peek")}<small>{t("set.sec.peekSub")}</small></span>
            <span class="state">{t("set.sec.peekState")}</span>
          </div>
          <div class="row">
            <span>{t("set.sec.brake")}<small>{t("set.sec.brakeSub", { rate: rate ?? "—", hops: hops ?? "—" })}</small></span>
            <span class="state">{t("set.sec.brakeState")}</span>
          </div>
          <div class="row">
            <span>{t("set.sec.panic")}<small>{t("set.sec.panicSub")}</small></span>
            <kbd>{stopKey}</kbd>
          </div>
          <div class="row">
            <span>{t("set.sec.stamp")}<small>{t("set.sec.stampSub")}</small></span>
            <span class="m nowrap">[Gembala · from shell-6]</span>
          </div>
        </section>
        <h4>{t("set.sec.log")}<span class="hint">{t("set.sec.logHint")}</span></h4>
        <section>
          <div class="row">
            <span>{t("set.sec.logAt")}<small>{t("set.sec.logAtSub")}</small></span>
            <span class="m">{auditDir}</span>
          </div>
          <div class="row">
            <span>{t("set.sec.logAge")}<small>{t("set.sec.logAgeSub")}</small></span>
            <span class="m">{t("set.sec.days", { n: 14 })}</span>
          </div>
          <div class="row">
            <span>{t("set.sec.logOpen")}</span>
            <button class="mini" onclick={() => reveal("audit")}>{t("common.open")}</button>
          </div>
        </section>
        <h4>{t("set.sec.behind")}<span class="hint">{t("set.sec.behindHint")}</span></h4>
        <section>
          <div class="row"><span>{t("set.sec.door")}<small>{t("set.sec.doorSub")}</small></span><span class="m">$TMPDIR/gembala</span></div>
          <div class="row"><span>{t("set.sec.buttons")}<small>{t("set.sec.buttonsSub")}</small></span><span class="m">{t("set.sec.locked")}</span></div>
          <div class="row"><span>{t("set.sec.newTerm")}<small>{t("set.sec.newTermSub")}</small></span><span class="m">{t("set.sec.clean")}</span></div>
          <div class="row"><span>{t("set.sec.links")}<small>{t("set.sec.linksSub")}</small></span><span class="m">{t("set.sec.askFirst")}</span></div>
          <div class="row"><span>{t("set.sec.net")}<small>{t("set.sec.netSub")}</small></span><span class="m">{t("set.sec.off")}</span></div>
        </section>
      {:else}
        <section>
          <div class="row"><span>{t("set.diag.versions")}</span><span class="m">{version} · {daemon?.daemon ?? "?"} · {daemon?.proto ?? "?"}</span></div>
          <div class="row">
            <span>{t("set.diag.release")}<small>{t("set.diag.releaseSub")}</small></span>
            <button class="mini" onclick={() => invoke("open_url", { url: "https://github.com/AHanif52/gembala/releases" })}>{t("set.diag.releaseBtn")}</button>
          </div>
          <div class="row"><span>{t("set.diag.cfgFile")}</span><span class="m">{cfg.path}</span></div>
          <div class="row"><span>{t("set.diag.cfgState")}</span><span class="m">{cfg.errors.length ? tn("set.diag.problems", cfg.errors.length) : t("set.sec.clean")}</span></div>
          <div class="row"><span>{t("set.diag.sock")}<small>{t("set.diag.sockSub")}</small></span><span class="m">$TMPDIR/gembala</span></div>
        </section>
        <h4>{t("set.diag.agents")}</h4>
        <section>
          {#each Object.values(meta) as m}
            <div class="row">
              <span>{m.name || m.profile}<small>{["claude", "codex", "opencode", "omp"].includes(m.agent) ? t("set.diag.byHook") : m.agent === "custom" ? t("set.diag.byScreen") : t("set.diag.byShell")}</small></span>
              <span class="m">{m.status ? tOr(`status.${m.status}`, m.status) : "-"}</span>
            </div>
          {:else}
            <div class="row"><span>{t("msg.noPanes")}</span></div>
          {/each}
        </section>
        <h4>{t("set.diag.help")}</h4>
        <section>
          <div class="row">
            <span>{t("set.diag.bug")}<small>{t("set.diag.bugSub")}</small></span>
            <span><span class="m">{copied}</span> <button class="mini" onclick={copyInfo}>{t("tile.copy")}</button></span>
          </div>
          {#if resetAsk}
            <div class="row" role="alert">
              <span>{t("set.diag.resetAsk")}</span>
              <span><button class="mini" onclick={() => (resetAsk = false)}>{t("common.cancel")}</button> <button class="mini" onclick={reset}>{t("common.delete")}</button></span>
            </div>
          {:else}
            <div class="row">
              <span>{t("set.diag.reset")}<small>{t("set.diag.resetSub")}</small></span>
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
  h4 > .mini + .mini {
    margin-left: 0;
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
  /* status agent (✓ terpasang / ! belum dicoba) dan akun (sudah dipakai) */
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
  /* Gembala.pen 32: trusted-folder note at the end of the add-on page; switch label inside the MCP editor */
  .warnbox {
    margin: 18px 0 0;
    padding: 10px 12px;
    border: 1px solid var(--line);
    border-radius: 6px;
    color: var(--muted);
    font-size: 12px;
  }
  .row.col label.inline {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
  }
</style>
