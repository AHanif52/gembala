<script lang="ts">
  // Settings (FR-42): the screen and config.toml are equal. Every change is written to the file by the daemon (one writer),
  // then comes back as `config.changed`, so the screen always mirrors the file.
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getVersion } from "@tauri-apps/api/app";
  import { call } from "./daemon";
  import { envRows, envPatch, type EnvRow } from "./env";
  import { pretty, type Account, type Agent, type Config, type Profile, type Ui } from "./config";
  import { newer, type Upd } from "./update";
  import { lineDiff } from "./diff";
  import type { AiAsk, AiDraft } from "./harness";
  import { errText, loc, t, tn, tOr } from "./i18n/state.svelte";

  let {
    cfg,
    meta,
    onclose,
    onquitall,
    onlogin,
    workspaces = [],
    tg = { state: "off" },
    upd = null,
    drafts = [],
    onai = () => {},
    aiOpen = null,
    onpane = () => {},
    ondraft = () => {},
    start = "umum",
    oninstall = () => {},
    onguide,
    onreport = () => {},
  }: {
    cfg: Config;
    workspaces?: { id: string; name: string }[]; // FR-78: MCP and plugins per workspace
    tg?: any; // FR-80: `telegram.status` / `telegram.changed` from the daemon
    upd?: Upd | null; // FR-82: `update.status` / `update.changed`
    drafts?: AiDraft[]; // FR-77 (H5): Atur pakai AI drafts the page started; `ready` = the agent wrote the file
    onai?: (r: AiAsk) => void; // opens the agent tab and sends the brief
    aiOpen?: string | null; // palette / section header: open Atur pakai AI right away ("" = recipe, else a settings section)
    onpane?: (pane: number) => void;
    ondraft?: (draft: string) => void; // reviewed (saved or rejected): drop it from the list
    start?: string; // section to open on (the sidebar phone icon opens Telegram)
    meta: Record<number, { name: string; agent: string; profile: string; status?: string }>;
    onclose: () => void;
    onquitall: () => void;
    onlogin: (id: string, name: string) => void; // FR-61: open a pane on that account to log in
    oninstall?: (a: { agent: string; name: string; install: string }) => void; // FR-99 P1: install command typed into a new terminal tab
    onguide?: (agent: string) => (() => void) | undefined; // FR-99 P1: the agent's official guide, when there is one
    onreport?: () => void; // FR-100: open the report dialog
  } = $props();

  // FR-61: Claude Code accounts (own config folder through CLAUDE_CONFIG_DIR); Utama is always there
  type AccRow = { account: Account & { main?: boolean }; status: { used: boolean; email: string | null } };
  const SHARE_DEFAULT = ["CLAUDE.md", "settings.json", "skills", "commands", "agents", "plugins"];
  let accRows = $state<AccRow[]>([]);
  let shareOpts = $state<string[]>([]);
  let accForm = $state({ name: "", dir: "", share: [...SHARE_DEFAULT], error: "", info: "" });
  let accOpen = $state(false); // form closed; the Utama account is already the default
  let removeAsk = $state("");
  let accErr = $state("");
  const slug = (n: string) => n.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 32);
  async function loadAccounts() {
    const r = await call("account.list", {}).catch(() => null);
    if (r) [accRows, shareOpts, models, efforts] = [r.accounts, r.share, r.models ?? [], r.efforts ?? []];
  }
  // FR-61 (8) variant A: own settings (model, effort), Follow Utama/Own per item, and what always belongs to the account
  let models = $state<string[]>([]);
  let efforts = $state<string[]>([]);
  let accDetail = $state(""); // id of the account whose page is open
  let ownAsk = $state(""); // "<id>:<item>" waiting for the Own confirmation for settings.json/plugins
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
    // settings.json/plugins become Own: hooks, permissions, plugins, shared MCP stop following Utama, so ask first
    if (!follow && (items.includes("settings.json") || items.includes("plugins")) && ownAsk !== key) return void (ownAsk = key);
    ownAsk = "";
    for (const item of items) await updateAccount({ id: a.id, item, follow });
  }
  $effect(() => {
    void cfg.values.accounts; // reload when config changes
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
  // FR-29: edit a profile from the screen; one line = one argument / one env
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
      env: envRows(p.env), // env values aren't sent to the UI; fill them in again to change them
      secret: p.secret_env.map((x) => `${x.name}=${x.ref}`).join("\n"),
      summarize: p.summarize,
      notify: p.notify,
      permission: p.permission ?? "default",
      policy: p.policy ?? "auto",
      isNew: copy,
    };
  }

  /// Gembala.pen 30: "+ Profile" adds an empty profile; its id is made unique since ids are used in config.toml
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
    // only the env that's filled in; what's left empty keeps the old value in the file
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

  /// FR-48: delete Gembala's config, layout, hooks, and Keychain secret, then quit
  async function reset() {
    await call("daemon.reset").catch(() => {});
    await invoke("forget_secret").catch(() => {});
    await invoke("quit");
  }

  const SECTIONS = ["umum", "tampilan", "agent", "pintasan", "toolbar", "harness", "telegram", "keamanan", "proses", "diagnostik"] as const;
  // FR-60: Gembala-owned processes, refreshed every 2 seconds while this section is open
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
  /// pane name from the nearest parent shell
  function paneOf(p: Proc): string {
    const by = new Map(procs.map((x) => [x.pid, x]));
    for (let q: Proc | undefined = p; q; q = by.get(q.ppid)) if (panePid[q.pid]) return panePid[q.pid];
    return "";
  }
  const mb = (kb: number) => `${Math.round(kb / 1024)} MB`;
  // one row per pane (total RAM/CPU of all its processes), details open per pane; panes with only a shell at the bottom
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
  let sec = $state<(typeof SECTIONS)[number]>(SECTIONS.find((s) => s === start) ?? "umum");
  let failed = $state("");
  let keys = $state<[string, string, string][]>([]);
  let keyErrors = $state<string[]>([]);
  let keyEdit = $state<{ id: string; combo: string } | null>(null);
  // FR-55: Warp-style agent toolbar editor (drag between sides, × removes, available list); written to [toolbar]
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
  /// chip drag: pointer (HTML drag clashes with Tauri file drop), target = chip or zone under the cursor
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

  // order and names same as the app menu (lib.rs)
  const GROUPS = [["tab.", "menu.tab"], ["pane.", "menu.pane"], ["term.", "menu.view"], ["msg.", "menu.msg"]] as const;

  /// FR-39: combos written as text (WKWebView doesn't pass every ⌘ combo to the page)
  async function saveKey(id: string, combo: string) {
    failed = "";
    keyEdit = null;
    const r = await call("keys.set", { action: id, combo: combo.trim() }).catch((e) => ({ error: String(e) }));
    if (r.error) failed = errText(r.error, r.args);
  }
  let version = $state("");
  let agents = $state<Agent[]>([]);
  let detecting = $state(false);
  // Gembala.pen 31: account page, opened from the Claude Code row; the Agents nav goes back to the parent page
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
  /// Kind of hook the daemon installs (core/src/profile.rs `hook_files`); source of each agent's pane status.
  const HOOK: Record<string, string> = {
    claude: "set.agent.hook.claude",
    codex: "hook notify",
    omp: "hook omp-gembala.ts",
    opencode: "plugin opencode-gembala.js",
  }; // claude's label is translated (tOr), the rest are file names
  /// The Account row shows for profiles that run Claude: agent claude, or its command is claude.
  const runsClaude = (f: Form) => f.agent === "claude" || /^claude(\s|-|$)/.test(f.command.trim());
  /// env: one line = one NAME and value; lines without a value are dropped on save (old value stays)
  function envAdd() {
    if (form) form.env = [...form.env, { name: "", value: "" }];
  }
  function envDel(i: number) {
    if (form) form.env = form.env.filter((_, j) => j !== i);
  }

  /// FR-28: detection through the login shell; "Check again" forces a new detection
  async function detect(refresh: boolean) {
    detecting = true;
    agents = await call("agents.detect", { refresh }).catch(() => []);
    detecting = false;
  }
  const ui = $derived(cfg.values.ui);
  /// Gembala.pen 36: brake numbers come from the daemon (core/src/msg.rs through daemon.info), not retyped in the markup
  const rate = $derived(daemon?.msg?.rate_per_min);
  const hops = $derived(daemon?.msg?.hop_max);
  /// The audit log folder is next to config.toml (core/src/daemon.rs Audit::open); the daemon decides its path.
  const auditDir = $derived(cfg.path.replace(/\/[^/]+$/, "") + "/audit");
  /// The emergency button shortcut follows [keys]; its fallback matches MSG_KEYS in ui/src-tauri/src/lib.rs.
  const stopKey = $derived(pretty(keys.find(([id]) => id === "msg.stop")?.[2] || "Cmd+Shift+."));
  /// Folders next to config.toml opened in Finder; the allowed list is in REVEAL (ui/src-tauri/src/lib.rs).
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

  // [keys] changed = the menu is rebuilt on the Rust side; fetch the list in effect again
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

  // FR-74 / FR-77 (H5, pen 35/20/21): recipes, Atur pakai AI, review of a draft or of a file changed outside the app
  type Rec = { id: string; status: string; hash?: string; last_used?: number | null; recipe?: { name: string; steps: { id: string; name: string }[] } };
  const hn = $derived(cfg.values.harness ?? { max_rounds: 2, total_mins: 120, fallback_on: ["exit", "timeout", "unknown"], plan_gate: false, token_saver: false, notify_gate: true, notify_blocked: true, notify_done: true });
  const setHn = (key: string, value: unknown) => set(key as keyof Ui, value as never, "harness");
  const usedAt = (s: number) => new Date(s * 1000).toLocaleString(loc(), { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  let recipes = $state<Rec[]>([]);
  let delAsk = $state("");
  let ai = $state<{ target: AiAsk["target"]; id: string; fresh: string; ask: string; profile: string; section: string; file: string } | null>(null);
  let review = $state<{ id: string; who?: string; ask?: string; draft?: AiDraft; text: string; current: string; hash: string; errors: any[]; warnings: any[] } | null>(null);
  const agentProfiles = $derived(cfg.values.profiles.filter((p) => p.agent !== "shell"));
  const chain = (r: Rec) => (r.recipe?.steps ?? []).map((s) => s.name || s.id).join(" → ");
  async function loadRecipes() {
    recipes = (await call("recipe.list").catch(() => [])) as Rec[];
  }
  $effect(() => {
    if (sec === "harness") loadRecipes();
  });
  async function recipeAct(method: string, params: Record<string, unknown>) {
    failed = "";
    const r = await call(method, params).catch((e) => ({ error: String(e) }));
    if (r?.error) failed = errText(r.error, r.args);
    await loadRecipes();
    return !r?.error;
  }
  async function duplicate(id: string) {
    const taken = new Set(recipes.map((r) => r.id));
    const to = [`${id}-salinan`, ...[2, 3, 4, 5].map((n) => `${id}-salinan-${n}`)].find((x) => !taken.has(x));
    if (to) await recipeAct("recipe.duplicate", { id, to });
  }
  function openAi(id = "", target: AiAsk["target"] = "recipe", section = "") {
    sec = "harness";
    review = null;
    ai = { target, id, fresh: "", ask: "", profile: agentProfiles.find((p) => p.agent === "claude")?.id ?? agentProfiles[0]?.id ?? "", section, file: "AGENTS.md" };
  }
  const aiReady = $derived(!!ai && !!ai.ask.trim() && !!ai.profile && (ai.target !== "recipe" || !!(ai.id || slug(ai.fresh))));
  function startAi() {
    if (!ai || !aiReady) return;
    const id = ai.target === "recipe" ? ai.id || slug(ai.fresh) : ai.target === "config" ? "config.toml" : ai.file;
    onai({ target: ai.target, id, ask: ai.ask.trim(), profile: ai.profile, section: ai.section, file: ai.file });
    ai = null;
  }
  // FR-77: opened from the palette or a section header
  onMount(() => {
    if (aiOpen !== null) openAi("", aiOpen ? "config" : "recipe", aiOpen);
  });
  /// a recipe edited outside the app: its text, approved as is (no diff to show)
  async function reviewFile(id: string) {
    const r = await call("recipe.get", { id }).catch(() => null);
    if (r) review = { id, text: r.text, current: r.text, hash: r.hash, errors: r.errors ?? [], warnings: r.warnings ?? [] };
  }
  async function reviewDraft(d: AiDraft) {
    const r = await call(d.kind === "config" ? "config.draft_get" : "recipe.draft_get", { draft: d.draft, id: d.id }).catch(() => null);
    if (r?.exists) review = { id: d.id, who: d.who, ask: d.ask, draft: d, text: r.text, current: r.current, hash: r.hash, errors: r.errors ?? [], warnings: r.warnings ?? [] };
  }
  async function approveReview() {
    if (!review) return;
    const ok = review.draft
      ? await recipeAct(review.draft.kind === "config" ? "config.draft_apply" : "recipe.draft_apply", { draft: review.draft.draft, id: review.id, hash: review.hash })
      : await recipeAct("recipe.approve", { id: review.id, hash: review.hash });
    if (!ok) return;
    if (review.draft) ondraft(review.draft.draft);
    review = null;
  }
  // FR-82: "0.1.1 udah keluar · dicek 10 Nov 09.12" / "Udah versi terbaru · …" / never checked
  const updLine = $derived.by(() => {
    const at = upd?.at ?? upd?.failed_at;
    const when = at ? ` · ${t("set.upd.checked", { at: new Date(at * 1000).toLocaleString(loc(), { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }) })}` : "";
    if (!upd?.latest) return (upd?.failed_at ? t("set.upd.failed") : t("set.upd.never")) + when;
    return (newer(upd.latest, version) ? t("set.upd.new", { v: upd.latest }) : t("set.upd.latest")) + when;
  });

  // FR-80 (Gembala.pen 39/40): the user's own bot. Pairing answers at once; the result arrives as `telegram.changed` (tg)
  let tgToken = $state("");
  let tgBusy = $state(false);
  let tgErr = $state("");
  let tgDrop = $state(false); // "Lepas" clicked once, the next click unpairs
  const tgLinked = $derived(!!tg?.name && tg.state !== "off");
  $effect(() => {
    void tg;
    tgBusy = false;
  });
  async function tgPair() {
    [tgErr, tgBusy] = ["", true];
    const r = await call("telegram.pair", { token: tgToken }).catch((e) => ({ error: String(e) }));
    if (r?.error) [tgErr, tgBusy] = [errText(r.error), false];
    else tgToken = "";
  }
  async function tgUnpair() {
    if (!tgDrop) return (tgDrop = true);
    tgDrop = false;
    await call("telegram.unpair", {}).catch(() => {});
  }
  const setTg = (key: string, value: unknown) => set(key as keyof Ui, value as never, "telegram");
  const tgSkipped = (w: string) => (cfg.values.telegram?.snippet_off ?? []).includes(w);
  const tgSkip = (w: string) => {
    const cur = cfg.values.telegram?.snippet_off ?? [];
    setTg("snippet_off", tgSkipped(w) ? cur.filter((x) => x !== w) : [...cur, w]);
  };
  const TG_CMDS = ["plain", "reply", "photo", "to", "open", "status", "look"] as const;

</script>

{#snippet editor()}
  {#if form}
    {@const claude = runsClaude(form)}
    <!-- FR-29 + SEC-5: open profile card, Gembala.pen 30 row style -->
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
      <!-- FR-97: Simple mode keeps the machine-room pages out of the menu (still reachable by switching to Full) -->
      {#each SECTIONS.filter((k) => ui.mode !== "simple" || !["proses", "diagnostik"].includes(k)) as k}
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
      <!-- FR-77: "Atur pakai AI" in the head of each settings part, on its config.toml section -->
      {#if ["umum", "tampilan", "agent", "pintasan", "toolbar", "telegram"].includes(sec)}
        <div class="aihead"><button class="mini" onclick={() => openAi("", "config", sec)}>{t("hn.set.aiTitle")}</button></div>
      {/if}

      {#if sec === "umum"}
        <section>
          <!-- FR-97: who the screens are written for; only the wording and folding change, never a safeguard -->
          <div class="row">
            <span>{t("set.general.mode")}<small>{t("set.general.modeSub")}</small></span>
            <select aria-label={t("set.general.mode")} value={ui.mode === "simple" ? "simple" : "full"} onchange={(e) => set("mode", e.currentTarget.value as Ui["mode"])}>
              <option value="simple">{t("set.general.modeSimple")}</option>
              <option value="full">{t("set.general.modeFull")}</option>
            </select>
          </div>
          <div class="row">
            <span>{t("set.general.language")}<small>{t("set.general.languageSub")}</small></span>
            <select aria-label={t("set.general.language")} value={ui.language ?? "system"} onchange={(e) => set("language", e.currentTarget.value as Ui["language"])}>
              <option value="system">{t("set.general.languageSystem")}</option>
              <option value="id">Indonesia</option>
              <option value="en">English</option>
            </select>
          </div>
          <!-- FR-100: in both modes (Diagnostics is hidden in Simple) -->
          <div class="row">
            <span>{t("rep.setting")}<small>{t("rep.settingSub")}</small></span>
            <button class="mini" onclick={onreport}>{t("rep.title")}…</button>
          </div>
          <div class="row"><span>{t("set.general.daemon")}<small>{t("set.general.daemonSub")}</small></span><span class="m">{t("set.general.daemonState")}</span></div>
          <!-- FR-86: helpers an agent opened (FR-75) close after sitting quiet this long past their first reply -->
          <div class="row">
            <span>{t("set.general.helperIdle")}<small>{t("set.general.helperIdleSub")}</small></span>
            <select aria-label={t("set.general.helperIdle")} value={ui.helper_idle_close} onchange={(e) => set("helper_idle_close", +e.currentTarget.value)}>
              {#each [5, 15, 30, 60] as n}<option value={n}>{t("set.general.mins", { n })}</option>{/each}
              <option value={0}>{t("set.general.never")}</option>
            </select>
          </div>
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
        <!-- FR-58 (decision 4 Nov, mockup-blok-perintah option C): one global switch for the command block timeline;
             "ringkas output agent" tetap per profil (FR-56), bukan di grup ini -->
        <h4>{t("set.look.blocks")}</h4>
        <section>
          <div class="row">
            <span>{t("set.look.timeline")}<small>{t("set.look.timelineSub")}</small></span>
            <input type="checkbox" role="switch" aria-label={t("set.look.timeline")} checked={ui.command_timeline} onchange={(e) => set("command_timeline", e.currentTarget.checked)} />
          </div>
          <div class="row">
            <span>{t("set.look.codex")}<small>{t("set.look.codexSub")}</small></span>
            <input type="checkbox" role="switch" aria-label={t("set.look.codex")} checked={ui.codex_status} onchange={(e) => set("codex_status", e.currentTarget.checked)} />
          </div>
        </section>
      {:else if sec === "agent"}
        {#if agentPage === "akun"}
          <!-- Gembala.pen 31: the account page opens from the agent row; the Agents nav goes back to the parent page -->
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
              <!-- FR-61 (8), mockup-akun-setelan variant A -->
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
          <!-- Gembala.pen 32 (FR-78): add-ons per launch; agent config isn't touched (SEC-15) -->
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
            <!-- Cross-agent add-ons (Claude plugins + MCP, Codex MCP), so in the group head, not on the Claude row -->
            <button class="mini" onclick={() => (agentPage = "addon")}>Add-on ({addons.plugins.length + addons.mcp.length})…</button></h4>
          <section>
            {#each agents as a (a.agent)}
              <div class="row">
                <span
                  >{a.name}<small
                    >{a.installed
                      ? `${a.version ?? t("set.agent.noVersion")} · ${a.tested ? t("set.agent.tested") : t("set.agent.untested")}${HOOK[a.agent] ? ` · ${tOr(HOOK[a.agent], HOOK[a.agent])}` : ""}`
                      : ui.mode === "simple" ? t("set.agent.notYet") : t("set.agent.notFound", { install: a.install })}</small
                  ></span
                >
                <span class="acts">
                  {#if a.agent === "claude" && a.installed}<button class="mini" onclick={() => (agentPage = "akun")}>{t("set.agent.accounts", { n: accRows.length })}</button>{/if}
                  {#if !a.installed && a.install}
                    <!-- FR-99 P1: a way forward instead of a bare command line -->
                    <button class="mini" onclick={() => oninstall(a)}>{t("wel.install")}</button>
                    <button class="mini" onclick={() => navigator.clipboard?.writeText(a.install).catch(() => {})}>{t("wel.copy")}</button>
                    {@const guide = onguide?.(a.agent)}
                    {#if guide}<button class="mini" onclick={guide}>{t("wel.guide")} ↗</button>{/if}
                  {/if}
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
        <!-- FR-39: grouped like the menu; Tab 1–9 on one row; action id in the tooltip -->
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
        <!-- FR-55: drag chips to reorder or switch sides; × removes; available chips can be added back -->
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
      {:else if sec === "harness"}
        {#if review}
          <!-- pen 21: what gets saved is exactly this text (hash), nothing else -->
          {@const lines = lineDiff(review.current, review.text)}
          <h4>{t("hn.set.reviewTitle", { id: review.id.endsWith(".toml") ? review.id : `${review.id}.toml` })}{#if review.who}<span class="hint">{t("hn.set.by", { who: review.who })}</span>{/if}
            {#if review.draft}<button class="mini" onclick={() => review?.draft && onpane(review.draft.pane)}>{t("hn.set.openPane")}</button>{/if}</h4>
          {#if review.ask}<p class="note">{t("hn.set.need")}: {review.ask}</p>{/if}
          <pre class="rdiff">{#each lines as l, i (i)}<span class={l.t === "+" ? "add" : l.t === "-" ? "del" : ""}>{l.t} {l.s}
</span>{/each}</pre>
          <section>
            {#if review.errors.length}
              {#each review.errors as e}<div class="row"><span class="bad">✕ {errText(e)}</span></div>{/each}
            {:else}
              <div class="row"><span class="ok">✓ {t("hn.set.schemaOk")}</span></div>
            {/if}
            {#each review.warnings as w}<div class="row"><span>! {errText(w)}</span></div>{/each}
          </section>
          <p class="note">{review.draft?.kind === "config" ? t("hn.set.saveNoteCfg") : t("hn.set.saveNote")}</p>
          <div class="acts">
            {#if review.draft}<button class="mini" onclick={() => review && ((ai = { target: review.draft?.kind ?? "recipe", id: review.draft?.kind === "config" ? "" : review.id, fresh: "", ask: review.ask ?? "", profile: agentProfiles[0]?.id ?? "", section: "", file: "AGENTS.md" }), (review = null))}>{t("hn.set.editNeed")}</button>{/if}
            <button class="mini" onclick={() => (review?.draft && ondraft(review.draft.draft), (review = null))}>{review.draft ? t("hn.set.reject") : t("common.cancel")}</button>
            <button class="mini pri" disabled={review.errors.length > 0} onclick={approveReview}>{review.draft ? t("hn.set.approveSave") : t("hn.set.approve")}</button>
          </div>
        {:else if ai}
          <!-- pen 20: the agent works in a visible tab and only writes the draft file -->
          <h4>{t("hn.set.aiTitle")}</h4>
          <section>
            <div class="row"><span>{t("hn.set.what")}</span>
              <span class="seg">{#each [["recipe", "hn.set.whatRecipe"], ["config", "hn.set.whatCfg"], ["agent", "hn.set.whatAgent"]] as [k, l]}
                <button class:on={ai.target === k} onclick={() => ai && (ai.target = k as AiAsk["target"])}>{t(l as "hn.set.whatRecipe")}</button>{/each}</span></div>
            {#if ai.target === "recipe"}
              <div class="row"><span>{t("hn.set.which")}</span>
                <select aria-label={t("hn.set.which")} bind:value={ai.id}>
                  <option value="">{t("hn.set.newRecipe")}</option>
                  {#each recipes as r (r.id)}<option value={r.id}>{r.recipe?.name || r.id}</option>{/each}
                </select></div>
              {#if !ai.id}<div class="row"><span>{t("hn.set.newName")}</span><input class="tin" bind:value={ai.fresh} placeholder="review-pr" aria-label={t("hn.set.newName")} /></div>{/if}
            {:else if ai.target === "config"}
              <div class="row"><span>{t("hn.set.cfgPart")}<small>{t("hn.set.cfgPartSub")}</small></span>
                <select aria-label={t("hn.set.cfgPart")} bind:value={ai.section}>
                  <option value="">{t("hn.set.cfgAny")}</option>
                  {#each SECTIONS.filter((k) => !["harness", "keamanan", "proses", "diagnostik"].includes(k)) as k}<option value={k}>{t(`set.nav.${k}`)}</option>{/each}
                </select></div>
            {:else}
              <div class="row"><span>{t("hn.set.file")}<small>{t("hn.set.fileSub")}</small></span>
                <select aria-label={t("hn.set.file")} bind:value={ai.file}><option>AGENTS.md</option><option>CLAUDE.md</option></select></div>
            {/if}
            <div class="row col"><span>{t("hn.set.need")}</span>
              <textarea class="tin" rows="4" bind:value={ai.ask} placeholder={t("hn.set.needPh")} aria-label={t("hn.set.need")}></textarea></div>
            <div class="row"><span>{t("hn.set.with")}</span>
              <select aria-label={t("hn.set.with")} bind:value={ai.profile}>
                {#each agentProfiles as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
              </select></div>
          </section>
          <p class="note">{ai.target === "agent" ? t("hn.set.aiNoteAgent") : ai.target === "config" ? t("hn.set.aiNoteCfg") : t("hn.set.aiNote")}</p>
          <div class="acts">
            <button class="mini" onclick={() => (ai = null)}>{t("common.cancel")}</button>
            <button class="mini pri" disabled={!aiReady} onclick={startAi}>{t("hn.set.go")}</button>
          </div>
        {:else}
          {#if drafts.length}
            <h4>{t("hn.set.drafts")}</h4>
            <section>
              {#each drafts as d (d.draft)}
                <div class="row">
                  <span>{d.id}<small>{t("hn.set.by", { who: d.who })} · {d.ready ? t("hn.set.ready") : t("hn.set.waiting")}</small></span>
                  <span class="acts"><button class="mini" onclick={() => onpane(d.pane)}>{t("hn.set.openPane")}</button>
                    <button class="mini pri" disabled={!d.ready} onclick={() => reviewDraft(d)}>{t("hn.set.review")}</button></span>
                </div>
              {/each}
            </section>
          {/if}
          <h4>{t("hn.set.recipes")}<span class="hint">{t("hn.set.where")}</span>
            <button class="mini" onclick={() => reveal("recipes")}>{t("hn.set.showFolder")}</button>
            <button class="mini" onclick={() => openAi()}>{t("hn.set.newAi")}</button></h4>
          <section>
            {#each recipes as r (r.id)}
              <div class="row">
                <span>{r.recipe?.name || r.id} <span class="st {r.status}">{tOr(`hn.set.st.${r.status}`, r.status)}</span>{#if r.last_used}<span class="st">· {t("hn.set.used", { at: usedAt(r.last_used) })}</span>{/if}<small class="chain">{chain(r)}</small></span>
                {#if delAsk === r.id}
                  <span class="acts">{t("hn.set.delAsk")} <button class="mini" onclick={() => (delAsk = "")}>{t("common.cancel")}</button>
                    <button class="mini danger" onclick={() => ((delAsk = ""), recipeAct("recipe.delete", { id: r.id }))}>{t("hn.set.delete")}</button></span>
                {:else}
                  <span class="acts">
                    {#if r.status === "changed" || r.status === "needs_approval"}<button class="mini pri" onclick={() => reviewFile(r.id)}>{t("hn.set.review")}…</button>{/if}
                    <button class="mini" onclick={() => openAi(r.id)}>{t("hn.set.editAi")}</button>
                    <button class="mini" onclick={() => duplicate(r.id)}>{t("hn.set.dup")}</button>
                    <button class="mini" onclick={() => (delAsk = r.id)}>{t("hn.set.delete")}</button>
                  </span>
                {/if}
              </div>
            {:else}
              <div class="row"><span>{t("common.loading")}</span></div>
            {/each}
          </section>
          <!-- pen 35: values a new recipe gets when it leaves them out ([harness] in config.toml) -->
          <h4>{t("hn.set.defaults")}<span class="hint">{t("hn.set.defaultsSub")}</span></h4>
          <section>
            <div class="row"><span>{t("hn.set.rounds")}<small>{t("hn.set.roundsSub")}</small></span>
              <select aria-label={t("hn.set.rounds")} value={hn.max_rounds} onchange={(e) => setHn("max_rounds", +e.currentTarget.value)}>
                {#each [0, 1, 2, 3] as n}<option value={n}>{n}</option>{/each}
              </select></div>
            <div class="row"><span>{t("hn.set.total")}</span>
              <select aria-label={t("hn.set.total")} value={hn.total_mins} onchange={(e) => setHn("total_mins", +e.currentTarget.value)}>
                {#each [30, 60, 90, 120] as m}<option value={m}>{m < 60 ? t("hn.set.mins", { n: m }) : t("hn.set.hours", { n: m / 60 })}</option>{/each}
              </select></div>
            <div class="row"><span>{t("hn.set.fallback")}<small>{t("hn.set.fallbackSub")}</small></span>
              <span class="seg">{#each ["exit", "timeout", "unknown", "limit"] as k}
                <button class:on={hn.fallback_on.includes(k)} title={k === "limit" ? t("hn.set.limitOff") : ""}
                  onclick={() => setHn("fallback_on", hn.fallback_on.includes(k) ? hn.fallback_on.filter((x) => x !== k) : [...hn.fallback_on, k])}>{t(`hn.set.fb.${k}` as "hn.set.fb.exit")}</button>{/each}</span></div>
            <div class="row"><span>{t("hn.set.planGate")}<small>{t("hn.set.planGateSub")}</small></span>
              <input type="checkbox" role="switch" aria-label={t("hn.set.planGate")} checked={hn.plan_gate} onchange={(e) => setHn("plan_gate", e.currentTarget.checked)} /></div>
            <div class="row"><span>{t("hn.set.saver")}<small>{t("hn.set.saverSub")}</small></span>
              <input type="checkbox" role="switch" aria-label={t("hn.set.saver")} checked={hn.token_saver} onchange={(e) => setHn("token_saver", e.currentTarget.checked)} /></div>
          </section>
          <h4>{t("hn.set.notify")}</h4>
          <section>
            <div class="row"><span>{t("hn.set.nGate")}</span>
              <input type="checkbox" role="switch" aria-label={t("hn.set.nGate")} checked={hn.notify_gate} onchange={(e) => setHn("notify_gate", e.currentTarget.checked)} /></div>
            <div class="row"><span>{t("hn.set.nBlocked")}<small>{t("hn.set.nBlockedSub")}</small></span>
              <input type="checkbox" role="switch" aria-label={t("hn.set.nBlocked")} checked={hn.notify_blocked} onchange={(e) => setHn("notify_blocked", e.currentTarget.checked)} /></div>
            <div class="row"><span>{t("hn.set.nDone")}</span>
              <input type="checkbox" role="switch" aria-label={t("hn.set.nDone")} checked={hn.notify_done} onchange={(e) => setHn("notify_done", e.currentTarget.checked)} /></div>
          </section>
          <h4>{t("hn.set.hard")}<span class="hint">{t("hn.set.hardSub")}</span></h4>
          <section><div class="row"><span>{t("hn.set.hardText")}</span></div></section>
          <p class="note">{t("hn.set.subNote")}</p>
        {/if}
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
      {:else if sec === "telegram"}
        <!-- Gembala.pen 39/40, FR-80: own bot, no server in between; token in telegram.json (SEC-36), snippets SEC-37 -->
        {#if tgLinked}
          <section>
            <div class="row">
              <span>{t("set.tg.on", { bot: tg.bot || "bot" })}<small>{tg.state === "on" ? t("set.tg.bound", { name: tg.name }) : errText(tg.error || "offline")}</small></span>
              <button class="mini danger" onclick={tgUnpair}>{tgDrop ? t("set.tg.unpairSure") : t("set.tg.unpair")}</button>
            </div>
          </section>
          <h4>{t("set.tg.when")}</h4>
          <section>
            <div class="row">
              <span>{t("set.tg.away")}<small>{t("set.tg.awaySub")}</small></span>
              <select aria-label={t("set.tg.away")} value={cfg.values.telegram?.away_idle_mins ?? 5} onchange={(e) => setTg("away_idle_mins", +e.currentTarget.value)}>
                {#each [1, 5, 10, 30] as n}<option value={n}>{t("set.tg.mins", { n })}</option>{/each}
              </select>
            </div>
            <div class="row">
              <span>{t("set.tg.kinds")}<small>{t("set.tg.kindsSub")}</small></span>
              <button class="mini" onclick={() => (sec = "umum")}>{t("set.tg.kindsBtn")}</button>
            </div>
          </section>
          <h4>{t("set.tg.snip")}<span class="hint">{t("set.tg.snipHint")}</span></h4>
          <section>
            <div class="row">
              <span>{t("set.tg.snipOn")}<small>{t("set.tg.snipOnSub")}</small></span>
              <input type="checkbox" role="switch" aria-label={t("set.tg.snipOn")} checked={cfg.values.telegram?.snippet ?? true} onchange={(e) => setTg("snippet", e.currentTarget.checked)} />
            </div>
            {#if workspaces.length}
              <div class="row col">
                <span>{t("set.tg.skip")}<small>{t("set.tg.skipSub")}</small></span>
                <div class="tgws" role="group" aria-label={t("set.tg.skip")}>
                  {#each workspaces as w (w.id)}<button class="mini" class:on={tgSkipped(w.name)} aria-pressed={tgSkipped(w.name)} onclick={() => tgSkip(w.name)}>{w.name}</button>{/each}
                </div>
              </div>
            {/if}
          </section>
          <h4>{t("set.tg.cmds")}</h4>
          <section>
            {#each TG_CMDS as k}
              <div class="row"><span class="m nowrap">{t(`set.tg.cmd.${k}`)}</span><span class="state">{t(`set.tg.cmd.${k}Sub`)}</span></div>
            {/each}
          </section>
          <p class="note">{t("set.tg.cant")}</p>
          <p class="warnbox">⚠ {t("set.tg.riskOn")}</p>
        {:else}
          <p class="note">{t("set.tg.intro")}</p>
          <section>
            <div class="row">
              <span>1 · {t("set.tg.s1")}<small>{t("set.tg.s1Sub")}</small></span>
              <button class="mini" onclick={() => invoke("open_url", { url: "https://t.me/BotFather" })}>{t("set.tg.botfather")}</button>
            </div>
            <div class="row col">
              <span>2 · {t("set.tg.s2")}<small>{t("set.tg.s2Sub")}</small></span>
              <div class="tgrow">
                <input class="tin mono" type="password" bind:value={tgToken} placeholder="8123456789:AA…" spellcheck="false" autocomplete="off" aria-label={t("set.tg.s2")} />
                <button class="mini pri" disabled={!tgToken.trim() || tgBusy} onclick={tgPair}>{tgBusy ? t("set.tg.checking") : t("set.tg.check")}</button>
              </div>
            </div>
            <div class="row col">
              <span>3 · {t("set.tg.s3")}<small>{tg.state === "pairing" && tg.link ? t("set.tg.s3Sub", { bot: tg.bot }) : t("set.tg.s3Wait")}</small></span>
              {#if tg.state === "pairing" && tg.link}
                <div class="tgrow">
                  <span class="m tgcode">/start {tg.code}</span>
                  <button class="mini" onclick={() => invoke("copy_text", { text: `/start ${tg.code}` })}>{t("set.tg.copy")}</button>
                  <button class="mini pri" onclick={() => invoke("open_url", { url: tg.link })}>{t("set.tg.open")}</button>
                </div>
                <small class="m">{t("set.tg.waiting", { n: Math.ceil((tg.code_secs ?? 600) / 60) })}</small>
              {:else if tg.state === "pairing"}
                <small class="m">{t("set.tg.expired")}</small>
              {/if}
            </div>
          </section>
          {#if tgErr || tg.error}<span class="m err" role="alert">{tgErr || errText(tg.error)}</span>{/if}
          <p class="warnbox">⚠ {t("set.tg.risk")}</p>
        {/if}
      {:else if sec === "keamanan"}
        <!-- Gembala.pen 36: values come from the daemon's rules, not retyped; cross-workspace exception
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
          <div class="row"><span>{t("set.sec.net")}<small>{t("set.sec.netSub")}</small></span><span class="m">{[tg?.state === "on" ? "Telegram" : "", cfg.values.update?.check !== false ? "GitHub" : ""].filter(Boolean).join(" · ") || t("set.sec.off")}</span></div>
        </section>
      {:else}
        <section>
          <div class="row"><span>{t("set.diag.versions")}</span><span class="m">{version} · {daemon?.daemon ?? "?"} · {daemon?.proto ?? "?"}</span></div>
          <div class="row"><span>{t("set.upd.via")}</span><span class="m">{upd?.brew ? "Homebrew" : t("set.upd.source")}</span></div>
          <!-- FR-82 (mockup-update B): status, check now, release page -->
          <div class="row">
            <span>{t("set.diag.release")}<small>{updLine}</small></span>
            <span class="acts">
              <button class="mini" onclick={() => call("update.check").catch(() => {})}>{t("set.upd.check")}</button>
              <button class="mini" onclick={() => invoke("open_url", { url: "https://github.com/AHanif52/gembala/releases" })}>{t("set.diag.releaseBtn")}</button>
            </span>
          </div>
          <div class="row">
            <span>{t("set.upd.auto")}<small>{t("set.upd.autoSub")}</small></span>
            <input type="checkbox" role="switch" aria-label={t("set.upd.auto")} checked={cfg.values.update?.check !== false} onchange={(e) => set("check" as keyof Ui, e.currentTarget.checked as never, "update")} />
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
            <span>{t("set.diag.bug")}<small>{t("rep.settingSub")}</small></span>
            <button class="mini" onclick={onreport}>{t("rep.title")}…</button>
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
    background: rgba(0, 0, 0, 0.5); /* the terminal behind really recedes */
  }
  /* FR-39: tidy shortcut list: name on the left, actions show on hover, combos right-aligned in a fixed column */
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
    height: calc(100vh - 80px); /* Gembala.pen 28: 40 px from the top and bottom edges */
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
  /* FR-61 (8): account page, three groups */
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
  /* Gembala.pen 28: macOS-style switch; the knob is drawn as a background so it stays one element
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
  /* Gembala.pen 28/30: bold group title above the card, gray note continuing to its right,
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
  /* Gembala.pen 31: back to the parent page from the account sub-page */
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
  /* agent state (✓ installed / ! not tried yet) and account (used) */
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
  /* open profile card: label–control rows, not a stacked form */
  .pcard {
    border-top: 1px solid var(--line);
    padding-bottom: 2px;
  }
  .pcard .row {
    padding-left: 24px;
  }
  /* closing row of the profile card: main action right-aligned, delete/reset on the left */
  .pcard .row.foot {
    justify-content: flex-end;
  }
  .pcard .row.foot .link {
    margin-right: auto;
  }
  /* FR-77 review (pen 21) and recipe rows (pen 35) */
  .rdiff {
    max-height: 320px;
    margin: 0 0 10px;
    padding: 8px 10px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
    font: 12px "SF Mono", Menlo, monospace;
    white-space: pre-wrap;
  }
  .rdiff .add {
    color: #86efac;
  }
  .rdiff .del {
    color: #fca5a5;
  }
  .ok {
    color: #86efac;
  }
  .bad {
    color: #fca5a5;
  }
  .st {
    margin-left: 6px;
    font-size: 11px;
    color: var(--muted);
  }
  .st.approved {
    color: #86efac;
  }
  .st.changed,
  .st.needs_approval {
    color: #fbbf24;
  }
  .st.broken {
    color: #fca5a5;
  }
  .mini:disabled {
    opacity: 0.45;
  }
  .aihead {
    display: flex;
    justify-content: flex-end;
    margin-bottom: 6px;
  }
  .seg button:disabled {
    opacity: 0.45;
  }
  .chain {
    text-transform: uppercase;
    letter-spacing: 0.03em;
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
  /* FR-80: token field / pairing code with buttons at their own width; unpair; workspace toggles that wrap */
  .tgrow {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .tgrow .tin {
    flex: 1;
  }
  .tgrow .mini,
  .row > .mini {
    white-space: nowrap;
  }
  .tgcode {
    margin-right: auto;
    font-size: 13px;
  }
  .mini.danger {
    color: var(--tx-red, #ef4444);
  }
  /* many workspaces (real app: 7+): toggles go under the label and wrap instead of pushing the modal sideways */
  .tgws {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .tgws .mini.on {
    border-color: var(--blue, #3b82f6);
    color: var(--blue, #3b82f6);
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
