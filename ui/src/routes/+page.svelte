<script lang="ts">
  // Fase 1: workspace → tab → pohon split pane (FR-1, 37, 38, 43–46, 50, 52). Tata letak disimpan daemon (FR-45).
  import { onMount } from "svelte";
  import Tile from "$lib/Tile.svelte";
  import Settings from "$lib/Settings.svelte";
  import Changes from "$lib/Changes.svelte";
  import OutputView from "$lib/OutputView.svelte";
  import Messages from "$lib/Messages.svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { getVersion } from "@tauri-apps/api/app";
  import { UI_DEFAULT, type Agent, type Config, type Profile } from "$lib/config";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { call, b64 } from "$lib/daemon";
  import {
    dividers, neighbor, nudge, panes, rects, remove, replace, setRatio, split, valid,
    type Arrow, type Dir, type Divider, type Node,
  } from "$lib/layout";

  type Tab = { id: string; title: string; tree: Node; focus: number };
  type Ws = { id: string; name: string; root: string; tabs: Tab[] };
  /// Data per pane untuk dipulihkan (FR-45): profil dan folder, tanpa isi layar (SEC-13).
  type Meta = {
    profile: string;
    cwd: string | null;
    agent: string;
    name: string;
    busy?: string | null;
    status?: string;
    session?: string; // FR-53: id sesi Gembala (Claude Code)
    git?: any; // FR-55: ringkasan git di toolbar
    pastes?: string[]; // SEC-34: gambar tempelan, dihapus saat pane ditutup
    output?: boolean; // FR-56: ada output sesi agent sebelumnya
    since?: number; // FR-40: kapan proses di pane shell mulai jalan
    sig?: string; // FR-51: sidik profil saat pane dibuat
    zoom?: number; // FR-34: zoom pane ini di atas zoom global
    policy?: string; // SEC-5
    readable?: boolean; // SEC-21
  };
  const AGENTS = ["claude", "codex", "opencode", "omp"];
  // FR-4/52: tanda status tab, tidak hanya warna (bagian 11 BRD)
  const MARK: Record<string, [string, string]> = { blocked: ["!", "butuh izin"], done: ["✓", "selesai"], working: ["•", "bekerja"] };
  const rank = (s?: string) => ["working", "done", "blocked"].indexOf(s ?? "");
  const tabStatus = (t: Tab): string => panes(t.tree).reduce((a, p) => (rank(meta[p]?.status) > rank(a) ? (meta[p]?.status ?? "") : a), "");

  let wss = $state<Ws[]>([]);
  let at = $state({ ws: 0, tab: 0 });
  let meta = $state<Record<number, Meta>>({});
  let dead = $state<Record<number, true>>({}); // pane dari run daemon lain atau sudah keluar (id negatif)
  let boot = $state(""); // state: layout disimpan ulang saat daemon baru dijalankan
  let zoomed = $state(false);
  let confirm = $state<{ pane: number; name: string } | null>(null);
  let quitAsk = $state(0); // jumlah pane yang masih menjalankan proses (FR-46)
  let linkAsk = $state(""); // SEC-14: URL asli ditampilkan sebelum dibuka
  let zoom = $state(0); // FR-34 ⌘=/⌘- sementara di atas ukuran config; ponytail: zoom per pane menyusul
  let cfg = $state<Config | null>(null);
  let settingsOpen = $state(false);
  let picker = $state<{ items: (Profile & { missing: boolean })[]; sel: number } | null>(null);
  let sidebar = $state(true);
  let filter = $state("");
  let search = $state<HTMLInputElement>();
  let editing = $state<{ kind: "tab" | "ws" | "pane"; id: string; value: string } | null>(null);
  let renameError = $state("");
  let newWs = $state<{ name: string; root: string } | null>(null);
  let welcome = $state<Agent[] | null>(null); // FR-43: kartu ringkas saat pertama kali dibuka
  let upgrade = $state<{ daemon: string; app: string; panes: number } | null>(null); // FR-21
  let wsAsk = $state<{ wi: number; busy: number } | null>(null); // FR-44: hapus workspace yang masih jalan
  let rich = $state<{ pane: number; text: string } | null>(null); // FR-14
  let changes = $state<{ pane: number; diff: any } | null>(null); // FR-22
  let outputOf = $state<number | null>(null); // FR-56

  const ui = $derived(cfg?.values.ui ?? UI_DEFAULT);
  const ws = $derived(wss[at.ws]);
  const tab = $derived(ws?.tabs[at.tab]);
  // Fase 2: laci pesan, kirim manual, data mesin pesan dari daemon
  let drawer = $state(false);
  let msgData = $state<any>(null);
  let compose = $state<{ to: number | null; body: string; force: boolean; error: string } | null>(null);
  async function refreshMsgs() {
    msgData = await call("msg.list").catch(() => msgData);
  }
  const pendingFor = (pane: number) => (msgData?.pending ?? []).filter((m: any) => m.to === meta[pane]?.name);
  const wsIdOf = (pane: number) => wss.find((w) => w.tabs.some((t) => panes(t.tree).includes(pane)))?.id ?? ws?.id;
  /// panel status (FR-10): semua pane di workspace aktif
  const rows = $derived(
    (ws?.tabs ?? [])
      .flatMap((t) => panes(t.tree))
      .filter((p) => !dead[p] && meta[p])
      .map((p) => ({ pane: p, name: meta[p].name, agent: meta[p].agent || "shell", status: meta[p].status, policy: meta[p].policy, readable: meta[p].readable })),
  );

  async function sendCompose() {
    if (!compose || compose.to === null) return;
    try {
      await call("msg.send", { to: compose.to, body: compose.body, force: compose.force });
      compose = null;
      refreshMsgs();
    } catch (e) {
      if (compose) compose.error = e === "target_blocked" ? "Target sedang butuh izin; jawab langsung di pane itu" : e === "target_busy" ? "Target sedang bekerja; centang paksa bila yakin" : String(e);
    }
  }

  $effect(() => {
    const r = document.documentElement;
    if (ui.theme === "system") delete r.dataset.theme;
    else r.dataset.theme = ui.theme;
  });
  const tiles: Record<number, Tile> = $state({});
  let error = $state("");
  let box = $state<HTMLDivElement>();

  const lines = $derived(tab && !zoomed ? dividers(tab.tree) : []);
  const uid = () => Math.random().toString(36).slice(2, 10);
  const base = (p: string | null | undefined) => (p ? p.split("/").filter(Boolean).at(-1) ?? "/" : "");
  const home = (p: string | null | undefined) => (p ?? "").replace(/^\/Users\/[^/]+/, "~");
  const title = (t: Tab) => t.title || base(meta[t.focus]?.cwd) || "~";
  /// Urutan global tab untuk ⌘1–⌘9
  const order = $derived(wss.flatMap((w, wi) => w.tabs.map((t, ti) => ({ wi, ti, t }))));

  async function createPane(opts: Record<string, unknown> = {}): Promise<number> {
    const profile = (opts.profile as string) ?? "shell";
    const agent = cfg?.values.profiles.find((p) => p.id === profile)?.agent ?? "shell";
    // FR-53: Claude Code menerima id sesi dari luar; dipakai lagi saat pane dijalankan ulang
    const session = (opts.session as string) ?? (agent === "claude" ? crypto.randomUUID() : undefined);
    const r = await call("pane.create", { workspace: ws?.id, ...opts, session });
    meta[r.pane] = { profile, cwd: null, agent, name: r.name, session, sig: sig(profile) };
    return r.pane;
  }

  const sig = (id: string) => JSON.stringify(cfg?.values.profiles.find((p) => p.id === id) ?? null);

  /// FR-51: pane yang profilnya berubah diganti pane baru dari profil terbaru di posisi yang sama
  const relaunching = new Set<number>(); // pane lama yang sedang diganti: pane.exit-nya tidak menutup tab
  async function relaunch(old: number) {
    const m = meta[old];
    if (!m || relaunching.has(old)) return;
    relaunching.add(old);
    // tutup dulu: agent lama dan baru tidak boleh memakai sesi yang sama bersamaan
    await call("pane.close", { pane: old }).catch(() => {});
    await new Promise((r) => setTimeout(r, 400));
    const id = await createPane({
      workspace: wsIdOf(old),
      profile: m.profile === "shell" ? undefined : m.profile,
      cwd: m.cwd ?? undefined,
      session: m.session,
      resume: AGENTS.includes(m.agent),
    });
    for (const w of wss)
      for (const t of w.tabs)
        if (panes(t.tree).includes(old)) {
          t.tree = replace(t.tree, old, id);
          if (t.focus === old) t.focus = id;
        }
    delete meta[old];
    relaunching.delete(old);
  }

  async function refreshGit(pane: number) {
    if (!meta[pane] || dead[pane] || ![...AGENTS, "custom"].includes(meta[pane].agent)) return;
    const g = await call("git.status", { pane }).catch(() => null);
    if (g && meta[pane]) meta[pane].git = g;
  }

  async function openChanges(pane: number) {
    if (changes?.pane === pane) return void (changes = null);
    changes = { pane, diff: null };
    refreshGit(pane);
    const diff = await call("git.diff", { pane }).catch(() => ({ repo: false }));
    if (changes?.pane === pane) changes.diff = diff;
  }

  /// FR-14: teks dikirim sebagai bracketed paste + satu Enter. ESC, kontrol C0 selain baris baru/tab,
  /// dan kontrol C1 dibuang supaya isi tidak bisa menutup paste lebih awal.
  function sendRich() {
    if (!rich) return;
    const clean = rich.text.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "");
    const bytes = new TextEncoder().encode(`\x1b[200~${clean}\x1b[201~\r`);
    call("pane.write", { pane: rich.pane, data: b64(bytes) });
    rich = null;
  }

  /// FR-15: path asli (symlink diselesaikan) ber-quote ditempel ke prompt sebagai bracketed paste, tanpa Enter
  async function attach(pane: number, paths: string[], resolved = false) {
    const real: string[] = resolved ? paths : await invoke<string[]>("resolve_paths", { paths }).catch(() => []);
    if (!real.length || dead[pane]) return;
    const text = real.map(quote).join(" ") + " ";
    call("pane.write", { pane, data: b64(new TextEncoder().encode(`\x1b[200~${text}\x1b[201~`)) });
  }

  const quote = (p: string) => `'${p.replaceAll("'", "'\\''")}'`;

  async function newTab(profile?: string, wi = at.ws) {
    const w = wss[wi];
    const from = wi === at.ws ? tab?.focus : undefined;
    const id = await createPane({ profile, from_pane: from, cwd: from === undefined ? w.root : undefined, workspace: w.id });
    w.tabs.push({ id: uid(), title: "", tree: { pane: id }, focus: id });
    at = { ws: wi, tab: w.tabs.length - 1 };
    zoomed = false;
  }

  async function splitFocused(dir: Dir) {
    if (!tab) return;
    const id = await createPane({ from_pane: tab.focus });
    tab.tree = split(tab.tree, tab.focus, dir, id);
    tab.focus = id;
    zoomed = false;
  }

  /// FR-38: konfirmasi bila ada proses selain shell diam
  async function askClose(pane: number) {
    if (dead[pane]) return onexit(pane);
    const info = await call("pane.info", { pane });
    if (info.busy) confirm = { pane, name: info.name ?? "proses" };
    else await call("pane.close", { pane });
  }

  /// Pane keluar: hilang dari tabnya; tab kosong ditutup; workspace tanpa tab dapat shell baru.
  async function onexit(pane: number) {
    if (relaunching.has(pane)) return;
    if (confirm?.pane === pane) confirm = null;
    for (const [wi, w] of wss.entries()) {
      const ti = w.tabs.findIndex((t) => panes(t.tree).includes(pane));
      if (ti < 0) continue;
      const t = w.tabs[ti];
      const next = neighbor(t.tree, pane, "left") ?? neighbor(t.tree, pane, "up");
      const rest = remove(t.tree, pane);
      if (rest) {
        t.tree = rest;
        if (t.focus === pane || !panes(rest).includes(t.focus)) t.focus = next ?? panes(rest)[0];
      } else {
        w.tabs.splice(ti, 1);
        if (at.ws === wi && at.tab >= ti) at.tab = Math.max(0, at.tab - 1);
      }
      if (at.ws === wi) zoomed = false;
      if (meta[pane]?.pastes?.length) invoke("forget_paste", { paths: meta[pane].pastes }).catch(() => {});
      delete meta[pane];
      delete dead[pane];
      if (!w.tabs.length) await newTab(undefined, wi);
      return;
    }
  }

  /// FR-45: pane mati diganti pane baru dari profil dan folder yang sama
  async function restart(old: number) {
    const m = meta[old];
    const id = await createPane({
      profile: m?.profile === "shell" ? undefined : m?.profile,
      cwd: m?.cwd ?? undefined,
      session: m?.session,
      resume: AGENTS.includes(m?.agent ?? ""), // FR-53: lanjutkan percakapan pane ini
      workspace: wsIdOf(old),
    });
    for (const w of wss)
      for (const t of w.tabs)
        if (panes(t.tree).includes(old)) {
          t.tree = replace(t.tree, old, id);
          if (t.focus === old) t.focus = id;
        }
    delete meta[old];
    delete dead[old];
  }

  /// Kriteria 17: daemon mati paksa = semua pane tampil sebagai pane mati (tombol jalankan ulang),
  /// lalu daemon baru dijalankan lewat panggilan berikutnya.
  let lost = false;
  async function onLost() {
    if (lost) return;
    lost = true;
    let next = Math.min(-1, ...Object.keys(meta).map(Number), ...Object.keys(dead).map(Number)) - 1;
    for (const w of wss)
      for (const t of w.tabs)
        for (const p of panes(t.tree)) {
          if (dead[p]) continue;
          const id = next--;
          t.tree = replace(t.tree, p, id);
          if (t.focus === p) t.focus = id;
          meta[id] = meta[p] ?? { profile: "shell", cwd: null, agent: "", name: "" };
          delete meta[p];
          dead[id] = true;
        }
    confirm = null;
    changes = null;
    rich = null;
    await new Promise((r) => setTimeout(r, 500));
    try {
      boot = (await call("daemon.info")).boot;
      cfg = await call("config.get");
    } catch (e) {
      error = `daemon tidak bisa dijalankan lagi: ${e}`;
    }
    lost = false;
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
    if (p) newTab(p.id === "shell" ? undefined : p.id);
  }

  async function openLink() {
    const url = linkAsk;
    linkAsk = "";
    await invoke("open_url", { url }).catch(() => {});
  }

  function goto(i: number) {
    const o = order[i];
    if (!o) return;
    at = { ws: o.wi, tab: o.ti };
    zoomed = false;
  }

  /// Aksi dari menu (key equivalent native, lihat lib.rs). `⌘` milik app, sisanya ke terminal (FR-39).
  let last = { id: "", at: 0 };
  function run(id: string) {
    if (id === "app.quit_all") return askQuitAll();
    if (id === "app.settings") return void (settingsOpen = !settingsOpen);
    if (settingsOpen || picker || editing || newWs || rich) return;
    if (!tab || confirm || quitAsk || linkAsk || wsAsk) return;
    // satu tombol bisa sampai lewat menu dan keydown sekaligus; jangan jalan dua kali
    if (id === last.id && performance.now() - last.at < 150) return;
    last = { id, at: performance.now() };
    const [, group, arg] = id.split(".");
    const cur = order.findIndex((o) => o.wi === at.ws && o.ti === at.tab);
    if (id === "msg.drawer") return void ((drawer = !drawer), drawer && refreshMsgs());
    if (id === "msg.stop") return void call("msg.pause", { on: !msgData?.paused }).then(refreshMsgs);
    if (id === "msg.compose") {
      const others = rows.filter((r) => r.pane !== tab.focus && r.agent !== "shell");
      compose = { to: others[0]?.pane ?? null, body: "", force: false, error: "" };
      return;
    }
    if (id === "tab.new") openPicker();
    else if (id === "tab.sidebar") sidebar = !sidebar;
    else if (id === "tab.prev") goto((cur + order.length - 1) % order.length);
    else if (id === "tab.next") goto((cur + 1) % order.length);
    else if (group === "goto") goto(+arg - 1);
    else if (group === "split") splitFocused(arg === "down" ? "col" : "row");
    else if (group === "focus" && !zoomed) tab.focus = neighbor(tab.tree, tab.focus, arg as Arrow) ?? tab.focus;
    else if (group === "resize" && !zoomed) tab.tree = nudge(tab.tree, tab.focus, arg as Arrow);
    else if (id === "pane.zoom.toggle") zoomed = !zoomed;
    else if (id === "pane.move.newtab") moveToNewTab();
    else if (id === "pane.input.rich") rich = { pane: tab.focus, text: "" };
    else if (id === "pane.diff.toggle") openChanges(tab.focus);
    else if (id === "pane.proc.kill") call("pane.kill", { pane: tab.focus });
    else if (id === "pane.proc.close") askClose(tab.focus);
    else if (id === "term.find") tiles[tab.focus]?.openFind();
    else if (id === "term.clear") tiles[tab.focus]?.clearScreen();
    else if (id === "term.zoom.in") zoom = Math.min(16, zoom + 1);
    else if (id === "term.zoom.out") zoom = Math.max(-6, zoom - 1);
    else if (id === "term.zoom.reset") zoom = 0;
    else if (id.startsWith("term.zoom.pane") && meta[tab.focus]) {
      const m = meta[tab.focus];
      m.zoom = arg === "pane_reset" ? 0 : Math.max(-6, Math.min(16, (m.zoom ?? 0) + (arg === "pane_in" ? 1 : -1)));
    }
    else if (id === "tab.search") {
      sidebar = true;
      setTimeout(() => search?.focus(), 0);
    }
  }

  const stop = (e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation();
  };

  /// Kartu konfirmasi, pemilih profil, dan ⌘⇧↩ yang menangkap tombol; lainnya milik terminal.
  function onKey(e: KeyboardEvent) {
    if (picker) {
      const n = picker.items.length;
      if (e.key === "ArrowDown") picker.sel = (picker.sel + 1) % n;
      else if (e.key === "ArrowUp") picker.sel = (picker.sel + n - 1) % n;
      else if (e.key === "Enter") pick(picker.sel);
      else if (e.key === "Escape") picker = null;
      else if (/^[1-9]$/.test(e.key)) pick(+e.key - 1);
      return stop(e);
    }
    if (settingsOpen && !quitAsk) {
      // Esc di kotak isian Pengaturan = batal isian, bukan tutup Pengaturan
      const typing = document.activeElement instanceof HTMLInputElement || document.activeElement instanceof HTMLTextAreaElement;
      if (e.key === "Escape" && !typing) {
        settingsOpen = false;
        e.preventDefault();
      }
      return; // kontrol di Pengaturan menerima tombol seperti biasa
    }
    if (rich) return; // textarea Rich Input menangani tombolnya sendiri
    if (compose) {
      if (e.key === "Escape") (compose = null), stop(e);
      return;
    }
    if (drawer && e.key === "Escape") {
      drawer = false;
      return stop(e);
    }
    if (changes && e.key === "Escape") {
      changes = null;
      return stop(e);
    }
    if (editing || newWs) {
      if (e.key === "Escape") {
        editing = null;
        newWs = null;
        stop(e);
      }
      return; // kotak isian di sidebar menerima tombol seperti biasa
    }
    if (renameError && (e.key === "Enter" || e.key === "Escape")) {
      renameError = "";
      return stop(e);
    }
    if (wsAsk) {
      if (e.key === "Enter") removeWorkspace(wsAsk.wi);
      if (e.key === "Escape") wsAsk = null;
      return stop(e);
    }
    if (welcome && (e.key === "Escape" || e.key === "Enter")) {
      welcome = null;
      return stop(e);
    }
    if (linkAsk) {
      if (e.key === "Enter") openLink();
      if (e.key === "Escape") linkAsk = "";
      return stop(e);
    }
    if (quitAsk) {
      if (e.key === "Enter") quitAll();
      if (e.key === "Escape") quitAsk = 0;
      return stop(e);
    }
    // WKWebView memakan ⌘⇧↩ sebelum menu sempat menerimanya
    if (!confirm && e.metaKey && e.shiftKey && e.key === "Enter") {
      run("pane.zoom.toggle");
      return stop(e);
    }
    if (!confirm) return;
    if (e.key === "Enter") call("pane.close", { pane: confirm.pane });
    if (e.key === "Enter" || e.key === "Escape") confirm = null;
    stop(e);
  }

  function saveEdit() {
    if (!editing) return;
    const v = editing.value.trim().slice(0, 60);
    if (editing.kind === "pane") {
      // FR-3: nama unik dicek daemon; gagal = nama lama tetap
      const pane = +editing.id;
      editing = null;
      call("pane.rename", { pane, name: v.toLowerCase() })
        .then((r) => meta[pane] && (meta[pane].name = r.name))
        .catch((e) => (renameError = e === "name_taken" ? `Nama "${v}" sudah dipakai pane lain` : "Nama hanya huruf kecil, angka, dan '-'"));
      return;
    }
    if (editing.kind === "tab") {
      const t = wss.flatMap((w) => w.tabs).find((t) => t.id === editing!.id);
      if (t) t.title = v; // kosong = kembali ke nama folder
    } else {
      const w = wss.find((w) => w.id === editing!.id);
      if (w && v) w.name = v;
    }
    editing = null;
  }

  /// FR-37: pane fokus pindah ke tab baru di workspace yang sama
  function moveToNewTab() {
    if (!tab || panes(tab.tree).length < 2) return;
    const pane = tab.focus;
    const rest = remove(tab.tree, pane);
    if (!rest) return;
    tab.tree = rest;
    tab.focus = panes(rest)[0];
    ws.tabs.push({ id: uid(), title: "", tree: { pane }, focus: pane });
    at = { ws: at.ws, tab: ws.tabs.length - 1 };
    zoomed = false;
  }

  /// FR-44: urutkan workspace
  function moveWs(wi: number, d: number) {
    const j = wi + d;
    if (j < 0 || j >= wss.length) return;
    const cur = wss[at.ws].id;
    [wss[wi], wss[j]] = [wss[j], wss[wi]];
    at = { ws: wss.findIndex((w) => w.id === cur), tab: at.tab };
  }

  /// FR-44: workspace = satu folder project; tab barunya mulai di folder itu
  async function addWorkspace() {
    if (!newWs) return;
    const name = newWs.name.trim().slice(0, 60) || base(newWs.root) || "workspace";
    const root = newWs.root.trim() || "~";
    newWs = null;
    wss.push({ id: uid(), name, root, tabs: [] });
    await newTab(undefined, wss.length - 1);
  }

  async function askRemoveWorkspace(wi: number) {
    if (wss.length < 2) return;
    const ids = wss[wi].tabs.flatMap((t) => panes(t.tree)).filter((p) => !dead[p]);
    const infos = await Promise.all(ids.map((pane) => call("pane.info", { pane }).catch(() => ({}))));
    const busy = infos.filter((i: any) => i.busy).length;
    if (busy) wsAsk = { wi, busy };
    else removeWorkspace(wi);
  }

  function removeWorkspace(wi: number) {
    wsAsk = null;
    const ids = wss[wi].tabs.flatMap((t) => panes(t.tree));
    wss.splice(wi, 1);
    at = { ws: 0, tab: 0 };
    for (const p of ids) {
      if (dead[p]) delete dead[p];
      else call("pane.close", { pane: p }).catch(() => {});
      delete meta[p];
    }
  }

  function drag(d: Divider, e: PointerEvent) {
    if (!box) return;
    const b = box.getBoundingClientRect();
    const move = (m: PointerEvent) => {
      const x = d.dir === "row" ? (m.clientX - b.left) / b.width : (m.clientY - b.top) / b.height;
      const [start, size] = d.dir === "row" ? [d.rect.x, d.rect.w] : [d.rect.y, d.rect.h];
      if (tab) tab.tree = setRatio(tab.tree, d.path, (x - start) / size);
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

  /// Isi layout.json dianggap tidak tepercaya: bentuk salah dibuang, sisanya dipakai.
  function sanitize(saved: any): Ws[] {
    if (!Array.isArray(saved?.workspaces)) return [];
    return saved.workspaces
      .filter((w: any) => typeof w?.id === "string" && Array.isArray(w.tabs))
      .map((w: any) => ({
        id: w.id,
        name: String(w.name ?? "workspace").slice(0, 60),
        root: String(w.root ?? "~").slice(0, 500),
        tabs: w.tabs
          .filter((t: any) => typeof t?.id === "string" && valid(t.tree))
          .map((t: any) => ({ id: t.id, title: String(t.title ?? "").slice(0, 60), tree: t.tree, focus: panes(t.tree).includes(t.focus) ? t.focus : panes(t.tree)[0] })),
      }));
  }

  /// FR-45 pulihkan; FR-43 pertama kali dibuka
  async function restore() {
    boot = (await call("daemon.info")).boot;
    const saved = await call("layout.load");
    const alive = new Set<number>(await call("pane.list"));
    const restored = sanitize(saved);
    if (restored.length) {
      const sameBoot = saved.boot === boot;
      const oldMeta: Record<string, Meta> = saved.panes ?? {};
      let next = -1;
      for (const w of restored)
        for (const t of w.tabs)
          for (const p of panes(t.tree)) {
            if (sameBoot && alive.has(p)) {
              meta[p] = oldMeta[p] ?? { profile: "shell", cwd: null, agent: "", name: "" };
              alive.delete(p);
              continue;
            }
            // id dari run lain bisa bentrok dengan id baru: pane mati selalu negatif
            const id = next--;
            t.tree = replace(t.tree, p, id);
            if (t.focus === p) t.focus = id;
            meta[id] = oldMeta[p] ?? { profile: "shell", cwd: null, agent: "", name: "" };
            dead[id] = true;
          }
      wss = restored;
      // pane hidup yang tidak ada di layout (mis. layout gagal tersimpan) tetap tampil
      for (const p of alive) wss[0].tabs.push({ id: uid(), title: "", tree: { pane: p }, focus: p });
      const a = saved.at ?? {};
      at = wss[a.ws]?.tabs[a.tab] ? { ws: a.ws, tab: a.tab } : { ws: 0, tab: 0 };
      for (const w of wss) if (!w.tabs.length) await newTab(undefined, wss.indexOf(w));
      return;
    }
    wss = [{ id: uid(), name: "default", root: "~", tabs: [] }];
    const ids = [...alive];
    if (ids.length) for (const p of ids) wss[0].tabs.push({ id: uid(), title: "", tree: { pane: p }, focus: p });
    else await newTab(undefined, 0);
    at = { ws: 0, tab: 0 };
    welcome = await call("agents.detect", {}).catch(() => []);
  }

  // FR-40: tanda selesai pane shell hilang begitu pane itu dilihat
  $effect(() => {
    const m = tab ? meta[tab.focus] : undefined;
    if (seen && m && !AGENTS.includes(m.agent) && m.status === "done") m.status = "";
  });

  // FR-55: toolbar pane yang baru difokus diperbarui (tanpa polling git)
  $effect(() => {
    if (tab?.focus) refreshGit(tab.focus);
  });

  // FR-40: badge Dock dan Dock melompat hanya saat jendela tidak sedang dilihat
  let seen = $state(document.hasFocus());
  // FR-55: isi dan urutan chip toolbar dari [toolbar]
  const toolbar = $derived(cfg?.values.toolbar ?? { left: ["attach", "diff", "rich_input"], right: ["cwd", "branch", "settings"] });
  const notify = $derived(cfg?.values.notify ?? { on_blocked: true, on_done: true, long_command_secs: 30, sound: true });

  /// FR-40: banner + Dock melompat saat jendela tidak dilihat; isi nama pane + status saja (SEC-33)
  let lastNotified: { pane: number; at: number } | null = null;
  function alert(pane: number, kind: "blocked" | "done", label: string) {
    const m = meta[pane];
    if (seen || !m || !(kind === "blocked" ? notify.on_blocked : notify.on_done)) return;
    if (cfg?.values.profiles.find((p) => p.id === m.profile)?.notify === false) return;
    invoke("attention").catch(() => {});
    invoke("notify_pane", { body: `${m.name || m.profile} ${label}`, sound: notify.sound }).catch(() => {});
    lastNotified = { pane, at: Date.now() };
  }

  /// Klik notifikasi = app aktif; loncat ke pane yang terakhir memberi tahu (bila belum lama)
  function onReturn() {
    const n = lastNotified;
    lastNotified = null;
    if (!n || Date.now() - n.at > 120_000) return;
    for (const [wi, w] of wss.entries())
      for (const [ti, t] of w.tabs.entries())
        if (panes(t.tree).includes(n.pane)) {
          at = { ws: wi, tab: ti };
          t.focus = n.pane;
          zoomed = false;
        }
  }
  const attention = $derived(
    Object.values(meta).filter((m) => (m.status === "blocked" && notify.on_blocked) || (m.status === "done" && notify.on_done)).length,
  );
  $effect(() => {
    invoke("badge", { count: seen ? 0 : attention }).catch(() => {});
  });
  function onStatus(pane: number, status: string) {
    if (!meta[pane]) return;
    meta[pane].status = status;
    if (status === "done" || status === "idle") refreshGit(pane); // toolbar diperbarui per giliran (TECH 7a)
    if (status === "blocked") alert(pane, "blocked", "butuh izin");
    if (status === "done") alert(pane, "done", "selesai");
  }

  // simpan tata letak setengah detik setelah perubahan terakhir (SEC-13: tanpa isi layar)
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const body = JSON.stringify({ version: 1, boot, at, workspaces: wss, panes: meta });
    if (!wss.length || !boot) return;
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => call("layout.save", { layout: JSON.parse(body) }).catch(() => {}), 500);
  });

  // folder dan proses tiap pane untuk judul tab, subjudul, dan ikon agent (FR-52)
  async function poll() {
    const ids = Object.keys(meta).map(Number).filter((p) => !dead[p]);
    for (const p of ids) {
      const i = await call("pane.info", { pane: p }).catch(() => null);
      if (!i || !meta[p]) continue;
      if (i.cwd && meta[p].cwd !== i.cwd) {
        meta[p].cwd = i.cwd;
        refreshGit(p);
      }
      const busy = i.busy ? (i.name ?? "proses") : null;
      const m = meta[p];
      if (AGENTS.includes(m.agent) || m.agent === "custom" || i.status) {
        // status dari daemon: hook resmi, atau pola layar untuk profil custom dan agent yang diketik manual (FR-4)
        if (m.status !== i.status) m.status = i.status;
      } else {
        // FR-40 untuk pane shell: perintah lebih dari 30 detik lalu selesai = done; menunggu password = blocked
        if (busy && !m.busy) m.since = Date.now();
        if (i.password && m.status !== "blocked") {
          m.status = "blocked";
          alert(p, "blocked", "menunggu password");
        } else if (!i.password && m.status === "blocked") m.status = "";
        if (!busy && m.busy && Date.now() - (m.since ?? Date.now()) >= notify.long_command_secs * 1000) {
          m.status = "done";
          alert(p, "done", "selesai menjalankan perintah lama");
        }
      }
      if (m.busy !== busy) m.busy = busy;
      if (i.pane_name && meta[p].name !== i.pane_name) meta[p].name = i.pane_name;
      if (meta[p].policy !== i.policy) meta[p].policy = i.policy;
      if (meta[p].readable !== i.readable) meta[p].readable = i.readable;
    }
  }

  onMount(() => {
    window.addEventListener("keydown", onKey, true);
    const unlisten = listen<string>("shortcut", ({ payload }) => run(payload));
    const unlistenCfg = listen<any>("daemon", ({ payload }) => {
      if (payload.event === "config.changed") cfg = payload.params;
      if (payload.event === "pane.status") onStatus(payload.params.pane, payload.params.status);
      if (payload.event === "pane.replace" && meta[payload.params.pane]) meta[payload.params.pane].output = true;
      if (payload.event === "daemon.lost") onLost();
      if (payload.event === "msg.event") refreshMsgs();
    });
    const timer = setInterval(poll, 2000);
    // FR-15: seret file ke jendela = path ber-quote ditempel ke pane yang fokus (bracketed paste, tanpa Enter)
    const unlistenDrop = getCurrentWebview().onDragDropEvent(({ payload }) => {
      if (payload.type !== "drop" || !tab || dead[tab.focus] || !payload.paths.length) return;
      attach(tab.focus, payload.paths);
    });
    const onFocus = () => {
      seen = true;
      onReturn();
    };
    const onBlur = () => (seen = false);
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);
    (async () => {
      try {
        cfg = await call("config.get");
        await restore();
        // pane yang hidup dari run sebelumnya: daemon diberi tahu workspace-nya (aturan akses SEC-4/21)
        for (const p of Object.keys(meta).map(Number).filter((p) => !dead[p])) call("pane.workspace", { pane: p, workspace: wsIdOf(p) }).catch(() => {});
        refreshMsgs();
        poll();
        // FR-21: app baru + daemon lama = tanya dulu; menjalankan ulang daemon menghentikan semua pane
        const [d, app] = await Promise.all([call("daemon.info"), getVersion()]);
        if (d.daemon !== app) upgrade = { daemon: d.daemon, app, panes: (await call("pane.list")).length };
      } catch (e) {
        error = `gagal sambung ke daemon: ${e}`;
      }
    })();
    return () => {
      clearInterval(timer);
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("keydown", onKey, true);
      unlisten.then((f) => f());
      unlistenCfg.then((f) => f());
      unlistenDrop.then((f) => f());
    };
  });
</script>

{#snippet chip(c: string, pane: number, g: any)}
  {#if c === "attach"}
    <button class="chip" title="Pilih file, atau seret file ke pane" onclick={async () => attach(pane, await invoke<string[]>("pick_files"), true)}>Lampirkan</button>
  {:else if c === "diff" && g?.repo && g.files}
    <button class="chip" onclick={() => openChanges(pane)}>{g.files} file <span class="plus">+{g.add}</span> <span class="minus">−{g.del}</span></button>
  {:else if c === "rich_input"}
    <button class="chip" onclick={() => (rich = { pane, text: "" })}>Rich Input <span class="n">⌘G</span></button>
  {:else if c === "cwd"}
    <span class="chip quiet" title={meta[pane]?.cwd ?? ""}>{base(meta[pane]?.cwd) || "~"}</span>
  {:else if c === "branch" && g?.repo && g.branch}
    <span class="chip quiet">{g.branch}{g.ahead ? ` ↑${g.ahead}` : ""}{g.behind ? ` ↓${g.behind}` : ""}</span>
  {:else if c === "settings"}
    <button class="chip" aria-label="Pengaturan profil" onclick={() => (settingsOpen = true)}>⚙</button>
  {/if}
{/snippet}

{#if sidebar}
  <aside class="side" aria-label="Tab">
    <input class="search" placeholder="Cari tab  ⌘P" aria-label="Cari tab" bind:this={search} bind:value={filter} spellcheck="false"
      onkeydown={(e) => {
        // Enter = buka tab pertama yang cocok; Esc = kosongkan dan kembali ke terminal
        if (e.key === "Enter") {
          const i = order.findIndex((o) => title(o.t).toLowerCase().includes(filter.toLowerCase()));
          if (i >= 0) goto(i);
          filter = "";
          search?.blur();
        }
        if (e.key === "Escape") {
          filter = "";
          search?.blur();
        }
      }} />
    {#each wss as w, wi (w.id)}
      <div class="ws">
        {#if editing?.kind === "ws" && editing.id === w.id}
          <!-- svelte-ignore a11y_autofocus -->
          <input class="edit" bind:value={editing.value} onkeydown={(e) => e.key === "Enter" && saveEdit()} onblur={saveEdit} autofocus aria-label="Nama workspace" />
        {:else}
          <span class="wsname" role="button" tabindex="-1" ondblclick={() => (editing = { kind: "ws", id: w.id, value: w.name })}>{w.name}</span>
          <span class="root" title={w.root}>{home(w.root)}</span>
          {#if wss.length > 1}
            <button class="x" aria-label="Naikkan workspace {w.name}" onclick={() => moveWs(wi, -1)}>↑</button>
            <button class="x" aria-label="Turunkan workspace {w.name}" onclick={() => moveWs(wi, 1)}>↓</button>
            <button class="x" aria-label="Hapus workspace {w.name}" onclick={() => askRemoveWorkspace(wi)}>×</button>
          {/if}
        {/if}
      </div>
      {#each w.tabs as t, ti (t.id)}
        {@const n = order.findIndex((o) => o.t === t)}
        {@const agents = panes(t.tree).map((p) => meta[p]?.busy).filter(Boolean)}
        {#if !filter || title(t).toLowerCase().includes(filter.toLowerCase())}
          <button class="tab" class:on={at.ws === wi && at.tab === ti} onclick={() => ((at = { ws: wi, tab: ti }), (zoomed = false))}
            ondblclick={() => (editing = { kind: "tab", id: t.id, value: t.title || title(t) })}>
            {#if editing?.kind === "tab" && editing.id === t.id}
              <!-- svelte-ignore a11y_autofocus -->
              <input class="edit" bind:value={editing.value} onkeydown={(e) => e.key === "Enter" && saveEdit()} onblur={saveEdit} autofocus aria-label="Nama tab" />
            {:else}
              {@const st = tabStatus(t)}
              <span class="tt">{#if MARK[st]}<span class="mark {st}" title={MARK[st][1]} aria-label={MARK[st][1]}>{MARK[st][0]}</span>{/if}{title(t)}</span>
              <span class="n">{n < 9 ? `⌘${n + 1}` : ""}</span>
              <span class="sub">{home(meta[t.focus]?.cwd)}{agents.length ? ` · ${agents.join(", ")}` : ""}</span>
            {/if}
          </button>
        {/if}
      {/each}
    {/each}
    <div class="adds">
      <button class="add" onclick={openPicker}>+ Tab <span class="n">⌘T</span></button>
      {#if newWs}
        <div class="newws">
          <input class="edit" placeholder="Nama" aria-label="Nama workspace baru" bind:value={newWs.name} />
          <input class="edit" placeholder="Folder, mis. ~/Programming/app" aria-label="Folder workspace baru" bind:value={newWs.root}
            onkeydown={(e) => e.key === "Enter" && addWorkspace()} />
        </div>
      {:else}
        <button class="add" onclick={() => (newWs = { name: "", root: "" })}>+ Workspace</button>
      {/if}
    </div>
  </aside>
{/if}

<div class="grid" class:full={!sidebar} bind:this={box}>
  {#if error}<p class="error">{error}</p>{/if}
  {#each wss as w, wi (w.id)}
    {#each w.tabs as t, ti (t.id)}
      {@const active = at.ws === wi && at.tab === ti}
      {#each [...rects(t.tree)] as [pane, r] (pane)}
        {@const hidden = !active || (zoomed && pane !== t.focus)}
        {@const rr = zoomed ? { x: 0, y: 0, w: 1, h: 1 } : r}
        {@const bar = !dead[pane] && [...AGENTS, "custom"].includes(meta[pane]?.agent ?? "")}
        {@const g = meta[pane]?.git}
        <div
          class="tile"
          class:dim={active && pane !== t.focus && panes(t.tree).length > 1}
          style:display={hidden ? "none" : "block"}
          style:left={pct(rr.x)}
          style:top={pct(rr.y)}
          style:width={pct(rr.w)}
          style:height={pct(rr.h)}
          onpointerdown={() => (t.focus = pane)}
          role="presentation"
        >
          {#if dead[pane]}
            <div class="dead">
              <span>Pane ini berhenti karena daemon berhenti atau dijalankan ulang.</span>
              <span class="keys">{meta[pane]?.name || meta[pane]?.profile} · {home(meta[pane]?.cwd) || "~"}</span>
              <button class="mini" onclick={() => restart(pane)}>Jalankan ulang {meta[pane]?.profile ?? "shell"}</button>
            </div>
          {:else}
            <div class="body" class:withbar={bar}>
              <Tile bind:this={tiles[pane]} {pane} focused={active && pane === t.focus} {ui} zoom={zoom + (meta[pane]?.zoom ?? 0)} {onexit} {onlink}
                onpasted={(path: string) => meta[pane] && (meta[pane].pastes = [...(meta[pane].pastes ?? []), path])} />
            </div>
            {#if bar}
              <!-- FR-55: toolbar agent; chip tanpa data (bukan repo) disembunyikan -->
              <div class="bar">
                {#if editing?.kind === "pane" && editing.id === String(pane)}
                  <!-- svelte-ignore a11y_autofocus -->
                  <input class="edit pname" bind:value={editing.value} onkeydown={(e) => e.key === "Enter" && saveEdit()} onblur={saveEdit} autofocus aria-label="Nama pane" />
                {:else}
                  <button class="chip pname" title="Klik dua kali untuk ganti nama" ondblclick={() => (editing = { kind: "pane", id: String(pane), value: meta[pane]?.name ?? "" })}>{meta[pane]?.name}</button>
                {/if}
                {#each toolbar.left as c}{@render chip(c, pane, g)}{/each}
                {#if meta[pane]?.output}<button class="chip" onclick={() => (outputOf = pane)}>Output sesi</button>{/if}
                {#if meta[pane]?.sig && meta[pane].sig !== sig(meta[pane].profile)}
                  <button class="chip changed" onclick={() => relaunch(pane)}>Profil berubah · jalankan ulang</button>
                {/if}
                <span class="grow"></span>
                {#each toolbar.right as c}{@render chip(c, pane, g)}{/each}
              </div>
            {/if}
            {#if outputOf === pane}
              <OutputView {pane} onclose={() => (outputOf = null)} />
            {/if}
            {#if rich?.pane === pane}
              <div class="rich">
                <!-- svelte-ignore a11y_autofocus -->
                <textarea
                  bind:value={rich.text}
                  autofocus
                  rows="4"
                  aria-label="Rich Input"
                  placeholder="Tulis prompt · Enter kirim · Shift+Enter baris baru · Esc tutup"
                  onkeydown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) (e.preventDefault(), sendRich());
                    if (e.key === "Escape") (e.preventDefault(), (rich = null));
                  }}
                ></textarea>
              </div>
            {/if}
          {/if}
          {#each pendingFor(pane) as m (m.id)}
            <!-- FR-12: kartu di dekat pane target; hanya klik (Enter tidak menyetujui tanpa sengaja) -->
            <div class="card" role="alertdialog" aria-label="Pesan menunggu persetujuan">
              <span><b>{m.from}</b> ingin mengirim ke pane ini <span class="keys">{m.id}</span></span>
              <pre>{m.body}</pre>
              <span class="acts">
                <button class="mini" onclick={() => call("msg.reject", { id: m.id }).then(refreshMsgs)}>Tolak</button>
                <button class="mini" onclick={() => call("msg.approve", { id: m.id }).then(refreshMsgs)}>Setujui</button>
              </span>
            </div>
          {/each}
          {#if confirm?.pane === pane}
            <div class="confirm" role="alertdialog" aria-label="Konfirmasi tutup pane">
              <span><b>{confirm?.name}</b> masih jalan. Tutup pane?</span>
              <span class="keys">Enter tutup · Esc batal</span>
            </div>
          {/if}
        </div>
      {/each}
    {/each}
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
  {#if upgrade}
    <div class="confirm" role="alertdialog" aria-label="Daemon versi lama">
      <span>Daemon versi <b>{upgrade.daemon}</b> masih jalan, app sekarang <b>{upgrade.app}</b>.</span>
      <span>Menjalankan ulang daemon menghentikan <b>{upgrade.panes} pane</b>; pane tampil lagi dengan tombol "Jalankan ulang".</span>
      <span class="acts">
        <button class="mini" onclick={() => (upgrade = null)}>Nanti</button>
        <button class="mini" onclick={() => ((upgrade = null), call("daemon.stop").catch(() => {}))}>Jalankan ulang daemon</button>
      </span>
    </div>
  {/if}
  {#if msgData?.paused}
    <div class="paused" role="status">Pesan antar agent dijeda · ⌘⇧. untuk lanjut</div>
  {/if}
  {#if drawer}
    <Messages data={msgData} {rows} onchange={refreshMsgs} onclose={() => (drawer = false)} />
  {/if}
  {#if compose}
    <div class="confirm compose" role="dialog" aria-label="Kirim ke agent lain">
      <b>Kirim ke agent lain</b>
      <select aria-label="Pane tujuan" bind:value={compose.to}>
        {#each rows.filter((r) => r.pane !== tab?.focus && r.agent !== "shell") as r (r.pane)}<option value={r.pane}>{r.name} · {r.status || "?"}</option>{/each}
      </select>
      <!-- svelte-ignore a11y_autofocus -->
      <textarea bind:value={compose.body} rows="5" autofocus aria-label="Isi pesan" placeholder="Isi pesan; dikirim dengan label dari pengguna"
        onkeydown={(e) => e.key === "Enter" && e.metaKey && sendCompose()}></textarea>
      <label class="inline"><input type="checkbox" bind:checked={compose.force} /> Paksa bila status target tidak diketahui</label>
      {#if compose.error}<span class="err">{compose.error}</span>{/if}
      <span class="acts"><button class="mini" onclick={() => (compose = null)}>Batal</button> <button class="mini" onclick={sendCompose}>Kirim ⌘↩</button></span>
    </div>
  {/if}
  {#if changes}
    <Changes status={meta[changes.pane]?.git} diff={changes.diff} onclose={() => (changes = null)} />
  {/if}
  {#if renameError}
    <div class="confirm" role="alert">
      <span>{renameError}</span>
      <button class="mini" onclick={() => (renameError = "")}>Oke</button>
    </div>
  {/if}
  {#if wsAsk}
    <div class="confirm" role="alertdialog" aria-label="Konfirmasi hapus workspace">
      <span><b>{wsAsk.busy} pane</b> di workspace {wss[wsAsk.wi]?.name} masih menjalankan proses. Hapus dan hentikan?</span>
      <span class="keys">Enter hapus · Esc batal</span>
    </div>
  {/if}
  {#if welcome}
    <div class="confirm welcome" role="dialog" aria-label="Selamat datang">
      <b>Siap dipakai</b>
      {#each welcome as a (a.agent)}
        <span>{a.installed ? "✓" : "·"} {a.name} <span class="keys">{a.installed ? a.version : "tidak terpasang"}</span></span>
      {/each}
      <span class="keys">⌘T tab baru dari profil · ⌘, Pengaturan · Enter tutup</span>
    </div>
  {/if}
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
    <Settings {cfg} {meta} onclose={() => (settingsOpen = false)} onquitall={askQuitAll} />
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
    inset: 0 0 0 220px;
  }
  .grid.full {
    left: 0;
  }
  /* FR-52: tab vertikal per workspace; tenang, hanya yang membantu kerja (memory: UI bersih ala Warp) */
  .side {
    position: fixed;
    inset: 0 auto 0 0;
    width: 220px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 10px 8px;
    border-right: 1px solid var(--line);
    overflow-y: auto;
    font: 13px system-ui, -apple-system, sans-serif;
    color: var(--fg);
  }
  .search,
  .edit {
    width: 100%;
    box-sizing: border-box;
    padding: 5px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    outline: none;
  }
  .search {
    margin-bottom: 6px;
  }
  .ws {
    display: flex;
    align-items: baseline;
    gap: 6px;
    margin-top: 8px;
    padding: 2px 8px;
    color: var(--muted);
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .wsname {
    color: var(--fg);
    font-weight: 600;
  }
  .root {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-transform: none;
    letter-spacing: 0;
  }
  .x {
    border: 0;
    background: none;
    color: var(--muted);
    visibility: hidden;
    cursor: pointer;
  }
  .ws:hover .x {
    visibility: visible;
  }
  .tab {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 0 6px;
    padding: 6px 8px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: default;
  }
  .tab.on {
    background: var(--line);
  }
  .tt {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mark {
    display: inline-block;
    width: 1.1em;
    font-weight: 700;
  }
  .mark.blocked {
    color: #d97706;
  }
  .mark.done {
    color: #16a34a;
  }
  .mark.working {
    color: var(--muted);
  }
  .n {
    color: var(--muted);
    font-size: 11px;
  }
  .sub {
    grid-column: 1 / -1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: 11px;
  }
  .adds {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: 10px;
  }
  .add {
    display: flex;
    justify-content: space-between;
    padding: 5px 8px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--muted);
    font: inherit;
    text-align: left;
  }
  .add:hover {
    color: var(--fg);
  }
  .newws {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .body {
    position: absolute;
    inset: 0;
  }
  .body.withbar {
    bottom: 26px;
  }
  .bar {
    position: absolute;
    inset: auto 0 0 0;
    height: 26px;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 8px;
    border-top: 1px solid var(--line);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .chip {
    padding: 2px 8px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--muted);
    font: inherit;
    white-space: nowrap;
  }
  button.chip:hover {
    background: var(--line);
    color: var(--fg);
  }
  .chip.changed {
    color: #d97706;
  }
  .chip.pname {
    color: var(--fg);
    font-weight: 600;
  }
  input.pname {
    width: 140px;
    padding: 1px 6px;
  }
  .chip.quiet {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .grow {
    flex: 1;
  }
  .plus {
    color: #16a34a;
  }
  .minus {
    color: #dc2626;
  }
  .rich {
    position: absolute;
    inset: auto 8px 32px 8px;
    z-index: 3;
  }
  .rich textarea {
    width: 100%;
    box-sizing: border-box;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    color: var(--fg);
    font: 13px/1.4 "SF Mono", Menlo, monospace;
    resize: vertical;
    outline: none;
  }
  .dead {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    color: var(--muted);
    font: 13px system-ui, -apple-system, sans-serif;
  }
  .mini {
    padding: 4px 10px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--card);
    color: var(--fg);
    font: inherit;
  }
  .card {
    position: absolute;
    inset: auto 12px 34px 12px;
    z-index: 3;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    border: 1px solid #d97706;
    border-radius: 6px;
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
  }
  .card pre {
    margin: 0;
    max-height: 120px;
    overflow: auto;
    white-space: pre-wrap;
    font: 12px "SF Mono", Menlo, monospace;
  }
  .acts {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
  .paused {
    position: absolute;
    top: 8px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 6;
    padding: 4px 12px;
    border: 1px solid #d97706;
    border-radius: 999px;
    background: var(--card);
    color: #d97706;
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .compose {
    min-width: 420px;
  }
  .compose textarea,
  .compose select {
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: 12px "SF Mono", Menlo, monospace;
  }
  .inline {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--muted);
    font-size: 12px;
  }
  .err {
    color: #dc2626;
    font-size: 12px;
  }
  .welcome {
    min-width: 280px;
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
