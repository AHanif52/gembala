<script lang="ts">
  // Fase 1: workspace → tab → pohon split pane (FR-1, 37, 38, 43–46, 50, 52). Tata letak disimpan daemon (FR-45).
  import { onMount, tick } from "svelte";
  import Tile from "$lib/Tile.svelte";
  import Settings from "$lib/Settings.svelte";
  import Changes from "$lib/Changes.svelte";
  import OutputView from "$lib/OutputView.svelte";
  import Messages from "$lib/Messages.svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { getVersion } from "@tauri-apps/api/app";
  import { pretty, UI_DEFAULT, type Agent, type Config, type Profile } from "$lib/config";
  import { Menu } from "@tauri-apps/api/menu";
  import { Image } from "@tauri-apps/api/image";
  import { homeDir } from "@tauri-apps/api/path";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { call, b64 } from "$lib/daemon";
  import {
    dividers, neighbor, nudge, panes, rects, remove, replace, setRatio, split, swap, valid,
    type Arrow, type Dir, type Divider, type Node,
  } from "$lib/layout";
  import { safeResume } from "$lib/resume";
  import { cleanTitle } from "$lib/label";

  type Tab = { id: string; title: string; tree: Node; focus: number; color?: string };
  type Ws = { id: string; name: string; root: string; tabs: Tab[]; collapsed?: boolean; accounts?: Record<string, string>; color?: string | null; brief?: string }; // FR-61: akun bawaan per agent; FR-44: warna blok; FR-13: brief
  /// Data per pane untuk dipulihkan (FR-45): profil dan folder, tanpa isi layar (SEC-13).
  type Meta = {
    profile: string;
    cwd: string | null;
    agent: string;
    name: string;
    busy?: string | null;
    shell?: string | null; // nama proses shell pane (`zsh`), untuk subjudul tab shell diam
    status?: string;
    session?: string; // FR-53, FR-68: id sesi agent di pane (Claude dari Gembala; Codex/OpenCode/diketik dari daemon)
    git?: any; // FR-55: ringkasan git di toolbar
    pastes?: string[]; // SEC-34: gambar tempelan, dihapus saat pane ditutup
    output?: boolean; // FR-56: ada output sesi agent sebelumnya
    since?: number; // FR-40: kapan proses di pane shell mulai jalan
    sig?: string; // FR-51: sidik profil saat pane dibuat
    roleWarn?: boolean; // FR-13: peran baca saja yang tidak bisa dikunci di agent ini
    prev?: { profile: string; agent: string; session?: string; acct?: string }; // FR-45: agent pane lama yang bisa dilanjutkan
    zoom?: number; // FR-34: zoom pane ini di atas zoom global
    policy?: string; // SEC-5
    readable?: boolean; // SEC-21
    from?: { boot: string; pane: number }; // FR-62: pane lama (daemon sebelumnya) untuk riwayat blok
    ended?: { resume: string | null }; // FR-56: blok agent selesai dengan tombol, hilang saat proses baru jalan
    acct?: string; // FR-61: id akun yang dipilih saat pane dibuat (dipakai lagi saat dijalankan ulang)
    accountName?: string; // FR-61: nama akun dari daemon untuk chip; kosong = Utama
    accountId?: string; // FR-68: id akun Claude yang jalan (`claude-<id>`); kosong = Utama
    owner?: { name: string; id: string }; // FR-61: sesi Claude yang jalan milik akun lain (Lanjutkan memakai akun ini)
    login?: string; // FR-61: kartu "masuk ke akun …" setelah tombol Login
  };
  const AGENTS = ["claude", "codex", "opencode", "omp"];
  // FR-4/52: titik status + keterangan teks, tidak hanya warna (bagian 11 BRD)
  const STATUS: Record<string, string> = { blocked: "butuh izin", done: "selesai", working: "bekerja" };
  // FR-52: warna tab pilihan pengguna (garis tipis kiri)
  const COLORS: [string, string][] = [
    ["red", "Merah"], ["orange", "Oranye"], ["yellow", "Kuning"], ["green", "Hijau"], ["blue", "Biru"],
    ["purple", "Ungu"], ["pink", "Merah muda"], ["cyan", "Toska"], ["brown", "Cokelat"],
  ];
  /// FR-44: workspace baru otomatis berwarna: warna yang paling jarang dipakai workspace lain, mulai biru (Gembala.pen 02)
  const FIRST = "blue";
  const nextColor = (list: Ws[]) => {
    const used = (c: string) => list.filter((w) => w.color === c).length;
    return COLORS.map(([c]) => c).reduce((best, c) => (used(c) < used(best) ? c : best), FIRST);
  };
  // FR-52: tanda per agent (bukan logo merek), warna mendekati warna khas agent
  const GLYPH: Record<string, string> = { claude: "✳", omp: "π", codex: "◎", opencode: "◇" };
  /// agent yang sedang jalan di pane (dari profil atau diketik sendiri); "" = tidak ada
  const agentOf = (p: number) => (AGENTS.includes(meta[p]?.busy ?? "") ? meta[p].busy! : "");
  /// FR-61: nama akun Claude pane yang sedang menjalankan Claude; begitu ada akun lain, Utama ikut berlabel
  const acctOf = (p: number) =>
    meta[p]?.accountName || (agentOf(p) === "claude" && (cfg?.values.accounts ?? []).some((a) => a.agent === "claude") ? "Utama" : "");
  const HEX: Record<string, string> = { red: "#ef4444", orange: "#f97316", yellow: "#eab308", green: "#22c55e", blue: "#3b82f6", purple: "#a855f7", pink: "#ec4899", cyan: "#06b6d4", brown: "#a16207" };
  /// submenu Warna: bulatan + nama, ✓ = warna sekarang
  const colorMenu = (cur: string | null | undefined, set: (c: string | undefined) => void): MItem[] => [
    [`Tanpa warna${cur ? "" : "  ✓"}`, () => set(undefined), true, "none"],
    "-",
    ...COLORS.map(([c, name]) => [`${name}${cur === c ? "  ✓" : ""}`, () => set(c), true, c] as MItem),
  ];
  const rank = (s?: string) => ["working", "done", "blocked"].indexOf(s ?? "");
  /// status yang ditampilkan: "selesai" yang sudah dilihat pudar (FR-40)
  const stat = (p: number) => (ack[p] && meta[p]?.status === "done" ? "" : (meta[p]?.status ?? ""));
  const worst = (ps: number[]): string => ps.reduce((a, p) => (rank(stat(p)) > rank(a) ? stat(p) : a), "");
  const tabStatus = (t: Tab) => worst(panes(t.tree));

  let wss = $state<Ws[]>([]);
  let at = $state({ ws: 0, tab: 0 });
  let meta = $state<Record<number, Meta>>({});
  let blockedAt = $state<Record<number, number>>({}); // FR-65: sejak kapan pane butuh izin
  let now = $state(Date.now()); // FR-65: lama menunggu di antrean, diperbarui tiap poll
  let ack = $state<Record<number, true>>({});
  let labels = $state<Record<number, string>>({}); // FR-69: dari judul terminal agent; tidak ikut layout.json (SEC-13) // pane yang status "selesai"-nya sudah dilihat; hapus saat status berubah
  let dead = $state<Record<number, true>>({}); // pane dari run daemon lain atau sudah keluar (id negatif)
  let boot = $state(""); // state: layout disimpan ulang saat daemon baru dijalankan
  let zoomed = $state(false);
  let confirm = $state<{ pane: number; name: string; close: number[]; what: string } | null>(null);
  let quitAsk = $state(0); // jumlah pane yang masih menjalankan proses (FR-46)
  let linkAsk = $state(""); // SEC-14: URL asli ditampilkan sebelum dibuka
  let zoom = $state(0); // FR-34 ⌘=/⌘- sementara di atas ukuran config; ponytail: zoom per pane menyusul
  let cfg = $state<Config | null>(null);
  let settingsOpen = $state(false);
  let picker = $state<{ items: (Profile & { missing: boolean; key: string; label: string; account?: string })[]; sel: number } | null>(null);
  let sidebar = $state(true);
  let sideW = $state(220); // FR-52: lebar sidebar, disimpan di layout
  let pal = $state<{ q: string; sel: number; keys: [string, string, string][] } | null>(null); // FR-39 palet
  /// `line` = garis sisip (koordinat layar) di tempat jatuh
  let dragging = $state<{ kind: "tab" | "ws"; wi: number; ti: number; over: { wi: number; ti: number }; line: { top: number; left: number; width: number } } | null>(null);
  let editing = $state<{ kind: "tab" | "ws" | "pane"; id: string; value: string } | null>(null);
  let renameError = $state("");
  let welcome = $state<Agent[] | null>(null); // FR-43: kartu ringkas saat pertama kali dibuka
  let upgrade = $state<{ daemon: string; app: string; panes: number } | null>(null); // FR-21
  let wsAsk = $state<{ wi: number; busy: number } | null>(null); // FR-44: hapus workspace yang masih jalan
  let rich = $state<{ pane: number; text: string } | null>(null); // FR-14
  let changes = $state<{ pane: number; diff: any; turns?: any[]; turn?: number | null; ask?: any } | null>(null); // FR-22, FR-70
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
  // tujuan kirim: pane agent selain yang fokus, termasuk agent yang diketik sendiri di pane shell (status dilacak daemon)
  const targets = $derived(rows.filter((r) => r.pane !== tab?.focus && (r.agent !== "shell" || r.status)));

  /// Kirim manual dari pengguna (⌘⇧K, FR-57); "" = terkirim, selain itu alasan ditolak untuk ditampilkan
  async function sendMsg(to: number, body: string, force: boolean): Promise<string> {
    try {
      await call("msg.send", { to, body, force });
      refreshMsgs();
      say(`Pesan terkirim ke ${meta[to]?.name ?? "agent"}`);
      return "";
    } catch (e) {
      return e === "target_blocked" ? "Target sedang butuh izin; jawab langsung di pane itu" : e === "target_busy" ? "Target sedang bekerja; kirim paksa bila yakin" : String(e);
    }
  }

  async function sendCompose() {
    if (!compose || compose.to === null) return;
    const error = await sendMsg(compose.to, compose.body, compose.force);
    if (!error) compose = null;
    else if (compose) compose.error = error;
  }

  // FR-59: input editor ala Warp di pane shell yang sedang menunggu di prompt (penanda OSC 133 dari FR-58)
  const editorOn = $derived(ui.input_editor); // [ui] input_editor: daemon juga menyembunyikan prompt zsh pane baru
  let atPrompt = $state<Record<number, boolean>>({});
  let draft = $state<Record<number, string>>({});
  const history: string[] = []; // ponytail: riwayat sesi app saja; riwayat zsh tetap di ↑ terminal
  let hi = -1;
  const editorShown = (pane: number) => editorOn && atPrompt[pane] && !dead[pane] && lempar?.pane !== pane;
  /// teks dikirim sebagai bracketed paste (baris banyak tidak jalan satu per satu); ESC/C0/C1 dibuang seperti FR-14
  function toShell(pane: number, text: string, enter: boolean) {
    const clean = text.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "");
    const body = clean ? `\x1b[200~${clean}\x1b[201~` : "";
    call("pane.write", { pane, data: b64(new TextEncoder().encode(body + (enter ? "\r" : ""))) });
  }
  let editorErr = $state<Record<number, string>>({});
  async function runDraft(pane: number) {
    const text = draft[pane] ?? "";
    // `@nama pesan` = lempar ke agent itu (FR-57); teks berawalan `@` tidak pernah dijalankan shell
    const m = /^@([\w.-]*)\s*([\s\S]*)$/.exec(text);
    if (m) {
      const to = targets.find((r) => [r.name, r.agent, meta[r.pane]?.busy ?? ""].includes(m[1]));
      if (!to) return void (editorErr[pane] = `Tidak ada agent "${m[1]}" di workspace ini`);
      if (!m[2].trim()) return void (editorErr[pane] = `Tulis pesan untuk ${to.name} setelah nama`);
      const error = await sendMsg(to.pane, m[2], false);
      if (error) return void (lempar = { pane, to: to.pane, name: to.name, body: m[2], error });
      draft[pane] = "";
      return;
    }
    if (text.trim() && history.at(-1) !== text) history.push(text);
    if (history.length > 200) history.shift();
    hi = -1;
    draft[pane] = "";
    atPrompt[pane] = false; // muncul lagi di prompt berikutnya
    toShell(pane, text, true);
  }
  // FR-59: panel "Mulai" di pane shell yang belum menjalankan apa pun
  let ran = $state<Record<number, boolean>>({});
  let installed = $state<string[]>([]); // agent terpasang (agents.detect), diisi saat app dibuka
  // FR-61: satu tombol per akun bila agent itu punya akun selain Utama (seperti palet)
  const startProfiles = $derived(
    (cfg?.values.profiles ?? [])
      .filter((p) => p.agent !== "shell" && installed.includes(p.agent))
      .flatMap((p) => {
        const own = (cfg?.values.accounts ?? []).filter((a) => a.agent === p.agent);
        const base = { ...p, key: p.id, label: p.name, account: undefined as string | undefined };
        return own.length ? [{ id: "utama", name: "Utama" }, ...own].map((a) => ({ ...base, key: `${p.id}:${a.id}`, label: `${p.name} · ${a.name}`, account: a.id })) : [base];
      }),
  );
  // FR-72: agent dengan pemilih sesi bawaan (OpenCode tidak punya dari baris perintah)
  const pickable = $derived(startProfiles.filter((p) => ["claude", "codex", "omp"].includes(p.agent)));
  // FR-71: agent di worktree baru (pen layar 07): nama, folder (bawaan `<repo>.<nama>`), profil
  let wt = $state<{ pane: number; wi: number; root: string; name: string; dir: string; dirSet: boolean; prof: number; err: string } | null>(null);
  const WT_ERR: Record<string, string> = {
    bad_name: "Nama cuma boleh huruf, angka, titik, _ dan - (maks 40)",
    bad_dir: "Folder harus path lengkap (/… atau ~/…)",
    dir_exists: "Folder itu sudah ada",
    not_repo: "Pane yang fokus bukan repo git",
    repo_filter: "Repo ini memakai filter git lokal; worktree tidak dibuat (SEC-29)",
    worktree_failed: "git worktree gagal, mungkin branch-nya sudah ada",
  };
  function openWorktree(wi = at.ws) {
    const w = wss[wi];
    const pane = wi === at.ws ? tab?.focus : w?.tabs[0]?.focus;
    const root = pane === undefined ? "" : meta[pane]?.git?.root;
    if (pane === undefined || !root) return say("Pane yang fokus bukan repo git");
    const name = `agent-${(w.tabs.length + 1).toString()}`;
    wt = { pane, wi, root, name, dir: `${root}.${name}`, dirSet: false, prof: 0, err: "" };
    tick().then(() => document.querySelector<HTMLInputElement>(".wtdlg input")?.select());
  }
  async function createWorktree() {
    if (!wt) return;
    const p = startProfiles[wt.prof];
    const r = await call("git.worktree_add", { pane: wt.pane, name: wt.name.trim(), dir: wt.dirSet ? wt.dir : undefined }).catch((e) => ({ error: String(e?.message ?? e) }));
    if (r.error) return void (wt.err = WT_ERR[r.error] ?? r.error);
    const wi = wt.wi;
    wt = null;
    await newTab(p?.id, wi, p?.account, { cwd: r.dir, from_pane: undefined });
    say(`Worktree ${home(r.dir)} · branch ${r.branch}`);
  }
  /// FR-71: tutup tab lalu hapus worktree-nya; perubahan belum di-commit = konfirmasi berisi daftar file
  async function removeWorktree(wi: number, t: Tab) {
    const dir = meta[t.focus]?.git?.root;
    if (!dir) return;
    const go = async (force: boolean) => {
      for (const p of panes(t.tree)) await call("pane.close", { pane: p }).catch(() => {});
      await new Promise((r) => setTimeout(r, 500));
      const r = await call("git.worktree_remove", { dir, force }).catch((e) => ({ error: String(e?.message ?? e) }));
      if (r.error) return say(r.error === "pane_in_worktree" ? "Masih ada pane lain di folder worktree itu" : `Gagal menghapus worktree: ${r.error}`);
      if (r.dirty) return void (wtDel = { wi, dir, files: r.dirty });
      say(`Worktree ${home(dir)} dihapus · branch tetap ada`);
    };
    wtDel = null;
    await go(false);
    wtForce = () => go(true);
  }
  let wtDel = $state<{ wi: number; dir: string; files: string[] } | null>(null);
  let wtForce: () => void = () => {};
  // FR-13: template peran (pen layar 17): satu pane per peran di tab baru, brief workspace ikut petunjuk launch
  const TEMPLATES: { name: string; desc: string; roles: [string, string, string, string][] }[] = [
    { name: "Koordinator + pekerja", desc: "1 baca saja · 2 pekerja · 1 reviewer", roles: [["Koordinator", "koordinator", "claude", "koord"], ["Pekerja 1", "pekerja", "claude", "kerja-1"], ["Pekerja 2", "pekerja", "codex", "kerja-2"], ["Reviewer", "reviewer", "codex", "review"]] },
    { name: "Planner → worker", desc: "1 planner · 1 worker", roles: [["Planner", "planner", "claude", "plan"], ["Worker", "pekerja", "claude", "kerja"]] },
    { name: "Review berpasangan", desc: "2 reviewer beda agent", roles: [["Reviewer 1", "reviewer", "claude", "review-1"], ["Reviewer 2", "reviewer", "codex", "review-2"]] },
  ];
  const READONLY = ["koordinator", "planner", "reviewer"];
  let tpl = $state<{ wi: number; t: number; rows: { label: string; role: string; prof: number; name: string }[]; brief: string } | null>(null);
  function pickTemplate(t: number) {
    if (!tpl) return;
    const prefer = (agent: string) => Math.max(0, startProfiles.findIndex((p) => p.agent === agent));
    tpl.t = t;
    tpl.rows = TEMPLATES[t].roles.map(([label, role, agent, name]) => ({ label, role, prof: prefer(agent), name }));
  }
  function openTemplate(wi = at.ws) {
    if (!startProfiles.length) return say("Belum ada agent yang terpasang");
    tpl = { wi, t: 0, rows: [], brief: wss[wi]?.brief ?? "" };
    pickTemplate(0);
  }
  const limitOf = (role: string, prof: number) =>
    !READONLY.includes(role) ? "edit" : startProfiles[prof]?.agent === "claude" ? "lock" : "warn";
  async function runTemplate() {
    if (!tpl) return;
    const { wi, rows } = tpl;
    const title = TEMPLATES[tpl.t].name;
    const w = wss[wi];
    w.brief = tpl.brief.trim().slice(0, 4000) || undefined;
    const from = wi === at.ws ? tab?.focus : undefined;
    tpl = null;
    const ids: number[] = [];
    for (const r of rows) {
      const p = startProfiles[r.prof];
      if (!p) continue;
      const id = await createPane({ profile: p.id, account: p.account, workspace: w.id, from_pane: from, cwd: from === undefined ? w.root : undefined, role: r.role });
      ids.push(id);
      if (r.name.trim()) call("pane.rename", { pane: id, name: r.name.trim().toLowerCase() }).then((x) => meta[id] && (meta[id].name = x.name)).catch(() => {});
    }
    if (!ids.length) return;
    // 2 = kiri|kanan, 3 = kiri | kanan atas/bawah, 4 = kotak 2x2
    let tree: Node = { pane: ids[0] };
    if (ids[1] !== undefined) tree = split(tree, ids[0], "row", ids[1]);
    if (ids[2] !== undefined) tree = split(tree, ids.length > 3 ? ids[0] : ids[1], "col", ids[2]);
    if (ids[3] !== undefined) tree = split(tree, ids[1], "col", ids[3]);
    w.tabs.push({ id: uid(), title, tree, focus: ids[0] });
    at = { ws: wi, tab: w.tabs.length - 1 };
    zoomed = false;
  }
  /// pane shell diganti pane profil di posisi yang sama (agent jalan dengan hook resmi, FR-4)
  async function swapToProfile(old: number, profile: string, account?: string, pick = false) {
    const m = meta[old];
    if (!m) return;
    const id = await createPane({ profile, cwd: m.cwd ?? undefined, workspace: wsIdOf(old), account, pick });
    for (const w of wss)
      for (const t of w.tabs)
        if (panes(t.tree).includes(old)) {
          t.tree = replace(t.tree, old, id);
          if (t.focus === old) t.focus = id;
        }
    delete meta[old];
    call("pane.close", { pane: old }).catch(() => {}); // pane.exit-nya tidak menemukan tab lagi: diabaikan
  }
  async function startInFolder(pane: number) {
    const path = await invoke<string | null>("pick_folder").catch(() => null);
    if (path) toShell(pane, `cd ${quote(path)}`, true);
  }

  /// FR-59: lampiran di editor = path ber-quote disisipkan ke teks (tombol +, seret file, ⌘V gambar)
  function insertPaths(pane: number, paths: string[]) {
    if (!paths.length) return;
    const d = draft[pane] ?? "";
    draft[pane] = d + (d && !d.endsWith(" ") ? " " : "") + paths.map(quote).join(" ") + " ";
    document.querySelector<HTMLTextAreaElement>(`[data-editor="${pane}"]`)?.focus();
  }
  const IMG: Record<string, string> = { "image/png": "png", "image/jpeg": "jpg", "image/gif": "gif", "image/webp": "webp" };
  /// ⌘V gambar di editor: disimpan ke folder sementara Gembala (SEC-34) lalu path-nya disisipkan, seperti di terminal (FR-54)
  async function pasteToEditor(pane: number, e: ClipboardEvent) {
    const item = [...(e.clipboardData?.items ?? [])].find((i) => i.kind === "file" && IMG[i.type]);
    const file = item?.getAsFile();
    if (!item || !file) return; // teks biasa: textarea yang menangani
    e.preventDefault();
    const path = await invoke<string>("save_paste", { data: b64(new Uint8Array(await file.arrayBuffer())), ext: IMG[item.type] }).catch(() => "");
    if (!path) return say("Gagal menyimpan gambar");
    if (meta[pane]) meta[pane].pastes = [...(meta[pane].pastes ?? []), path];
    insertPaths(pane, [path]);
  }

  /// Tab/Esc: teks pindah ke baris zsh, kendali ke terminal (autocomplete zsh tetap jalan)
  function handOff(pane: number, tab: boolean) {
    toShell(pane, draft[pane] ?? "", false);
    if (tab) call("pane.write", { pane, data: b64(new Uint8Array([9])) });
    draft[pane] = "";
    atPrompt[pane] = false;
    tiles[pane]?.focus();
  }
  // FR-59 autocomplete ala Warp: saran abu-abu dari riwayat (→ terima), Tab = daftar folder/file/perintah
  let zhist: string[] = []; // riwayat zsh, dimuat sekali saat app dibuka
  let homePath = "";
  type Comp = { label: string; kind: string; insert: string };
  // `auto` = saran akun yang muncul sendiri saat mengetik `cl…` (FR-61): Tab/Enter mengisi perintah, lalu bisa
  // ditambah argumen sebelum Enter berikutnya menjalankan
  let comp = $state<{ pane: number; items: Comp[]; sel: number; auto?: boolean } | null>(null);
  /// FR-59: pemilih folder dari chip folder (ala Warp): cari, "..", subfolder; Enter = `cd` di pane
  let dirs = $state<{ pane: number; dir: string; all: string[]; q: string; sel: number } | null>(null);
  const dirItems = $derived.by(() => {
    if (!dirs) return [];
    const q = dirs.q.toLowerCase();
    return [...(q ? [] : [".."]), ...dirs.all.filter((n) => n.toLowerCase().includes(q))];
  });
  async function openDirs(pane: number) {
    const dir = meta[pane]?.cwd ?? homePath;
    dirs = { pane, dir, all: await subdirs(dir, ""), q: "", sel: 0 };
  }
  const subdirs = async (dir: string, prefix: string) =>
    (await invoke<[string, boolean][]>("list_dir", { dir, prefix }).catch(() => [] as [string, boolean][])).filter(([, d]) => d).map(([n]) => n);
  async function dirsInput() {
    if (!dirs) return;
    dirs.sel = 0;
    // folder tersembunyi hanya dicari bila pencarian diawali titik (seperti Tab di editor)
    if (dirs.q.startsWith(".") || dirs.all.some((n) => n.startsWith("."))) dirs.all = await subdirs(dirs.dir, dirs.q.startsWith(".") ? "." : "");
  }
  function cdTo(name: string | undefined) {
    if (!dirs || !name) return;
    const { pane, dir } = dirs;
    dirs = null;
    draft[pane] = name === ".." ? "cd .." : `cd ${quote(`${dir}/${name}`)}`;
    runDraft(pane);
  }
  function dirsKey(e: KeyboardEvent) {
    if (!dirs) return;
    const n = dirItems.length;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      dirs.sel = (dirs.sel + (e.key === "ArrowDown" ? 1 : n - 1)) % Math.max(1, n);
      tick().then(() => document.querySelector(".dirs .opt.on")?.scrollIntoView({ block: "nearest" }));
    } else if (e.key === "Enter") (e.preventDefault(), cdTo(dirItems[dirs.sel]));
    else if (e.key === "Escape") (e.preventDefault(), (dirs = null));
  }
  /// FR-61 varian C: `cl…` di kotak ketik = satu saran per akun Claude; terpilih pertama = akun bawaan workspace
  async function accountSuggest(pane: number) {
    const r = await call("account.list", {}).catch(() => null);
    const rows: { account: { id: string; name: string; agent: string }; status: { email?: string } }[] = r?.accounts ?? [];
    const items = rows
      .filter((x) => x.account.agent === "claude")
      .map((x) => ({ label: `claude · ${x.account.name}`, kind: x.status.email ?? "", insert: x.account.id === "utama" ? "claude " : `claude-${x.account.id} ` }));
    const d = draft[pane] ?? "";
    if (items.length < 2 || d.length < 2 || !"claude".startsWith(d)) return;
    // sesi pane ini yang baru selesai (FR-56) = baris pertama dan terpilih, supaya Enter tidak menimpa niat lanjut
    const resume = meta[pane]?.ended?.resume;
    if (resume?.startsWith("claude")) return void (comp = { pane, items: [{ label: resume.replace(/([0-9a-f]{8})-[0-9a-f-]{27}$/, "$1…"), kind: "sesi terakhir pane ini", insert: `${resume} ` }, ...items], sel: 0, auto: true });
    const def = wss.find((w) => w.id === wsIdOf(pane))?.accounts?.claude;
    comp = { pane, items, sel: Math.max(0, items.findIndex((it) => it.insert === `claude-${def} `)), auto: true };
  }
  const ghost = (pane: number) => {
    const d = draft[pane] ?? "";
    if (!d || d.includes("\n") || d.startsWith("@")) return "";
    // perintah lanjut dari kartu sesi selesai (FR-56) didahulukan: `claude` → `claude --resume <id>` pane ini
    const resume = meta[pane]?.ended?.resume;
    const hit = [...(resume ? [resume] : []), ...[...history].reverse(), ...zhist].find((h) => h.startsWith(d) && h.length > d.length && !h.includes("\n"));
    return hit ? hit.slice(d.length) : "";
  };
  async function complete(pane: number) {
    const text = draft[pane] ?? "";
    const token = /(\S*)$/.exec(text)![1];
    const before = text.slice(0, text.length - token.length);
    let items: Comp[];
    const cwd = meta[pane]?.cwd ?? homePath;
    const pick = (names: string[], kind: string) =>
      names.filter((n) => n.startsWith(token)).map((n) => ({ label: n, kind, insert: `${before}${n} ` }));
    if (/^\s*git\s+(checkout|switch|merge|rebase|diff|log|branch\s+-[dD]|push\s+\S+|pull\s+\S+)\s+$/.test(before)) {
      items = pick(await invoke<string[]>("git_branches", { dir: cwd }).catch(() => [] as string[]), "Branch");
    } else if (/^\s*(npm|pnpm|yarn|bun)\s+run\s+$/.test(before)) {
      items = pick(await invoke<string[]>("npm_scripts", { dir: cwd }).catch(() => [] as string[]), "Script");
    } else if (!before.trim() && !token.includes("/")) {
      const cmds = await invoke<string[]>("list_commands", { prefix: token }).catch(() => [] as string[]);
      items = cmds.map((c) => ({ label: c, kind: "Perintah", insert: `${before}${c} ` }));
    } else {
      const tok = token.replace(/^~(?=\/|$)/, homePath);
      const cut = tok.lastIndexOf("/") + 1;
      const [dirPart, prefix] = [tok.slice(0, cut), tok.slice(cut)];
      const dir = dirPart.startsWith("/") ? dirPart : `${cwd}/${dirPart}`;
      const onlyDirs = before.trim() === "cd";
      const entries = await invoke<[string, boolean][]>("list_dir", { dir, prefix }).catch(() => [] as [string, boolean][]);
      const keep = token.slice(0, token.length - prefix.length);
      items = entries
        .filter(([, isDir]) => isDir || !onlyDirs)
        .map(([n, isDir]) => ({ label: n + (isDir ? "/" : ""), kind: isDir ? "Folder" : "File", insert: before + keep + n.replace(/ /g, "\\ ") + (isDir ? "/" : " ") }));
    }
    if (!items.length) return;
    if (items.length === 1) return void ((draft[pane] = items[0].insert), (comp = null));
    comp = { pane, items, sel: 0 };
  }

  function editorKey(pane: number, e: KeyboardEvent) {
    const ta = e.currentTarget as HTMLTextAreaElement;
    editorErr[pane] = "";
    // daftar autocomplete terbuka: ↑↓ geser, Tab/Enter pilih, Esc tutup, tombol lain = daftar ditutup
    if (comp?.pane === pane) {
      const n = comp.items.length;
      if (["ArrowDown", "ArrowUp", "Tab", "Enter", "Escape"].includes(e.key)) e.preventDefault();
      if (e.key === "ArrowDown") return void (comp.sel = (comp.sel + 1) % n);
      if (e.key === "ArrowUp") return void (comp.sel = (comp.sel + n - 1) % n);
      if (e.key === "Tab" || e.key === "Enter") return void ((draft[pane] = comp.items[comp.sel].insert), (comp = null));
      if (e.key === "Escape") return void (comp = null);
      // saran akun tetap terbuka selama teks masih awal `claude` (diatur onEditorInput)
      if (!comp.auto) comp = null;
    }
    // saran abu-abu: → di ujung teks = terima
    if (e.key === "ArrowRight" && ta.selectionStart === ta.value.length && ghost(pane)) {
      e.preventDefault();
      draft[pane] = (draft[pane] ?? "") + ghost(pane);
      comp = null; // teks sudah bukan awal `claude` lagi: saran akun ditutup supaya Enter menjalankan teks ini
      return;
    }
    // daftar @ terbuka: Tab/Enter pilih, panah geser, Esc tutup
    if (mention?.editor && mention.pane === pane && mentionItems.length && ["Tab", "Enter", "ArrowUp", "ArrowDown", "Escape"].includes(e.key)) {
      e.preventDefault();
      if (e.key === "Tab" || e.key === "Enter") chooseMention(mention.sel);
      else onMentionKey(pane, e.key);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.altKey) return void (e.preventDefault(), runDraft(pane));
    // Tab = autocomplete di editor (teks `@…` milik lempar ke agent, tidak dilengkapi)
    if (e.key === "Tab" && !e.shiftKey) return void (e.preventDefault(), !ta.value.startsWith("@") && complete(pane));
    if (e.key === "Escape") return void (e.preventDefault(), handOff(pane, false));
    if (e.key === "c" && e.ctrlKey) return void (e.preventDefault(), (draft[pane] = ""));
    const first = !ta.value.slice(0, ta.selectionStart).includes("\n");
    const last = !ta.value.slice(ta.selectionEnd).includes("\n");
    // ↑↓ = riwayat sesi app (terbaru) lalu riwayat zsh pengguna; hi 0 = paling baru
    const h = [...history].reverse().concat(zhist);
    if (e.key === "ArrowUp" && first && h.length) {
      e.preventDefault();
      hi = Math.min(h.length - 1, hi + 1);
      draft[pane] = h[hi];
    } else if (e.key === "ArrowDown" && last && hi >= 0) {
      e.preventDefault();
      hi -= 1;
      draft[pane] = hi < 0 ? "" : h[hi];
    }
  }

  // FR-59: pane yang difokus dengan editor tampil = kursor ke editor
  $effect(() => {
    const f = tab?.focus;
    if (f !== undefined && editorShown(f)) setTimeout(() => document.querySelector<HTMLTextAreaElement>(`[data-editor="${f}"]`)?.focus(), 0);
  });

  // FR-57: `@` di awal prompt pane = daftar agent lain; pilih = bar kirim di pane itu
  // editor = daftar dipicu dari input editor (FR-59), bukan dari ketikan di terminal
  // picked = nama yang sudah dilengkapi dari daftar `@@` ini (pilih beberapa agent sekaligus)
  let mention = $state<{ pane: number; q: string; sel: number; editor?: boolean; inline?: boolean; picked?: string[] } | null>(null);
  let lempar = $state<{ pane: number; to: number; name: string; body: string; error: string } | null>(null);
  const mentionItems = $derived(
    mention
      ? targets.filter(
          (r) => !mention!.picked?.includes(r.name) && [r.name, r.agent, meta[r.pane]?.busy ?? ""].some((s) => s.toLowerCase().startsWith(mention!.q.toLowerCase())),
        )
      : [],
  );
  /// Di terminal pemicunya `@@` (`@` tunggal milik agent: pilih file di Claude/OMP). `@@` di awal baris = bar kirim;
  /// di tengah kalimat (mis. "cek buat @@om") = diganti `@<nama pane>` untuk agent itu
  function onLine(pane: number, line: string) {
    const m = /(?:^|\s)@@([\w.-]*)$/.exec(line);
    // selalu lengkapi nama: Gembala tidak tahu pasti isi baris prompt TUI, jadi "awal baris" tidak bisa diandalkan
    mention = m ? { pane, q: m[1], sel: 0, inline: true } : null;
  }
  function onMentionKey(pane: number, key: string): boolean {
    if (!mention || mention.pane !== pane || !mentionItems.length) return false;
    const n = mentionItems.length;
    if (key === "ArrowDown") mention.sel = (mention.sel + 1) % n;
    else if (key === "ArrowUp") mention.sel = (mention.sel + n - 1) % n;
    else if (key === "Tab" || key === "Enter") chooseMention(mention.sel);
    else mention = null; // Escape: tutup daftar, ketikan `@…` tetap di prompt
    return true;
  }
  function onEditorInput(pane: number) {
    const m = /^@([\w.-]*)$/.exec(draft[pane] ?? "");
    mention = m ? { pane, q: m[1], sel: 0, editor: true } : mention?.editor ? null : mention;
    const d = draft[pane] ?? "";
    const claudeAccts = (cfg?.values.accounts ?? []).some((a) => a.agent === "claude");
    if (claudeAccts && d.length >= 2 && "claude".startsWith(d)) {
      if (!(comp?.pane === pane && comp.auto)) accountSuggest(pane);
    } else if (comp?.pane === pane && comp.auto) comp = null;
  }
  function chooseMention(i: number) {
    const r = mentionItems[i];
    if (!mention || !r) return;
    const { pane, q } = mention;
    if (mention.editor) {
      // di editor: nama dilengkapi, pesan ditulis di kotak yang sama, Enter kirim
      draft[pane] = `@${r.name} `;
      mention = null;
      return;
    }
    if (mention.inline) {
      // di tengah kalimat prompt agent: `@…` diganti nama pane lengkap; agent memakai `gembala send` (petunjuk FR-9)
      // `@@q` → `@nama ` (satu `@` tersisa); pilihan berikutnya dari daftar yang sama = `@nama2 ` di belakangnya
      const picked = mention.picked ?? [];
      const text = picked.length ? `@${r.name} ` : "\x7f".repeat(q.length + 1) + r.name + " ";
      call("pane.write", { pane, data: b64(new TextEncoder().encode(text)) });
      // baris yang dilacak ikut berubah, supaya daftar @@ berikutnya membaca teks yang benar
      tiles[pane]?.setLine((l) => (picked.length ? l : l.slice(0, -(q.length + 1))) + (picked.length ? text : `${r.name} `));
      // daftar tetap terbuka tanpa yang sudah dipilih: Tab/Enter tambah agent, Esc atau terus mengetik = tutup
      const next = [...picked, r.name];
      mention = targets.some((t) => !next.includes(t.name)) ? { pane, q: "", sel: 0, inline: true, picked: next } : null;
      return;
    }
    // `@@…` yang sudah terkirim ke TUI dihapus lagi dengan backspace
    call("pane.write", { pane, data: b64(new TextEncoder().encode("\x7f".repeat(q.length + 2))) });
    tiles[pane]?.resetLine();
    mention = null;
    lempar = { pane, to: r.pane, name: r.name, body: "", error: "" };
  }
  async function sendLempar(force = false) {
    if (!lempar?.body.trim()) return;
    const { pane, to, body } = lempar;
    const error = await sendMsg(to, body, force);
    if (error) return void (lempar && (lempar.error = error));
    lempar = null;
    tiles[pane]?.focus();
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
  /// kepala pane dan bar bawah: dua folder terakhir saja (`~/…/gembala/app`, Gembala.pen 02); path penuh di tooltip
  const short = (p: string | null | undefined) => {
    const a = home(p).split("/");
    return a.length > 4 ? [a[0], "…", ...a.slice(-2)].join("/") : a.join("/");
  };
  /// FR-52 subjudul tab: hanya yang beda dari workspace. Folder = akar workspace → kosong; selain itu nama folder saja
  /// (Gembala.pen 02: `main · requirements`); path lengkap ada di tooltip
  const where = (cwd: string | null | undefined, root: string) => {
    const c = home(cwd);
    return !c || c === root ? "" : base(c);
  };
  /// FR-52: judul bawaan = agent yang sedang jalan di pane fokus, lalu folder (`~` untuk home)
  const title = (t: Tab) => {
    const m = meta[t.focus];
    if (t.title) return t.title;
    if (labels[t.focus]) return labels[t.focus]; // FR-69
    if (m?.busy && AGENTS.includes(m.busy)) return m.busy;
    return home(m?.cwd) === "~" ? "~" : base(m?.cwd) || "~";
  };
  /// Urutan global tab untuk ⌘1–⌘9
  const order = $derived(wss.flatMap((w, wi) => w.tabs.map((t, ti) => ({ wi, ti, t }))));

  async function createPane(opts: Record<string, unknown> = {}): Promise<number> {
    const profile = (opts.profile as string) ?? "shell";
    const agent = cfg?.values.profiles.find((p) => p.id === profile)?.agent ?? "shell";
    // FR-53: Claude Code menerima id sesi dari luar; dipakai lagi saat pane dijalankan ulang
    // FR-72: pemilih sesi bawaan = id sesi belum diketahui
    const session = opts.pick ? undefined : ((opts.session as string) ?? (agent === "claude" ? crypto.randomUUID() : undefined));
    // FR-61, C-9: akun = pilihan saat membuka (atau akun pane yang dijalankan ulang) > akun bawaan workspace > profil/Utama (daemon)
    const w = wss.find((x) => x.id === ((opts.workspace as string) ?? ws?.id));
    // pane shell ikut akun Claude bawaan workspace: `claude` yang diketik memakai akun itu
    const acct = (opts.account as string | undefined) || w?.accounts?.[agent === "shell" ? "claude" : agent] || undefined;
    // FR-13: brief workspace ikut petunjuk launch tiap agent di workspace itu
    const brief = agent !== "shell" ? w?.brief : undefined;
    const r = await call("pane.create", { workspace: ws?.id, ...opts, session, account: acct, brief });
    // FR-13: peran baca saja di agent tanpa pembatas tool = cuma petunjuk, ditandai di kepala pane
    const roleWarn = ["koordinator", "planner", "reviewer"].includes(opts.role as string) && agent !== "claude";
    meta[r.pane] = { profile, cwd: null, agent, name: r.name, session, sig: sig(profile), acct, roleWarn };
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
      account: m.owner?.id ?? m.acct, // FR-61: sesi milik akun lain dilanjutkan di akun pemiliknya
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
    if (!meta[pane] || dead[pane]) return; // semua pane: branch juga jadi subjudul tab (FR-52)
    const g = await call("git.status", { pane }).catch(() => null);
    if (g && meta[pane]) meta[pane].git = g;
  }

  async function openChanges(pane: number) {
    if (changes) return void (changes = null);
    loadChanges(pane);
  }

  /// FR-22: panel menempel ikut pane yang fokus; diff lama tetap tampil sampai yang baru datang
  async function loadChanges(pane: number) {
    const same = changes?.pane === pane;
    changes = { pane, diff: same ? changes!.diff : null, turns: same ? changes!.turns : [], turn: same ? changes!.turn : null };
    refreshGit(pane);
    const turns = await call("git.turns", { pane }).catch(() => []);
    if (changes?.pane !== pane) return;
    changes.turns = turns;
    if (changes.turn != null && !turns.some((t: any) => t.i === changes!.turn)) changes.turn = null;
    const diff = await (changes.turn == null ? call("git.diff", { pane }) : call("git.turn_diff", { pane, i: changes.turn })).catch(() => ({ repo: false }));
    if (changes?.pane === pane) changes.diff = diff;
  }
  /// FR-70: pilih giliran (null = semua sejak commit)
  function pickTurn(i: number | null) {
    if (!changes) return;
    changes.turn = i;
    changes.diff = null;
    loadChanges(changes.pane);
  }
  async function askRestore(i: number) {
    if (!changes) return;
    const pane = changes.pane;
    const r = await call("git.turn_preview", { pane, i }).catch((e) => ({ error: String(e) }));
    if (r.error) return say(`Gagal memeriksa giliran: ${r.error}`);
    if (!r.files.length) return say("Folder kerja sudah sama dengan sebelum giliran ini");
    if (changes?.pane === pane) changes.ask = { i, ...r };
  }
  async function doRestore() {
    const c = changes;
    if (!c?.ask) return;
    const r = await call("git.turn_restore", { pane: c.pane, i: c.ask.i }).catch((e) => ({ error: String(e) }));
    c.ask = null;
    say(r.error ? `Gagal mengembalikan: ${r.error}` : `${r.restored} file dikembalikan`);
    pickTurn(null);
  }
  let panelW = $state(460); // FR-22: lebar panel perubahan, disimpan di layout
  $effect(() => {
    const f = tab?.focus;
    if (changes && f !== undefined && f !== changes.pane && !dead[f]) loadChanges(f);
  });

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

  async function newTab(profile?: string, wi = at.ws, account?: string, extra: Record<string, unknown> = {}): Promise<number> {
    const w = wss[wi];
    const from = wi === at.ws ? tab?.focus : undefined;
    const id = await createPane({ profile, from_pane: from, cwd: from === undefined ? w.root : undefined, workspace: w.id, account, ...extra });
    w.tabs.push({ id: uid(), title: "", tree: { pane: id }, focus: id });
    at = { ws: wi, tab: w.tabs.length - 1 };
    zoomed = false;
    return id;
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
    if (info.busy) confirm = { pane, name: info.name ?? "proses", close: [pane], what: "pane" };
    else await call("pane.close", { pane });
  }

  /// FR-38: tutup tab = tutup semua pane-nya, satu konfirmasi berisi nama proses yang masih jalan
  async function closeTabs(wi: number, ts: Tab[]) {
    const ps = ts.flatMap((t) => panes(t.tree));
    const live = ps.filter((p) => !dead[p]);
    ps.filter((p) => dead[p]).forEach(onexit);
    const infos = await Promise.all(live.map((p) => call("pane.info", { pane: p }).then((i) => ({ p, i }))));
    const busy = infos.filter((x) => x.i.busy);
    if (!busy.length) return live.forEach((p) => call("pane.close", { pane: p }).catch(() => {}));
    const t = ts.find((t) => panes(t.tree).includes(busy[0].p))!;
    at = { ws: wi, tab: wss[wi].tabs.indexOf(t) };
    zoomed = false;
    confirm = { pane: t.focus, name: busy.map((x) => x.i.name ?? "proses").join(", "), close: live, what: ts.length > 1 ? `${ts.length} tab` : "tab" };
  }

  /// Pane keluar: hilang dari tabnya; tab kosong ditutup; workspace boleh kosong (FR-44).
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
      return;
    }
  }

  /// FR-45: pane mati diganti pane baru dari profil dan folder yang sama; `shell` = shell saja,
  /// agent lamanya dicatat di `prev` untuk chip "Lanjutkan sesi"
  async function restart(old: number, shell = false) {
    const m = meta[old];
    const id = await createPane({
      profile: shell || m?.profile === "shell" ? undefined : m?.profile,
      cwd: m?.cwd ?? undefined,
      session: shell ? undefined : m?.session,
      resume: !shell && AGENTS.includes(m?.agent ?? ""), // FR-53: lanjutkan percakapan pane ini
      workspace: wsIdOf(old),
      history: m?.from, // FR-62
      account: shell ? undefined : m?.acct,
    });
    if (shell && m && AGENTS.includes(m.agent)) meta[id].prev = { profile: m.profile, agent: m.agent, session: m.session, acct: m.acct };
    if (shell && m?.ended) meta[id].ended = m.ended; // FR-56: kartu sesi selesai ikut ke shell pengganti
    // FR-68: agent yang diketik ikut berhenti bersama daemon = kartu lanjut dari id sesinya (perintah lolos pola aman)
    else if (shell && m && !AGENTS.includes(m.agent) && m.busy && m.session) {
      // sama dengan daemon (FR-61): sesi milik akun lain = `claude-<pemilik>`, selain itu akun proses; `claude` polos = akun shell
      const who = m.owner?.id ?? m.accountId;
      const cmd = { claude: `${who ? `claude-${who}` : "claude"} --resume ${m.session}`, codex: `codex resume ${m.session}`, opencode: `opencode --session ${m.session}`, omp: `omp --resume ${m.session}` }[m.busy];
      if (safeResume(cmd)) meta[id].ended = { resume: cmd ?? null };
    }
    for (const w of wss)
      for (const t of w.tabs)
        if (panes(t.tree).includes(old)) {
          t.tree = replace(t.tree, old, id);
          if (t.focus === old) t.focus = id;
        }
    delete meta[old];
    delete dead[old];
  }

  /// FR-45: chip "Lanjutkan sesi": shell diganti agent lamanya dengan id sesi yang sama (FR-53)
  function resumePrev(pane: number) {
    const { prev, ...m } = meta[pane];
    if (!prev) return;
    meta[pane] = { ...m, ...prev };
    relaunch(pane);
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
          meta[id] = { ...(meta[p] ?? { profile: "shell", cwd: null, agent: "", name: "" }), from: { boot, pane: p } };
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
    const accs = cfg?.values.accounts ?? [];
    // FR-61: satu baris per akun bila agent itu punya akun selain Utama
    const items = (cfg?.values.profiles ?? []).flatMap((p) => {
      const own = accs.filter((a) => a.agent === p.agent);
      const base = { ...p, missing: missing.has(p.agent), key: p.id, label: p.name, account: undefined as string | undefined };
      return own.length ? [{ id: "utama", name: "Utama" }, ...own].map((a) => ({ ...base, key: `${p.id}:${a.id}`, label: `${p.name} · ${a.name}`, account: a.id })) : [base];
    });
    picker = { items, sel: 0 };
  }

  function pick(i: number) {
    const p = picker?.items[i];
    picker = null;
    if (p) newTab(p.id === "shell" ? undefined : p.id, at.ws, p.account);
  }

  /// FR-61: tombol Login = pane baru dengan akun itu + kartu petunjuk; Gembala tidak mengetik apa pun ke terminal
  async function loginAccount(id: string, name: string) {
    settingsOpen = false;
    const pane = await newTab("claude", at.ws, id);
    if (meta[pane]) meta[pane].login = name;
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
    if (settingsOpen || picker || editing || rich || pal || lempar) return;
    if (confirm || quitAsk || linkAsk || wsAsk) return;
    // satu tombol bisa sampai lewat menu dan keydown sekaligus; jangan jalan dua kali
    if (id === last.id && performance.now() - last.at < 150) return;
    last = { id, at: performance.now() };
    const [, group, arg] = id.split(".");
    const cur = order.findIndex((o) => o.wi === at.ws && o.ti === at.tab);
    if (id === "msg.drawer") return void ((drawer = !drawer), drawer && refreshMsgs());
    if (id === "msg.stop") return void call("msg.pause", { on: !msgData?.paused }).then(refreshMsgs);
    if (id === "msg.compose") {
      compose = { to: targets[0]?.pane ?? null, body: "", force: false, error: "" };
      return;
    }
    // aksi tanpa tab: workspace boleh kosong (FR-44)
    if (id === "tab.new") return void openPicker();
    if (id === "tab.sidebar") return void (sidebar = !sidebar);
    if (id === "tab.search") return void openPalette();
    if (id === "tab.prev") return goto((cur + order.length - 1) % order.length);
    if (id === "tab.next") return goto((cur + 1) % order.length);
    if (id === "pane.attention.next") {
      if (!queue.length) return;
      const i = queue.findIndex((q) => q.p === tab?.focus);
      return jumpTo(queue[(i + 1) % queue.length].p);
    }
    if (group === "goto") return goto(+arg - 1);
    if (!tab) return;
    if (group === "split") splitFocused(arg === "down" ? "col" : "row");
    else if (group === "focus" && !zoomed) tab.focus = neighbor(tab.tree, tab.focus, arg as Arrow) ?? tab.focus;
    else if (group === "resize" && !zoomed) tab.tree = nudge(tab.tree, tab.focus, arg as Arrow);
    else if (group === "swap" && !zoomed) {
      // FR-37: pane fokus bertukar tempat dengan tetangganya; fokus ikut pane yang dipindah
      const other = neighbor(tab.tree, tab.focus, arg as Arrow);
      if (other !== null) tab.tree = swap(tab.tree, tab.focus, other);
    }
    else if (id === "pane.zoom.toggle") zoomed = !zoomed;
    else if (id === "pane.move.newtab") moveToNewTab();
    else if (id === "pane.input.rich") rich = { pane: tab.focus, text: "" };
    else if (id === "pane.diff.toggle") openChanges(tab.focus);
    else if (id === "pane.proc.kill") call("pane.kill", { pane: tab.focus });
    else if (id === "pane.proc.close") askClose(tab.focus);
    else if (id === "term.find") tiles[tab.focus]?.openFind();
    else if (id === "term.clear") tiles[tab.focus]?.clearScreen();
    else if (id === "term.block.prev" || id === "term.block.next") tiles[tab.focus]?.jump(arg === "prev" ? -1 : 1);
    else if (id === "term.zoom.in") zoom = Math.min(16, zoom + 1);
    else if (id === "term.zoom.out") zoom = Math.max(-6, zoom - 1);
    else if (id === "term.zoom.reset") zoom = 0;
    else if (id.startsWith("term.zoom.pane") && meta[tab.focus]) {
      const m = meta[tab.focus];
      m.zoom = arg === "pane_reset" ? 0 : Math.max(-6, Math.min(16, (m.zoom ?? 0) + (arg === "pane_in" ? 1 : -1)));
    }
    // zoom tanpa tanda = tidak tahu ukuran sekarang; angka sama dengan Pengaturan > Tampilan > Ukuran huruf
    if (id.startsWith("term.zoom")) {
      const base = cfg?.values.ui.font_size ?? 13;
      const pz = id.startsWith("term.zoom.pane") ? (meta[tab.focus]?.zoom ?? 0) : 0;
      say(`Ukuran huruf ${Math.max(6, base + zoom + pz)}${pz ? " (pane ini)" : ""} · bawaan ${base}`);
    }
  }

  /// FR-39: palet = semua tab + profil + semua aksi beserta pintasannya (sekaligus daftar pintasan)
  type PalItem = { label: string; hint: string; go: () => void };
  async function openPalette() {
    // ⌘P lagi = tutup
    if (pal) return void (pal = null);
    const [keys] = await invoke<[[string, string, string][], string[]]>("shortcuts").catch(() => [[]] as any);
    pal = { q: "", sel: 0, keys };
    // autofocus bisa kalah oleh editor pane yang merebut fokus; tanpa fokus palet tidak bisa dipakai/ditutup
    await tick();
    document.querySelector<HTMLInputElement>(".palette input")?.focus();
    setTimeout(() => pal && document.querySelector<HTMLInputElement>(".palette input")?.focus(), 50);
  }
  const palItems = $derived.by((): PalItem[] => {
    if (!pal) return [];
    const items: PalItem[] = [
      ...order.map((o, i) => ({ label: title(o.t), hint: `${wss[o.wi].name}${i < 9 ? ` · ⌘${i + 1}` : ""}`, go: () => goto(i) })),
      ...(cfg?.values.profiles ?? []).map((p) => ({ label: `Tab baru: ${p.name}`, hint: p.command || "$SHELL", go: () => newTab(p.id === "shell" ? undefined : p.id) })),
      ...pal.keys.filter(([id]) => !id.startsWith("tab.goto.") && id !== "tab.search").map(([id, label, key]) => ({ label, hint: pretty(key), go: () => run(id) })),
      { label: "Agent di worktree baru…", hint: "git worktree dari HEAD repo pane ini", go: () => openWorktree() },
      { label: "Dari template peran…", hint: "koordinator, pekerja, reviewer + brief workspace", go: () => openTemplate() },
      ...pickable.map((p) => ({ label: `Pilih sesi lama: ${p.label}`, hint: "pemilih sesi bawaan agent", go: () => newTab(p.id, at.ws, p.account, { pick: true }) })),
      { label: "Workspace baru…", hint: "", go: addWorkspace },
      { label: editorOn ? "Matikan input editor (pane baru)" : "Nyalakan input editor (pane baru)", hint: "", go: () => call("config.set", { section: "ui", key: "input_editor", value: !editorOn }).catch(() => {}) },
      { label: "Ganti nama tab", hint: "", go: () => tab && (editing = { kind: "tab", id: tab.id, value: title(tab) }) },
      { label: "Ganti nama workspace", hint: "", go: () => ws && (editing = { kind: "ws", id: ws.id, value: ws.name }) },
      { label: "Pengaturan", hint: "⌘,", go: () => (settingsOpen = true) },
      { label: "Keluar dan hentikan semua agent…", hint: "", go: askQuitAll },
    ];
    const words = pal.q.toLowerCase().split(/\s+/).filter(Boolean);
    const hay = (it: PalItem) => `${it.label} ${it.hint}`.toLowerCase();
    // urutan: cocok persis dulu, lalu yang terakhir dipakai; sort stabil = urutan asal untuk sisanya
    const rank = (it: PalItem) => {
      const r = recent.indexOf(it.label);
      return (words.every((w) => hay(it).includes(w)) ? 0 : 1000) + (r < 0 ? 100 : r);
    };
    return items.filter((it) => words.every((w) => fuzzy(hay(it), w))).sort((a, b) => rank(a) - rank(b));
  });
  /// huruf kata dicari berurutan (boleh berjarak): "spl kn" cocok "split ke kanan"
  const fuzzy = (hay: string, w: string) => {
    let i = 0;
    for (const c of hay) if (c === w[i]) i++;
    return i === w.length;
  };
  // aksi palet yang terakhir dipakai (kenyamanan per pengguna; storage bisa gagal = diabaikan)
  let recent: string[] = (() => {
    try {
      return JSON.parse(localStorage.getItem("gembala.palet.terakhir") ?? "[]");
    } catch {
      return [];
    }
  })();
  $effect(() => {
    if (pal) void pal.sel, document.querySelector(".pallist .on")?.scrollIntoView({ block: "nearest" });
  });
  function palRun(i: number) {
    const it = palItems[i];
    pal = null;
    if (!it) return;
    recent = [it.label, ...recent.filter((l) => l !== it.label)].slice(0, 20);
    try {
      localStorage.setItem("gembala.palet.terakhir", JSON.stringify(recent));
    } catch {}
    it.go();
    if (editing) sidebar = true; // ganti nama terjadi di sidebar
  }

  /// Menu klik kanan native (FR-37, FR-44, FR-52). Item: [label, aksi, aktif?], "-" pemisah, [label, sub-item[]] submenu.
  // elemen ke-4 = warna: bulatan digambar sebagai ikon item menu native (bukan emoji)
  type MItem = "-" | [string, (() => void) | MItem[], boolean?, string?];
  const opts = (items: MItem[]): any[] =>
    items.map((it) =>
      it === "-"
        ? { item: "Separator" }
        : Array.isArray(it[1])
          ? { text: it[0], items: opts(it[1]) }
          : { text: it[0], action: it[1], enabled: it[2] ?? true, ...(it[3] && dots[it[3]] ? { icon: dots[it[3]] } : {}) },
    );
  /// FR-44/52: bulatan warna 36px (menu macOS menampilkan ikon setinggi 18pt); "none" = lingkaran kosong
  let dots: Record<string, Image> = {};
  async function makeDots() {
    const hex: Record<string, string> = { ...HEX, none: "" };
    const out: Record<string, Image> = {};
    for (const [k, c] of Object.entries(hex)) {
      const cv = Object.assign(document.createElement("canvas"), { width: 36, height: 36 });
      const g = cv.getContext("2d")!;
      g.beginPath();
      g.arc(18, 18, c ? 13 : 12, 0, Math.PI * 2);
      if (c) (g.fillStyle = c), g.fill();
      else (g.strokeStyle = "#8b8b8b"), (g.lineWidth = 2.5), g.stroke();
      out[k] = await Image.new(g.getImageData(0, 0, 36, 36).data.buffer, 36, 36);
    }
    dots = out;
  }
  async function popup(e: MouseEvent, items: MItem[]) {
    e.preventDefault();
    e.stopPropagation();
    // bulatan warna belum ada (gagal saat app dibuka) = coba lagi sebelum menu dibangun
    if (!Object.keys(dots).length) await makeDots().catch((err) => say(`Ikon warna gagal dimuat: ${err}`));
    (await Menu.new({ items: opts(items) })).popup().catch(() => {});
  }
  const copy = (text: string | null | undefined) =>
    text && invoke("copy_text", { text }).then(() => say("Tersalin"), () => say("Gagal menyalin"));

  // umpan balik singkat untuk aksi tanpa hasil yang terlihat (bagian 11 BRD); role=status = dibacakan VoiceOver
  let toast = $state("");
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  function say(msg: string) {
    toast = msg;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = ""), 1800);
  }

  function tabMenu(e: MouseEvent, wi: number, t: Tab) {
    const ti = wss[wi].tabs.indexOf(t);
    const others = wss.map((w, j) => [w.name, () => moveTab(wi, wss[wi].tabs.indexOf(t), j, w.tabs.length)] as MItem).filter((_, j) => j !== wi);
    popup(e, [
      ["Ganti nama", () => (editing = { kind: "tab", id: t.id, value: title(t) })],
      ["Warna", colorMenu(t.color, (c) => (t.color = c))],
      others.length ? ["Pindah ke workspace", others] : ["Pindah ke workspace", () => {}, false],
      ["Salin folder", () => copy(meta[t.focus]?.cwd)],
      ["Agent di worktree baru…", () => ((at = { ws: wi, tab: ti }), openWorktree(wi))],
      ...(meta[t.focus]?.git?.branch?.startsWith("gembala/") ? [["Tutup dan hapus worktree…", () => removeWorktree(wi, t)] as MItem] : []),
      "-",
      ["Tutup tab", () => closeTabs(wi, [t])],
      ["Tutup tab lain", () => closeTabs(wi, wss[wi].tabs.filter((x) => x !== t)), wss[wi].tabs.length > 1],
      ["Tutup tab di bawahnya", () => closeTabs(wi, wss[wi].tabs.slice(ti + 1)), ti < wss[wi].tabs.length - 1],
    ]);
  }

  /// FR-52: tab baru dari profil di workspace tertentu (tombol + di kepala workspace)
  async function profileMenu(e: MouseEvent, wi: number) {
    e.preventDefault();
    e.stopPropagation();
    const agents: Agent[] = await call("agents.detect", {}).catch(() => []);
    const missing = new Set(agents.filter((a) => !a.installed).map((a) => a.agent));
    popup(e, [
      ...(cfg?.values.profiles ?? []).map((p) => [`${p.name}${missing.has(p.agent) ? " (tidak terpasang)" : ""}`, () => newTab(p.id === "shell" ? undefined : p.id, wi)] as MItem),
      "-",
      ["Agent di worktree baru…", () => openWorktree(wi)],
      ["Dari template peran…", () => openTemplate(wi)],
      pickable.length
        ? ["Pilih sesi lama…", pickable.map((p) => [p.label, () => newTab(p.id, wi, p.account, { pick: true })] as MItem)]
        : ["Pilih sesi lama…", () => {}, false],
    ]);
  }

  function wsMenu(e: MouseEvent, wi: number) {
    const w = wss[wi];
    popup(e, [
      ["Ganti nama", () => (editing = { kind: "ws", id: w.id, value: w.name })],
      ["Ganti folder…", () => changeRoot(w)],
      ["Warna", colorMenu(w.color, (c) => (w.color = c ?? null))],
      // FR-61: akun Claude bawaan pane baru di workspace ini (pilihan manusia, C-9)
      ...((cfg?.values.accounts ?? []).some((a) => a.agent === "claude")
        ? [
            "-" as MItem,
            ...[{ id: "utama", name: "Utama" }, ...(cfg?.values.accounts ?? []).filter((a) => a.agent === "claude")].map(
              (a): MItem => [`${(w.accounts?.claude ?? "utama") === a.id ? "✓ " : ""}Akun Claude bawaan: ${a.name}`, () => void (w.accounts = { ...w.accounts, claude: a.id })],
            ),
          ]
        : []),
      [w.collapsed ? "Buka" : "Ciutkan", () => (w.collapsed = !w.collapsed)],
      "-",
      ["Hapus workspace", () => askRemoveWorkspace(wss.indexOf(w)), wss.length > 1],
    ]);
  }

  /// FR-37: klik kanan terminal
  function paneMenu(e: MouseEvent, pane: number) {
    const tile = tiles[pane];
    const blk = tile?.blockAt(e.clientY); // FR-58: blok perintah di bawah kursor
    popup(e, [
      ["Salin", () => copy(tile?.selection()), !!tile?.selection()],
      ["Tempel", async () => tile?.paste(await invoke<string>("paste_text").catch(() => ""))],
      "-",
      ["Salin perintah blok ini", () => copy(blk?.cmd), !!blk?.cmd],
      ["Salin output blok ini", () => copy(blk?.output), !!blk?.output],
      // FR-58: perintah blok masuk editor untuk diubah lalu dijalankan lagi (ala Warp)
      ["Pakai lagi di editor", () => blk && ((draft[pane] = blk.cmd), document.querySelector<HTMLTextAreaElement>(`[data-editor="${pane}"]`)?.focus()), !!blk?.cmd && editorShown(pane)],
      "-",
      ["Split kanan", () => splitFocused("row")],
      ["Split bawah", () => splitFocused("col")],
      [zoomed ? "Kembalikan ukuran" : "Maksimalkan", () => (zoomed = !zoomed), zoomed || (!!tab && panes(tab.tree).length > 1)],
      ["Bersihkan", () => tile?.clearScreen()],
      "-",
      ["Tutup pane", () => askClose(pane)],
    ]);
  }

  /// FR-44: folder akar baru berlaku untuk tab baru; tab yang sudah ada tetap di foldernya
  async function changeRoot(w: Ws) {
    const path = await invoke<string | null>("pick_folder");
    if (path) w.root = home(path);
  }

  /// `at` tetap menunjuk tab yang sama setelah urutan berubah
  function keepAt(change: () => void) {
    const tid = tab?.id;
    const wid = ws?.id;
    change();
    const wi = wss.findIndex((w) => w.tabs.some((t) => t.id === tid));
    at = wi >= 0 ? { ws: wi, tab: wss[wi].tabs.findIndex((t) => t.id === tid) } : { ws: Math.max(0, wss.findIndex((w) => w.id === wid)), tab: 0 };
  }

  /// FR-52: tab pindah posisi atau workspace; daemon diberi tahu workspace baru pane-nya (aturan akses SEC-4/21)
  function moveTab(wi: number, ti: number, to: number, index: number) {
    keepAt(() => {
      const [t] = wss[wi].tabs.splice(ti, 1);
      wss[to].tabs.splice(Math.min(index, wss[to].tabs.length), 0, t);
      if (wi !== to) for (const p of panes(t.tree)) if (!dead[p]) call("pane.workspace", { pane: p, workspace: wss[to].id }).catch(() => {});
    });
  }

  /// FR-44/52: seret tab dan workspace di sidebar (pointer, bukan drag HTML: WKWebView + drop file Tauri).
  /// Pen layar 09: baris asal tetap di tempat (pudar), salinannya ikut kursor, garis sisip biru di tempat jatuh;
  /// baris lain tidak bergeser. Target = baris terdekat menurut Y (celah antar baris ikut terhitung). Esc = batal.
  function startDrag(e: PointerEvent, kind: "tab" | "ws", wi: number, ti = -1) {
    if (e.button !== 0 || editing) return;
    const row = e.currentTarget as HTMLElement;
    const side = row.closest<HTMLElement>(".side")!;
    const [x0, y0] = [e.clientX, e.clientY];
    let ghost: HTMLElement | null = null;
    let esc = false;
    const move = (m: PointerEvent) => {
      if (esc || (!dragging && Math.hypot(m.clientX - x0, m.clientY - y0) < 5)) return;
      if (!ghost) {
        // salinan baris dibungkus salinan kosong .wsg-nya, jadi warna blok dan garis aksen tetap kena
        const g = row.parentElement!.getBoundingClientRect();
        ghost = document.createElement("div");
        ghost.className = "ghost";
        ghost.append(row.parentElement!.cloneNode(false));
        ghost.firstChild!.appendChild(row.cloneNode(true));
        Object.assign(ghost.style, { left: `${g.left}px`, top: `${row.getBoundingClientRect().top}px`, width: `${g.width}px` });
        document.body.append(ghost);
      }
      ghost.style.transform = `translate(${m.clientX - x0}px, ${m.clientY - y0}px)`;
      const y = m.clientY;
      let over: { wi: number; ti: number };
      let top: number;
      let box: DOMRect; // kartu workspace tujuan: lebar garis sisip
      if (kind === "ws") {
        // blok workspace = kartu .wsg; sisip sebelum/sesudah kartu terdekat
        const gs = [...side.querySelectorAll<HTMLElement>(".wsg")].map((el) => el.getBoundingClientRect());
        const j = nearest(gs, y);
        const after = y > (gs[j].top + gs[j].bottom) / 2;
        over = { wi: j + +after, ti: -1 };
        top = after ? gs[j].bottom + 1 : gs[j].top - 4;
        box = gs[j];
      } else {
        // semua baris sidebar urut DOM: kepala workspace, tab, "+ Tab" workspace kosong
        const els = [...side.querySelectorAll<HTMLElement>("[data-k]")];
        const rs = els.map((el) => el.getBoundingClientRect());
        const k = nearest(rs, y);
        const el = els[k];
        const head = el.dataset.ti === undefined;
        // kepala workspace (juga yang terlipat, atau "+ Tab" workspace kosong) = jatuh di posisi pertama workspace itu
        const after = head || y > (rs[k].top + rs[k].bottom) / 2;
        over = { wi: +el.dataset.wi!, ti: head ? 0 : +el.dataset.ti! + +after };
        top = after ? rs[k].bottom : rs[k].top - 2;
        box = el.parentElement!.getBoundingClientRect();
      }
      dragging = { kind, wi, ti, over, line: { top, left: box.left, width: box.width } };
    };
    const key = (k: KeyboardEvent) => {
      if (k.key !== "Escape" || !dragging) return;
      k.preventDefault();
      k.stopPropagation();
      esc = true;
      stop();
      window.addEventListener("pointerup", up); // klik penutup seretan yang dibatalkan tetap dimakan
    };
    const stop = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", stop);
      window.removeEventListener("keydown", key, true);
      ghost?.remove();
      const d = dragging;
      dragging = null;
      return d;
    };
    const up = () => {
      const d = stop();
      if (!d && !esc) return;
      // klik yang menutup seretan tidak ikut memindah tab (click datang tepat setelah pointerup, bila ada)
      const eat = (c: MouseEvent) => c.stopPropagation();
      window.addEventListener("click", eat, true);
      setTimeout(() => window.removeEventListener("click", eat, true), 0);
      if (!d) return;
      if (d.kind === "ws") {
        const to = d.over.wi - +(d.over.wi > d.wi);
        if (to !== d.wi) keepAt(() => wss.splice(to, 0, ...wss.splice(d.wi, 1)));
      } else {
        const to = d.over.ti - +(d.over.wi === d.wi && d.over.ti > d.ti);
        if (d.over.wi !== d.wi || to !== d.ti) moveTab(d.wi, d.ti, d.over.wi, to);
      }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", stop);
    window.addEventListener("keydown", key, true);
  }
  function nearest(rs: { top: number; bottom: number }[], y: number) {
    const gap = (r: { top: number; bottom: number }) => Math.max(r.top - y, y - r.bottom, 0);
    return rs.reduce((b, r, i) => (gap(r) < gap(rs[b]) ? i : b), 0);
  }

  /// FR-52: lebar sidebar diseret
  function dragSide(e: PointerEvent) {
    const move = (m: PointerEvent) => (sideW = Math.min(480, Math.max(160, m.clientX)));
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    e.preventDefault();
  }

  /// FR-56: tombol Lanjutkan. Pane profil agent: jalankan ulang profilnya dengan id sesi (flag hook tetap);
  /// agent yang diketik sendiri: perintah lanjut diketik ke shell yang sedang diam.
  async function resumeEnded(pane: number) {
    const m = meta[pane];
    const cmd = m?.ended?.resume;
    if (!m || !cmd) return;
    delete m.ended;
    if (AGENTS.includes(m.agent)) return relaunch(pane);
    const info = await call("pane.info", { pane }).catch(() => ({ busy: true }));
    // Ctrl+U first: leftover text on the zsh line (e.g. a pasted image path) would become part of the command
    if (!info.busy) call("pane.write", { pane, data: b64(new TextEncoder().encode(`\x15${cmd}\r`)) });
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
    if (rich || pal || lempar) return; // textarea Rich Input, palet, dan bar lempar menangani tombolnya sendiri
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
    if (editing) {
      if (e.key === "Escape") {
        editing = null;
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
    if (e.key === "Enter") confirm.close.forEach((pane) => call("pane.close", { pane }).catch(() => {}));
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

  /// FR-44: workspace = satu folder project dipilih lewat dialog; nama = nama folder (ganti dengan klik dua kali);
  /// tab barunya mulai di folder itu
  async function addWorkspace() {
    const path = await invoke<string | null>("pick_folder");
    if (!path) return;
    const root = home(path);
    wss.push({ id: uid(), name: base(path).slice(0, 60) || "workspace", root, tabs: [], color: nextColor(wss) });
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
    const out: Ws[] = saved.workspaces
      .filter((w: any) => typeof w?.id === "string" && Array.isArray(w.tabs))
      .map((w: any) => ({
        id: w.id,
        name: String(w.name ?? "workspace").slice(0, 60),
        root: String(w.root ?? "~").slice(0, 500),
        collapsed: w.collapsed === true,
        // FR-44: warna blok; workspace lama tanpa warna diberi warna di bawah (null = sengaja tanpa warna)
        color: COLORS.some(([c]) => c === w.color) ? w.color : w.color === null ? null : undefined,
        // FR-61: hanya id akun yang sah (dicocokkan lagi oleh daemon saat membuka pane)
        brief: typeof w.brief === "string" && w.brief.trim() ? w.brief.slice(0, 4000) : undefined,
        accounts: Object.fromEntries(Object.entries(w.accounts ?? {}).filter(([k, v]) => /^[a-z]+$/.test(k) && typeof v === "string" && /^[a-z0-9-]{1,32}$/.test(v))),
        tabs: w.tabs
          .filter((t: any) => typeof t?.id === "string" && valid(t.tree))
          .map((t: any) => ({
            id: t.id,
            title: String(t.title ?? "").slice(0, 60),
            tree: t.tree,
            focus: panes(t.tree).includes(t.focus) ? t.focus : panes(t.tree)[0],
            color: COLORS.some(([c]) => c === t.color) ? t.color : undefined,
          })),
      }));
    for (const w of out) if (w.color === undefined) w.color = nextColor(out);
    return out;
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
            meta[id] = { ...(oldMeta[p] ?? { profile: "shell", cwd: null, agent: "", name: "" }), from: { boot: saved.boot, pane: p } };
            dead[id] = true;
          }
      wss = restored;
      // layout.json tidak tepercaya: kartu FR-56 tetap, perintah lanjutnya hanya bila cocok bentuk buatan daemon
      for (const m of Object.values(meta)) if (m.ended) m.ended = { resume: safeResume(m.ended.resume) };
      // FR-45: pane mati langsung diganti shell baru (id negatif dulu supaya tidak bentrok dengan id run baru)
      for (const d of Object.keys(dead).map(Number)) await restart(d, true);
      // pane hidup yang tidak ada di layout (mis. layout gagal tersimpan) tetap tampil
      for (const p of alive) wss[0].tabs.push({ id: uid(), title: "", tree: { pane: p }, focus: p });
      const a = saved.at ?? {};
      at = wss[a.ws]?.tabs[a.tab] ? { ws: a.ws, tab: a.tab } : { ws: 0, tab: 0 };
      if (typeof saved.side === "number") sideW = Math.min(480, Math.max(160, saved.side));
      if (typeof saved.panel === "number") panelW = Math.min(1600, Math.max(320, saved.panel));      return;
    }
    wss = [{ id: uid(), name: "default", root: "~", tabs: [], color: FIRST }];
    const ids = [...alive];
    if (ids.length) for (const p of ids) wss[0].tabs.push({ id: uid(), title: "", tree: { pane: p }, focus: p });
    else await newTab(undefined, 0);
    at = { ws: 0, tab: 0 };
    welcome = await call("agents.detect", {}).catch(() => []);
  }

  // FR-65: antrean perhatian = semua pane yang butuh izin, paling lama menunggu dulu
  $effect(() => {
    for (const p of Object.keys(meta).map(Number)) {
      if (meta[p]?.status === "blocked") blockedAt[p] ??= Date.now();
      else if (blockedAt[p]) delete blockedAt[p];
    }
  });
  const queue = $derived(
    wss
      .flatMap((w, wi) => w.tabs.flatMap((t, ti) => panes(t.tree).filter((p) => !dead[p] && meta[p]?.status === "blocked").map((p) => ({ p, wi, ti, t, w }))))
      .sort((a, b) => (blockedAt[a.p] ?? 0) - (blockedAt[b.p] ?? 0)),
  );
  const ago = (ms: number) => {
    const s = Math.max(0, Math.round(ms / 1000));
    return s < 60 ? `${s} dtk` : s < 3600 ? `${Math.floor(s / 60)} mnt` : `${Math.floor(s / 3600)} jam`;
  };
  /// FR-65: pindah ke tab dan pane itu (klik baris antrean, Cmd+Shift+U)
  function jumpTo(p: number) {
    for (const [wi, w] of wss.entries())
      for (const [ti, t] of w.tabs.entries())
        if (panes(t.tree).includes(p)) {
          at = { ws: wi, tab: ti };
          t.focus = p;
          zoomed = false;
        }
  }
  const tabOf = (p: number) => wss.flatMap((w) => w.tabs).find((t) => panes(t.tree).includes(p));

  // FR-40: tanda selesai hilang begitu pane itu dilihat (agent maupun shell)
  $effect(() => {
    const f = tab?.focus;
    if (seen && f && meta[f]?.status === "done" && !ack[f]) ack[f] = true;
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

  /// FR-40: banner + Dock melompat saat jendela tidak dilihat; isi nama pane + status saja (SEC-33).
  /// Kembali ke app tidak memindah pane: plugin tidak memberi callback klik, dan event fokus jendela
  /// tidak bisa membedakan klik notifikasi dari Cmd+Tab (gesekan 2 Nov 2026).
  function alert(pane: number, kind: "blocked" | "done", label: string) {
    const m = meta[pane];
    if (seen || !m || !(kind === "blocked" ? notify.on_blocked : notify.on_done)) return;
    if (cfg?.values.profiles.find((p) => p.id === m.profile)?.notify === false) return;
    if (kind === "blocked") invoke("attention").catch(() => {}); // Dock melompat hanya bila perlu tindakan; selesai cukup badge
    // SEC-33: nama pane, nama tab (FR-65), status; tanpa isi layar
    const t = tabOf(pane);
    invoke("notify_pane", { body: `${m.name || m.profile}${t ? ` di ${title(t)}` : ""} ${label}`, sound: notify.sound }).catch(() => {});
  }
  const attention = $derived(
    Object.keys(meta).map(Number).filter((p) => (stat(p) === "blocked" && notify.on_blocked) || (stat(p) === "done" && notify.on_done)).length,
  );
  $effect(() => {
    invoke("badge", { count: seen ? 0 : attention }).catch(() => {});
  });
  function onStatus(pane: number, status: string) {
    if (!meta[pane]) return;
    delete ack[pane];
    meta[pane].status = status;
    if (status === "done" || status === "idle") {
      refreshGit(pane); // toolbar diperbarui per giliran (TECH 7a)
      if (changes?.pane === pane) loadChanges(pane); // FR-22: panel ikut diperbarui
    }
    if (status === "blocked") alert(pane, "blocked", "butuh izin");
    if (status === "done") alert(pane, "done", "selesai");
  }

  // simpan tata letak setengah detik setelah perubahan terakhir (SEC-13: tanpa isi layar)
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const body = JSON.stringify({ version: 1, boot, at, side: sideW, panel: panelW, workspaces: wss, panes: meta });
    if (!wss.length || !boot) return;
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => call("layout.save", { layout: JSON.parse(body) }).catch(() => {}), 500);
  });

  // folder dan proses tiap pane untuk judul tab, subjudul, dan ikon agent (FR-52)
  async function poll() {
    now = Date.now();
    const ids = Object.keys(meta).map(Number).filter((p) => !dead[p]);
    // dikirim sekaligus (klien daemon memultipleks per id), tidak menunggu satu per satu (TECH 19 Responsif)
    const infos = await Promise.all(ids.map((p) => call("pane.info", { pane: p }).catch(() => null)));
    for (const [k, p] of ids.entries()) {
      const i = infos[k];
      if (!i || !meta[p]) continue;
      if (i.cwd && meta[p].cwd !== i.cwd) {
        meta[p].cwd = i.cwd;
        refreshGit(p);
      }
      const busy = i.busy ? (i.name ?? "proses") : null;
      const m = meta[p];
      // agent yang diketik di pane shell (`m.busy` = agent di poll sebelumnya): status kosong dari daemon saat agent
      // keluar juga dipakai, supaya "selesai" tidak tertahan setelah agentnya mati
      if (AGENTS.includes(m.agent) || m.agent === "custom" || i.status || AGENTS.includes(m.busy ?? "")) {
        // status dari daemon: hook resmi, atau pola layar untuk profil custom dan agent yang diketik manual (FR-4)
        if (m.status !== i.status) {
          delete ack[p];
          m.status = i.status;
        }
      } else {
        // FR-40 untuk pane shell: perintah lebih dari 30 detik lalu selesai = done; menunggu password = blocked
        if (busy && !m.busy) m.since = Date.now();
        if (i.password && m.status !== "blocked") {
          m.status = "blocked";
          alert(p, "blocked", "menunggu password");
        } else if (!i.password && m.status === "blocked") m.status = "";
        if (!busy && m.busy && Date.now() - (m.since ?? Date.now()) >= notify.long_command_secs * 1000) {
          delete ack[p];
          m.status = "done";
          alert(p, "done", "selesai menjalankan perintah lama");
        }
      }
      if (busy && busy !== m.busy && m.ended) delete m.ended; // FR-56: blok selesai hilang begitu proses baru jalan
      if (m.busy !== busy) m.busy = busy;
      if (m.shell !== i.shell) m.shell = i.shell;
      // FR-69: label hanya selama agent berjalan di pane ini
      const label = AGENTS.includes(m.agent) || AGENTS.includes(busy ?? "") ? cleanTitle(i.title, base(i.cwd)) : "";
      if (label) labels[p] !== label && (labels[p] = label);
      else if (labels[p]) delete labels[p];
      if (i.pane_name && meta[p].name !== i.pane_name) meta[p].name = i.pane_name;
      if (meta[p].policy !== i.policy) meta[p].policy = i.policy;
      if (meta[p].readable !== i.readable) meta[p].readable = i.readable;
      if (i.session && meta[p].session !== i.session) meta[p].session = i.session; // FR-68
      if ((i.account ?? "") !== (meta[p].accountName ?? "")) meta[p].accountName = i.account || undefined; // FR-61
      if ((i.account_id ?? "") !== (meta[p].accountId ?? "")) meta[p].accountId = i.account_id || undefined; // FR-68
      if ((i.owner_id ?? "") !== (meta[p].owner?.id ?? "")) meta[p].owner = i.owner_id ? { name: i.owner, id: i.owner_id } : undefined;
    }
  }

  // Esc yang tidak dipakai halaman diteruskan WebKit ke jendela = keluar layar penuh macOS; layar penuh cukup fn+F
  const keepFullscreen = (e: KeyboardEvent) => e.key === "Escape" && e.preventDefault();
  let fullscreen = $state(false); // layar penuh: tombol jendela macOS hilang, ruangnya di sidebar ikut hilang

  onMount(() => {
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keydown", keepFullscreen);
    const win = getCurrentWindow();
    const onFull = () => win.isFullscreen().then((f) => (fullscreen = f)).catch(() => {});
    onFull();
    const unlistenFull = win.onResized(onFull);
    const unlisten = listen<string>("shortcut", ({ payload }) => run(payload));
    const unlistenCfg = listen<any>("daemon", ({ payload }) => {
      if (payload.event === "config.changed") cfg = payload.params;
      if (payload.event === "pane.status") onStatus(payload.params.pane, payload.params.status);
      if (payload.event === "pane.replace" && meta[payload.params.pane])
        // agent keluar = status kosong (daemon mengosongkan tanpa event pane.status; tanpa ini tetap "bekerja")
        Object.assign(meta[payload.params.pane], { output: true, status: "", ended: { resume: payload.params.resume ?? null } });
      if (payload.event === "daemon.lost") onLost();
      if (payload.event === "msg.event") refreshMsgs();
    });
    const timer = setInterval(poll, 2000);
    // FR-15: seret file ke jendela = path ber-quote ditempel ke pane yang fokus (bracketed paste, tanpa Enter)
    const unlistenDrop = getCurrentWebview().onDragDropEvent(({ payload }) => {
      if (payload.type !== "drop" || !tab || dead[tab.focus] || !payload.paths.length) return;
      const pane = tab.focus;
      // FR-59: editor tampil = path masuk ke editor, bukan langsung ke shell
      if (editorShown(pane)) invoke<string[]>("resolve_paths", { paths: payload.paths }).then((p) => insertPaths(pane, p)).catch(() => {});
      else attach(pane, payload.paths);
    });
    const onFocus = () => (seen = true);
    const onBlur = () => (seen = false);
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);
    (async () => {
      try {
        cfg = await call("config.get");
        // ikon gagal dibuat = menu tetap jalan tanpa bulatan; sebabnya ditampilkan supaya tidak hilang diam-diam
        makeDots().catch((e) => say(`Ikon warna gagal dimuat: ${e}`));
        // FR-59: bahan autocomplete
        homePath = (await homeDir().catch(() => "")).replace(/\/$/, "");
        call("agents.detect", {}).then((a: Agent[]) => (installed = a.filter((x) => x.installed).map((x) => x.agent))).catch(() => {});
        invoke<string[]>("shell_history").then((h) => (zhist = h)).catch(() => {});
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
      window.removeEventListener("keydown", keepFullscreen);
      unlistenFull.then((f) => f());
      unlisten.then((f) => f());
      unlistenCfg.then((f) => f());
      unlistenDrop.then((f) => f());
    };
  });
</script>

{#snippet icon(name: string)}
  <!-- ikon garis 12px gaya Warp; currentColor ikut warna chip -->
  <svg class="ic" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round">
    {#if name === "plus"}<path d="M8 3v10M3 8h10" />
    {:else if name === "file"}<path d="M4 1.5h5l3 3v10H4zM9 1.5v3h3" />
    {:else if name === "rich"}<path d="M2 3.5h12v7H2zM5 13.5h6M5 6h4" />
    {:else if name === "folder"}<path d="M1.5 3.5h4l1.5 1.5h7.5v8h-13z" />
    {:else if name === "branch"}<path d="M5 2.5v11M5 9c0-3 6-2 6-5.5M11 2.5v1" /><circle cx="11" cy="4" r="1.3" /><circle cx="5" cy="13" r="1.3" />
    {:else if name === "history"}<path d="M2.5 8a5.5 5.5 0 1 0 1.6-3.9M2.5 2.5v2h2M8 5v3l2 1.5" />
    {:else if name === "fork"}<circle cx="4.5" cy="3.5" r="1.3" /><circle cx="11.5" cy="3.5" r="1.3" /><circle cx="8" cy="12.5" r="1.3" /><path d="M4.5 5c0 3 3.5 3 3.5 6M11.5 5c0 3-3.5 3-3.5 6" />
    {:else if name === "sliders"}<path d="M2 5h7M12 5h2M2 11h2M7 11h7" /><circle cx="10.5" cy="5" r="1.5" /><circle cx="5.5" cy="11" r="1.5" />{/if}
  </svg>
{/snippet}

{#snippet chip(c: string, pane: number, g: any)}
  {#if c === "attach"}
    <button class="chip box" title="Lampirkan file (atau seret file ke pane)" aria-label="Lampirkan file" onclick={async () => attach(pane, await invoke<string[]>("pick_files"), true)}>{@render icon("plus")}</button>
  {:else if c === "diff" && g?.repo && g.files}
    <button class="chip box" title="Panel perubahan ⌘⇧=" onclick={() => openChanges(pane)}>{@render icon("file")}{g.files} • <span class="plus">+{g.add}</span> <span class="minus">−{g.del}</span></button>
  {:else if c === "rich_input"}
    <button class="chip box" onclick={() => (rich = { pane, text: "" })}>{@render icon("rich")}Rich Input <span class="n">⌘G</span></button>
  {:else if c === "cwd"}
    <span class="chip box quiet" title={meta[pane]?.cwd ?? ""}>{@render icon("folder")}{short(meta[pane]?.cwd) || "~"}</span>
  {:else if c === "branch" && g?.repo && g.branch}
    <!-- branch tidak dipotong; yang mengalah folder (TECH 19 #9) -->
    <span class="chip box">{@render icon("branch")}{g.branch}{g.ahead ? ` ↑${g.ahead}` : ""}{g.behind ? ` ↓${g.behind}` : ""}</span>
  {:else if c === "settings"}
    <button class="chip box" title="Pengaturan" aria-label="Pengaturan profil" onclick={() => (settingsOpen = true)}>{@render icon("sliders")}</button>
  {/if}
{/snippet}

{#if sidebar}
  <aside class="side" aria-label="Tab" style:width="{sideW}px" class:dragging={!!dragging} class:full={fullscreen}>
    <!-- title bar transparan: area tombol jendela + geser jendela -->
    <div class="drag" data-tauri-drag-region></div>
    <button class="add find" onclick={openPalette}>Cari tab atau aksi <span class="n">⌘P</span></button>
    <!-- FR-44: di bawah kotak cari, selalu terlihat walau daftar tab panjang -->
    {#if queue.length}
      <!-- FR-65: antrean perhatian; hilang bila tidak ada yang menunggu -->
      <div class="queue" role="region" aria-label="Butuh izin">
        <div class="qhead">Butuh lu <span>{queue.length}</span></div>
        {#each queue as q (q.p)}
          {@const ag = agentOf(q.p) || meta[q.p]?.agent || ""}
          <button class="tab" onclick={() => jumpTo(q.p)}>
            <span class="tt"><span class="dot blocked" aria-hidden="true"></span>{#if GLYPH[ag]}<span class="aico {ag}">{GLYPH[ag]}</span>{/if}{meta[q.p]?.name}{labels[q.p] || GLYPH[ag] ? ` · ${labels[q.p] || ag}` : ""}</span>
            <span class="k">{ago(now - (blockedAt[q.p] ?? now))}</span>
            <span class="sub">{[title(q.t) !== labels[q.p] ? title(q.t) : "", wss.length > 1 ? q.w.name : ""].filter(Boolean).join(" · ")}</span>
          </button>
        {/each}
        <div class="qhint">{pretty("Cmd+Shift+U")} ke pane berikutnya</div>
      </div>
    {/if}
    <button class="add wsnew" onclick={addWorkspace}>+ Workspace</button>
    {#each wss as w, wi (w.id)}
      <!-- FR-44: klik judul = buka/ciutkan; ganti nama lewat klik kanan atau palet -->
      {@const liftWs = dragging?.kind === "ws" && dragging.wi === wi}
      <!-- FR-44: blok workspace berwarna (mockup-warna-tab varian D) -->
      <div class="wsg {w.color ?? ''}">
      <div class="ws" data-wi={wi} data-k="{w.id}:h" class:lift={liftWs} class:target={dragging?.kind === "tab" && dragging.over.wi === wi && dragging.wi !== wi}
        role="button" tabindex="0" aria-expanded={!w.collapsed} title={w.root}
        onclick={() => editing?.id !== w.id && (w.collapsed = !w.collapsed)}
        onkeydown={(e) => e.key === "Enter" && e.target === e.currentTarget && (w.collapsed = !w.collapsed)}
        onpointerdown={(e) => startDrag(e, "ws", wi)} oncontextmenu={(e) => wsMenu(e, wi)}>
        {#if editing?.kind === "ws" && editing.id === w.id}
          <!-- svelte-ignore a11y_autofocus -->
          <input class="edit" bind:value={editing.value} onkeydown={(e) => e.key === "Enter" && saveEdit()} onblur={saveEdit} autofocus aria-label="Nama workspace" />
        {:else}
          <span class="chev" aria-hidden="true">{w.collapsed ? "▸" : "▾"}</span>
          <span class="wsname" title={w.name}>{w.name}</span>
          {#if w.collapsed}
            <!-- FR-44: diciutkan tetap menampilkan jumlah tab dan status terberat -->
            {@const st = worst(w.tabs.flatMap((t) => panes(t.tree)))}
            <span class="wsum">{#if STATUS[st]}<span class="dot {st}" aria-hidden="true"></span><span class="st {st}">{STATUS[st]}</span> · {/if}{w.tabs.length} tab</span>
          {/if}
          <!-- FR-52: tab baru di workspace INI; klik kanan/Option = pilih profil -->
          <button class="wsadd" title="Tab baru di {w.name} · klik kanan: pilih profil" aria-label="Tab baru di workspace {w.name}"
            onpointerdown={(e) => e.stopPropagation()}
            onclick={(e) => (e.stopPropagation(), e.altKey ? profileMenu(e, wi) : newTab(undefined, wi))}
            oncontextmenu={(e) => profileMenu(e, wi)}>+</button>
        {/if}
      </div>
      {#if !w.collapsed && !w.tabs.length}
        <button class="add empty-ws" data-wi={wi} data-k="{w.id}:e" class:lift={liftWs}
          onclick={() => newTab(undefined, wi)}>+ Tab</button>
      {/if}
      {#each w.collapsed ? [] : w.tabs as t, ti (t.id)}
        {@const n = order.findIndex((o) => o.t === t)}
        {@const agents = panes(t.tree).map((p) => meta[p]?.busy).filter(Boolean)}
        {@const st = tabStatus(t)}
        <button class="tab {t.color ?? ''}" class:on={at.ws === wi && at.tab === ti} class:blocked={st === "blocked"} data-wi={wi} data-ti={ti}
          class:lift={(dragging?.kind === "tab" && dragging.wi === wi && dragging.ti === ti) || (dragging?.kind === "ws" && dragging.wi === wi)}
          data-k={t.id}
          onpointerdown={(e) => startDrag(e, "tab", wi, ti)} oncontextmenu={(e) => tabMenu(e, wi, t)}
          onclick={() => ((at = { ws: wi, tab: ti }), (zoomed = false))}
          ondblclick={() => (editing = { kind: "tab", id: t.id, value: t.title || title(t) })}>
          {#if editing?.kind === "tab" && editing.id === t.id}
            <!-- svelte-ignore a11y_autofocus -->
            <input class="edit" bind:value={editing.value} onkeydown={(e) => e.key === "Enter" && saveEdit()} onblur={saveEdit} autofocus aria-label="Nama tab" />
          {:else}
            {@const ag = panes(t.tree).map(agentOf).find(Boolean) ?? ""}
            <span class="tt">{#if STATUS[st]}<span class="dot {st}" aria-hidden="true"></span>{/if}{#if ag}<span class="aico {ag}" title={ag}>{GLYPH[ag]}</span>{/if}{ag && meta[t.focus]?.name ? `${meta[t.focus].name} · ` : ""}{title(t)}</span>
            <span class="n"><span class="k">{n < 9 ? `⌘${n + 1}` : ""}</span><span class="x" role="button" tabindex="-1" aria-label="Tutup tab {title(t)}"
                onclick={(e) => (e.stopPropagation(), closeTabs(wi, [t]))} onkeydown={(e) => e.key === "Enter" && closeTabs(wi, [t])}>×</span></span>
            {@const g = meta[t.focus]?.git}
            <!-- FR-61: akun selain Utama tampil di keterangan tab -->
            <!-- agent sudah tampil sebagai ikon; proses lain (npm, vim) tetap disebut -->
            {@const idle = meta[t.focus]?.agent === "shell" && !meta[t.focus]?.busy ? meta[t.focus]?.shell : ""}
            {@const rest = [g?.repo ? g.branch : "", where(meta[t.focus]?.cwd, w.root), ...agents.filter((a) => !AGENTS.includes(a!)), acctOf(t.focus), idle].filter((x) => x && x !== title(t)).join(" · ")}
            {#if STATUS[st] || rest}
              <span class="sub" title={home(meta[t.focus]?.cwd)}>{#if panes(t.tree).length > 1}<!-- satu titik per pane: pane selesai tidak menutupi pane lain yang masih bekerja -->{#each panes(t.tree) as p (p)}<span class="dot pdot {stat(p) || 'idle'}" title="{meta[p]?.name} {STATUS[stat(p)] ?? 'diam'}"></span>{/each}{:else if STATUS[st]}<span class="st {st}">{STATUS[st]}</span>{rest ? " · " : ""}{/if}{rest}</span>
            {/if}
          {/if}
        </button>
      {/each}
      </div>
    {/each}
    {#if dragging}<div class="dropline" style:top="{dragging.line.top}px" style:left="{dragging.line.left}px" style:width="{dragging.line.width}px"></div>{/if}
  </aside>
  <div class="sidegrip" style:left="{sideW - 3}px" onpointerdown={dragSide} role="separator" aria-orientation="vertical" aria-label="Lebar sidebar"></div>
{/if}

{#if !sidebar}<div class="drag top" data-tauri-drag-region></div>{/if}

<div class="grid" style:left={sidebar ? `${sideW}px` : "0"} style:top={sidebar ? "0" : "28px"} style:right={changes ? `${panelW}px` : "0"} bind:this={box}>
  {#if ws && !tab}
    <div class="empty">
      <span>Workspace {ws.name} kosong</span>
      <button class="mini" onclick={openPicker}>+ Tab <span class="keys">⌘T</span></button>
    </div>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
  {#each wss as w, wi (w.id)}
    {#each w.tabs as t, ti (t.id)}
      {@const active = at.ws === wi && at.tab === ti}
      {#each [...rects(t.tree)] as [pane, r] (pane)}
        {@const hidden = !active || (zoomed && pane !== t.focus)}
        {@const rr = zoomed ? { x: 0, y: 0, w: 1, h: 1 } : r}
        <!-- FR-55: toolbar untuk pane agent dari profil, dan agent yang diketik sendiri selama jalan -->
        {@const bar = !dead[pane] && ([...AGENTS, "custom"].includes(meta[pane]?.agent ?? "") || AGENTS.includes(meta[pane]?.busy ?? ""))}
        {@const split = panes(t.tree).length > 1}
        {@const head = !zoomed}
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
            {#if head}
              <!-- FR-37: kepala tipis, juga saat tab cuma satu pane (Gembala.pen 02) -->
              {@const hs = stat(pane)}
              <div class="head" class:on={split && pane === t.focus}>
                <span class="hname">{#if STATUS[hs]}<span class="dot {hs}" aria-hidden="true"></span>{/if}{meta[pane]?.name}{labels[pane] || meta[pane]?.busy ? ` · ${labels[pane] || meta[pane]?.busy}` : ""}</span>
                {#if STATUS[hs]}<span class="st {hs}">{STATUS[hs]}</span>{/if}
                <!-- folder cukup sekali: toolbar agent sudah menampilkannya -->
                {#if !(bar && [...toolbar.left, ...toolbar.right].includes("cwd"))}<span class="hcwd" title={home(meta[pane]?.cwd)}>{short(meta[pane]?.cwd)}</span>{/if}
                <span class="grow"></span>
                <button class="hb" title="Split ke kanan ⌘D" aria-label="Split ke kanan" onclick={() => splitFocused("row")}>◫</button>
                <button class="hb" title="Split ke bawah ⌘⇧D" aria-label="Split ke bawah" onclick={() => splitFocused("col")}>⊟</button>
                <button class="hb" title="Tutup pane ⌘W" aria-label="Tutup pane" onclick={() => askClose(pane)}>×</button>
              </div>
            {/if}
            {@const ed = editorShown(pane)}
            <div class="body" class:withbar={bar} class:withhead={head} class:withedit={ed} oncontextmenu={(e) => paneMenu(e, pane)} role="presentation">
              <Tile bind:this={tiles[pane]} {pane} focused={active && pane === t.focus && lempar?.pane !== pane && !ed} {ui} zoom={zoom + (meta[pane]?.zoom ?? 0)} {onexit} {onlink}
                onpasted={(path: string) => meta[pane] && (meta[pane].pastes = [...(meta[pane].pastes ?? []), path])}
                online={(line: string) => onLine(pane, line)} onkey={(key: string) => onMentionKey(pane, key)}
                onprompt={(on: boolean) => ((atPrompt[pane] = on), on || (ran[pane] = true))}
                onused={() => (ran[pane] = true)}
                onblock={(kind: string, text: string) => {
                  if (kind === "output") return void (outputOf = pane);
                  // sesi terakhir pane agent dari profil: jalankan ulang pane-nya (hook resmi), bukan diketik ke shell
                  if (kind === "run" && meta[pane]?.ended?.resume === text && AGENTS.includes(meta[pane].agent)) return void resumeEnded(pane);
                  // FR-56 linimasa: Lanjutkan/Ulangi = jalankan perintah di pane ini (lewat editor supaya tercatat di riwayat)
                  if (kind === "run" && editorShown(pane)) return void ((draft[pane] = text), runDraft(pane));
                  if (kind !== "reuse") return void copy(text);
                  if (!editorShown(pane)) return void copy(text); // editor tidak tampil (mis. TUI jalan): salin saja
                  draft[pane] = text;
                  document.querySelector<HTMLTextAreaElement>(`[data-editor="${pane}"]`)?.focus();
                }}
                onfocusin={() => editorShown(pane) && setTimeout(() => document.querySelector<HTMLTextAreaElement>(`[data-editor="${pane}"]`)?.focus(), 0)}
                ontype={(text: string) => {
                  if (!editorShown(pane)) return false;
                  draft[pane] = (draft[pane] ?? "") + text;
                  onEditorInput(pane);
                  document.querySelector<HTMLTextAreaElement>(`[data-editor="${pane}"]`)?.focus();
                  return true;
                }} />
            </div>
            {#if ed && !ran[pane] && !draft[pane]}
              <!-- FR-59: panel Mulai: hilang begitu ada perintah jalan atau mulai mengetik -->
              <div class="start" class:withhead={head}>
                <span class="m">Mulai di {home(meta[pane]?.cwd) || "~"}</span>
                <div class="starts">
                  {#each startProfiles as p (p.key)}
                    <button class="sbtn" onclick={() => swapToProfile(pane, p.id, p.account)}><span class="aico {p.agent}">{GLYPH[p.agent] ?? "›"}</span>{p.label}</button>
                  {/each}
                  {#if pickable.length}
                    <button class="sbtn" onclick={(e) => popup(e, pickable.map((p) => [p.label, () => swapToProfile(pane, p.id, p.account, true)] as MItem))}
                      >{@render icon("history")} Sesi lama…</button
                    >
                  {/if}
                  <button class="sbtn" onclick={() => startInFolder(pane)}>{@render icon("folder")} Buka folder…</button>
                </div>
              </div>
            {/if}
            {#if ed}
              <!-- FR-59: input editor; kursor dan seleksi pakai mouse, Enter jalan, Tab/Esc ke terminal -->
              <div class="editor" class:withbar={bar}>
                <!-- konteks ala Warp: folder dan branch, menggantikan prompt shell -->
                <div class="ctx">
                  <button class="cchip" title="Lampirkan file · bisa juga seret file atau ⌘V gambar" aria-label="Lampirkan file"
                    onclick={async () => insertPaths(pane, await invoke<string[]>("pick_files").catch(() => [] as string[]))}>{@render icon("plus")}</button>
                  <!-- FR-59: klik = pemilih folder ala Warp -->
                  <button class="cchip dir" title="Pindah folder · {meta[pane]?.cwd ?? ''}" onclick={() => openDirs(pane)}>
                    <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M1.5 3.5h4l1.5 1.5h7.5v8h-13z" fill="none" stroke="currentColor" /></svg>
                    {short(meta[pane]?.cwd) || "~"}
                  </button>
                  {#if meta[pane]?.git?.repo && meta[pane].git.branch}
                    <span class="cchip">⎇ {meta[pane].git.branch}{meta[pane].git.files ? ` · ${meta[pane].git.files} file` : ""}</span>
                  {/if}
                  <span class="ehint" class:off={!active || pane !== t.focus}>Enter jalankan · Shift+Enter baris baru · Tab lengkapi · Esc ke terminal · @agent pesan</span>
                </div>
                {#if dirs?.pane === pane}
                  <div class="mention complist acct dirs" role="listbox" aria-label="Pilih folder">
                    <!-- svelte-ignore a11y_autofocus -->
                    <input class="dq" placeholder="Cari folder…" bind:value={dirs.q} autofocus spellcheck="false" autocapitalize="off"
                      onkeydown={dirsKey} oninput={dirsInput} onblur={() => (dirs = null)} />
                    {#each dirItems as n, i (n)}
                      <button class="opt" class:on={i === dirs.sel} role="option" aria-selected={i === dirs.sel}
                        onpointerdown={(e) => e.preventDefault()} onclick={() => cdTo(n)}>
                        <span class="ico" aria-hidden="true">{#if n === ".."}↑{:else}{@render icon("folder")}{/if}</span>
                        <span>{n === ".." ? ".. (Folder induk)" : n}</span><span></span>
                      </button>
                    {/each}
                    {#if !dirItems.length}<span class="keys">Tidak ada folder yang cocok</span>{/if}
                  </div>
                {/if}
                {#if comp?.pane === pane}
                  <!-- FR-59: daftar Tab ala Warp -->
                  <div class="mention complist" class:acct={comp.auto} role="listbox" aria-label="Pilihan lengkapi">
                    {#each comp.items as it, i (it.label)}
                      <button class="opt" class:on={i === comp.sel} role="option" aria-selected={i === comp.sel}
                        onpointerdown={(e) => e.preventDefault()} onclick={() => ((draft[pane] = it.insert), (comp = null))}>
                        {#if comp.auto}
                          <!-- FR-61 varian C (mockup-pilih-akun): nama di atas, email/perintah kecil di bawah -->
                          <span class="ico claude" aria-hidden="true">{GLYPH.claude}</span>
                          <span class="two"><span>{it.label}</span><small>{it.kind}</small></span>
                          <span class="keys">{i === comp.sel ? "Tab" : ""}</span>
                        {:else}
                          <span class="ico" aria-hidden="true">{({ Folder: "▸", Perintah: "›", Branch: "⎇", Script: "▶" } as Record<string, string>)[it.kind] ?? "·"}</span>
                          <span>{it.label}</span>
                          <span class="keys">{it.kind}</span>
                        {/if}
                      </button>
                    {/each}
                  </div>
                {/if}
                <!-- kotak input yang jelas: bingkai + ❯, bingkai biru saat fokus -->
                <div class="ebox">
                <span class="ps" aria-hidden="true">❯</span>
                <div class="ghostwrap">
                  <!-- saran abu-abu dari riwayat; → terima -->
                  <div class="ghost" aria-hidden="true"><span class="typed">{draft[pane] ?? ""}</span>{ghost(pane)}</div>
                <textarea bind:value={draft[pane]} rows={Math.min(6, (draft[pane] ?? "").split("\n").length)} data-editor={pane}
                  aria-label="Perintah untuk {meta[pane]?.name}" spellcheck="false" autocapitalize="off" autocomplete="off"
                  placeholder="Ketik perintah…"
                  onkeydown={(e) => editorKey(pane, e)} oninput={() => onEditorInput(pane)} onfocus={() => (t.focus = pane)}
                  onpaste={(e) => pasteToEditor(pane, e)}
                  oncopy={(e) => {
                    // ⌘C tanpa seleksi di editor = salin seleksi terminal (fokus selalu di editor saat editor tampil)
                    const ta = e.currentTarget;
                    const sel = tiles[pane]?.selection();
                    if (ta.selectionStart === ta.selectionEnd && sel) (e.preventDefault(), e.clipboardData?.setData("text/plain", sel));
                  }}></textarea>
                </div>
                </div>
                {#if editorErr[pane]}<span class="err">{editorErr[pane]}</span>{/if}
              </div>
            {/if}
            {#if mention?.pane === pane}
              <!-- FR-57: daftar @; Tab pilih, ↑↓ geser, Esc tutup, terus mengetik = @ milik agent -->
              <div class="mention" class:withbar={bar} class:withedit={mention.editor} role="listbox" aria-label="Lempar ke agent">
                {#if !mentionItems.length}
                  <!-- kosong tetap tampil: tanpa ini @@ terlihat rusak -->
                  <span class="keys">{targets.length ? `Tidak ada agent yang cocok dengan "${mention.q}"` : "Belum ada agent lain di workspace ini · jalankan agent di tab atau split lain"}</span>
                {/if}
                {#each mentionItems as r, i (r.pane)}
                  <button class="opt" class:on={i === mention.sel} role="option" aria-selected={i === mention.sel}
                    onpointerdown={(e) => e.preventDefault()} onclick={() => chooseMention(i)}>
                    <span class="dot {r.status}"></span>
                    <span>{r.name}</span>
                    <span class="keys">{r.agent === "shell" ? (meta[r.pane]?.busy ?? "shell") : r.agent} · {STATUS[r.status ?? ""] ?? r.status ?? "?"}</span>
                  </button>
                {/each}
                {#if mentionItems.length}<span class="keys"
                    >{mention.picked ? "Tab/Enter tambah agent · ↑↓ pilih · Esc selesai" : `Tab/Enter ${mention.inline ? "lengkapi nama" : "pilih"} · ↑↓ pilih · Esc tutup`}</span
                  >{/if}
              </div>
            {/if}
            {#if lempar?.pane === pane}
              <div class="lempar" class:withbar={bar}>
                <span class="to">→ {lempar.name}</span>
                <!-- svelte-ignore a11y_autofocus -->
                <textarea bind:value={lempar.body} rows="2" autofocus aria-label="Pesan ke {lempar.name}" placeholder="Pesan · Enter kirim · Shift+Enter baris baru · Esc batal"
                  spellcheck="false" autocapitalize="off"
                  onkeydown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) (e.preventDefault(), sendLempar());
                    if (e.key === "Escape") (e.preventDefault(), (lempar = null), tiles[pane]?.focus());
                  }}></textarea>
                {#if lempar.error}
                  <span class="err">{lempar.error}</span>
                  {#if lempar.error.includes("paksa")}<button class="mini" onclick={() => sendLempar(true)}>Kirim paksa</button>{/if}
                {/if}
              </div>
            {/if}
            {#if meta[pane]?.login}
              <!-- FR-61: Gembala tidak mengetik apa pun; login dilakukan pengguna di Claude Code pane ini -->
              <div class="ended" class:withhead={head} role="status">
                <span class="etitle">Masuk ke akun {meta[pane].login}: ikuti langkah login di pane ini, atau ketik <code>/login</code></span>
                <span class="grow"></span>
                <button class="hb" aria-label="Tutup" onclick={() => delete meta[pane].login}>×</button>
              </div>
            {/if}
            <!-- FR-56: tombol sesi agent selesai ada di blok linimasa (pen layar 02), tanpa banner di atas pane -->
            {#if bar}
              <!-- FR-55: toolbar agent; chip tanpa data (bukan repo) disembunyikan -->
              <!-- pane tak fokus di tab ber-split: isi toolbar disembunyikan (tinggi tetap, terminal tidak reflow), tampil saat hover -->
              <div class="bar" class:rest={split && !zoomed && pane !== t.focus}>
                {#if editing?.kind === "pane" && editing.id === String(pane)}
                  <!-- svelte-ignore a11y_autofocus -->
                  <input class="edit pname" bind:value={editing.value} onkeydown={(e) => e.key === "Enter" && saveEdit()} onblur={saveEdit} autofocus aria-label="Nama pane" />
                {:else}
                  <button class="chip pname" title="Klik dua kali untuk ganti nama" ondblclick={() => (editing = { kind: "pane", id: String(pane), value: meta[pane]?.name ?? "" })}>{#if agentOf(pane)}<span class="aico {agentOf(pane)}">{GLYPH[agentOf(pane)]}</span>{/if}{meta[pane]?.name}</button>
                {/if}
                {#if acctOf(pane)}<span class="chip acc" title="Akun Claude Code (FR-61)">{acctOf(pane)}</span>{/if}
                {#if meta[pane]?.owner && agentOf(pane) === "claude"}<span class="chip rwarn" title="Sesi ini dibuat di akun {meta[pane].owner.name}. Di akun lain Claude membuang thinking lama: lebih lambat dan boros token. Keluar, lalu Lanjutkan di blok sesi = dibuka lagi di akun {meta[pane].owner.name} (FR-61)">sesi dari {meta[pane].owner.name}</span>{/if}
                {#if meta[pane]?.roleWarn}<span class="chip rwarn" title="Peran baca saja hanya lewat petunjuk; agent ini tidak punya pembatas tool (FR-13)">edit tidak diblokir</span>{/if}
                {#each toolbar.left as c}{@render chip(c, pane, g)}{/each}
                {#if meta[pane]?.output}<button class="chip" onclick={() => (outputOf = pane)}>Output sesi</button>{/if}
                {#if meta[pane]?.prev}<button class="chip" onclick={() => resumePrev(pane)}>Lanjutkan sesi {meta[pane].prev.agent}</button>{/if}
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
                  spellcheck="false"
                  autocapitalize="off"
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
              <span><b>{confirm?.name}</b> masih jalan. Tutup {confirm?.what}?</span>
              <span class="acts">
                <button class="mini" onclick={() => (confirm = null)}>Batal <span class="keys">Esc</span></button>
                <button class="mini danger" onclick={() => { confirm?.close.forEach((p) => call("pane.close", { pane: p }).catch(() => {})); confirm = null; }}>Tutup <span class="keys">Enter</span></button>
              </span>
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
      ondblclick={() => tab && (tab.tree = setRatio(tab.tree, d.path, 0.5))}
      title="Seret untuk ubah ukuran · klik dua kali = sama rata"
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
      <div class="targets" role="listbox" aria-label="Pane tujuan">
        {#each targets as r (r.pane)}
          <button class="opt" class:on={compose.to === r.pane} role="option" aria-selected={compose.to === r.pane} onclick={() => compose && (compose.to = r.pane)}>
            <span class="dot {r.status}"></span>
            <span>{r.name}</span>
            <span class="keys">{r.agent === "shell" ? (meta[r.pane]?.busy ?? "shell") : r.agent} · {STATUS[r.status ?? ""] ?? r.status ?? "?"}</span>
          </button>
        {:else}
          <span class="keys">Belum ada agent lain di workspace ini</span>
        {/each}
      </div>
      <!-- svelte-ignore a11y_autofocus -->
      <textarea bind:value={compose.body} rows="5" autofocus aria-label="Isi pesan" placeholder="Isi pesan; dikirim dengan label dari pengguna" spellcheck="false" autocapitalize="off"
        onkeydown={(e) => e.key === "Enter" && e.metaKey && sendCompose()}></textarea>
      <label class="inline"><input type="checkbox" bind:checked={compose.force} /> Paksa bila status target tidak diketahui</label>
      {#if compose.error}<span class="err">{compose.error}</span>{/if}
      <span class="acts"><button class="mini" onclick={() => (compose = null)}>Batal</button> <button class="mini" onclick={sendCompose}>Kirim ⌘↩</button></span>
    </div>
  {/if}
  {#if changes}
    <Changes status={meta[changes.pane]?.git} diff={changes.diff} width={panelW} onwidth={(w: number) => (panelW = w)}
      pname={meta[changes.pane]?.name ?? ""} turns={changes.turns ?? []} turn={changes.turn ?? null} ask={changes.ask ?? null}
      onturn={pickTurn} onrestore={askRestore} onconfirm={doRestore} oncancel={() => changes && (changes.ask = null)}
      onreload={() => changes && loadChanges(changes.pane)} onclose={() => (changes = null)} />
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
      <span class="acts">
        <button class="mini" onclick={() => (wsAsk = null)}>Batal <span class="keys">Esc</span></button>
        <button class="mini danger" onclick={() => wsAsk && removeWorkspace(wsAsk.wi)}>Hapus <span class="keys">Enter</span></button>
      </span>
    </div>
  {/if}
  {#if welcome}
    <div class="confirm welcome" role="dialog" aria-label="Selamat datang">
      <b>Siap dipakai</b>
      {#each welcome as a (a.agent)}
        <span>{a.installed ? "✓" : "·"} {a.name} <span class="keys">{a.installed ? a.version : "tidak terpasang"}</span></span>
      {/each}
      <span class="keys">⌘P cari tab dan aksi · ⌘T tab baru dari profil · ⌘, Pengaturan</span>
      <span class="keys">Ketik perintah di kotak bawah · @@ di prompt agent = lempar ke agent lain</span>
      <span class="acts"><button class="mini" onclick={() => (welcome = null)}>Mulai <span class="keys">Enter</span></button></span>
    </div>
  {/if}
  {#if quitAsk}
    <div class="confirm" role="alertdialog" aria-label="Konfirmasi keluar">
      <span><b>{quitAsk} pane</b> masih menjalankan proses. Keluar dan hentikan semua?</span>
      <span class="acts">
        <button class="mini" onclick={() => (quitAsk = 0)}>Batal <span class="keys">Esc</span></button>
        <button class="mini danger" onclick={quitAll}>Keluar <span class="keys">Enter</span></button>
      </span>
    </div>
  {/if}
  {#if linkAsk}
    <div class="confirm" role="alertdialog" aria-label="Konfirmasi buka tautan">
      <span>Buka di browser?</span>
      <code class="url">{linkAsk}</code>
      <span class="acts">
        <button class="mini" onclick={() => (linkAsk = "")}>Batal <span class="keys">Esc</span></button>
        <button class="mini" onclick={openLink}>Buka <span class="keys">Enter</span></button>
      </span>
    </div>
  {/if}
  {#if tpl}
    <!-- FR-13: dialog template peran (pen layar 17) -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div class="confirm tpldlg" role="dialog" aria-label="Dari template peran" tabindex="-1" onkeydown={(e) => e.key === "Escape" && (tpl = null)}>
      <span class="thd"><b>Dari template peran</b><button class="mini" onclick={() => (tpl = null)} aria-label="Tutup">×</button></span>
      <div class="tpls">
        {#each TEMPLATES as t, i (t.name)}
          <button class="tplc" class:on={tpl.t === i} onclick={() => pickTemplate(i)}><span>{t.name}</span><span class="m">{t.desc}</span></button>
        {/each}
      </div>
      <div class="roles">
        <div class="rrow rh"><span>Peran</span><span>Agent</span><span>Batas</span><span>Nama pane</span></div>
        {#each tpl.rows as r, i (i)}
          {@const lim = limitOf(r.role, r.prof)}
          <div class="rrow">
            <span>{r.label}</span>
            <select class="edit" bind:value={r.prof}>
              {#each startProfiles as p, j (p.key)}<option value={j}>{GLYPH[p.agent] ?? "›"} {p.label}</option>{/each}
            </select>
            <span class="lim {lim}">{lim === "lock" ? "cuma bisa baca, dikunci lewat --disallowedTools" : lim === "warn" ? "cuma dikasih tahu baca saja, edit tidak bisa diblokir" : "boleh ubah file"}</span>
            <input class="edit" bind:value={r.name} spellcheck="false" autocapitalize="off" />
          </div>
        {/each}
      </div>
      <div class="brief">
        <b>Brief workspace {wss[tpl.wi]?.name}</b>
        <span class="m">Ikut dikirim ke tiap agent di workspace ini · agent cuma bisa baca · disimpan di Gembala, bukan di repo</span>
        <textarea class="edit" bind:value={tpl.brief} rows="5" maxlength="4000" spellcheck="false" placeholder="Tujuan, cara build dan test, batasan"></textarea>
      </div>
      <span class="acts">
        <span class="m">{tpl.rows.length} pane baru di tab baru · namanya bisa diganti</span>
        <button class="mini" onclick={() => (tpl = null)}>Batal</button>
        <button class="mini pri" onclick={runTemplate}>Jalankan {tpl.rows.length} pane</button>
      </span>
    </div>
  {/if}
  {#if wt}
    <!-- FR-71: dialog worktree (pen layar 07) -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div class="confirm wtdlg" role="dialog" aria-label="Agent di worktree baru" tabindex="-1"
      onkeydown={(e) => (e.key === "Escape" ? (wt = null) : e.key === "Enter" && !e.isComposing && (e.preventDefault(), createWorktree()))}>
      <b>Agent di worktree baru</b>
      <span class="m">Diambil dari HEAD {home(wt.root)}. Perubahan yang belum di-commit tidak ikut.</span>
      <label>Nama <input class="edit" bind:value={wt.name} spellcheck="false" autocapitalize="off"
        oninput={() => wt && !wt.dirSet && (wt.dir = `${wt.root}.${wt.name.trim()}`)} /></label>
      <label>Folder <input class="edit" bind:value={wt.dir} spellcheck="false" autocapitalize="off" oninput={() => wt && (wt.dirSet = true)} /></label>
      <label>Branch baru <input class="edit" value={`gembala/${wt.name.trim()}`} disabled /></label>
      <label>Profil
        <select class="edit" bind:value={wt.prof}>
          {#each startProfiles as p, i (p.key)}<option value={i}>{p.label}</option>{/each}
        </select>
      </label>
      <span class="m note">Menutup pane tidak menghapus worktree. Gabungkan hasilnya lewat git biasa; Gembala tidak merge atau push.</span>
      {#if wt.err}<span class="err">{wt.err}</span>{/if}
      <span class="acts">
        <button class="mini" onclick={() => (wt = null)}>Batal <span class="keys">Esc</span></button>
        <button class="mini pri" onclick={createWorktree} disabled={!startProfiles.length}>Buat dan jalankan <span class="keys">Enter</span></button>
      </span>
    </div>
  {/if}
  {#if wtDel}
    <div class="confirm wtdlg" role="alertdialog" aria-label="Hapus worktree">
      <b>Worktree {home(wtDel.dir)} punya perubahan yang belum di-commit</b>
      <span class="m">File ini akan hilang kalau dihapus:</span>
      <pre class="files">{wtDel.files.slice(0, 30).join("\n")}{wtDel.files.length > 30 ? `\n… ${wtDel.files.length - 30} lagi` : ""}</pre>
      <span class="acts">
        <button class="mini" onclick={() => (wtDel = null)}>Batal</button>
        <button class="mini danger" onclick={() => ((wtDel = null), wtForce())}>Hapus beserta perubahannya</button>
      </span>
    </div>
  {/if}
  {#if pal}
    <!-- FR-39: palet perintah -->
    <div class="confirm picker palette" role="dialog" aria-label="Palet perintah">
      <!-- svelte-ignore a11y_autofocus -->
      <input class="edit" bind:value={pal.q} autofocus placeholder="Cari tab atau aksi" aria-label="Cari tab atau aksi" spellcheck="false" autocapitalize="off"
        oninput={() => pal && (pal.sel = 0)}
        onkeydown={(e) => {
          const n = palItems.length;
          if (e.key === "ArrowDown" && n) (e.preventDefault(), pal && (pal.sel = (pal.sel + 1) % n));
          if (e.key === "ArrowUp" && n) (e.preventDefault(), pal && (pal.sel = (pal.sel + n - 1) % n));
          if (e.key === "Enter") (e.preventDefault(), palRun(pal?.sel ?? 0));
          if (e.key === "Escape") (e.preventDefault(), (pal = null));
        }} onblur={() => setTimeout(() => (pal = null), 150)} />
      <div class="pallist" role="listbox" aria-label="Hasil">
        {#each palItems as it, i (i)}
          <button class="opt" class:on={i === pal.sel} role="option" aria-selected={i === pal.sel} onpointerdown={(e) => e.preventDefault()} onclick={() => palRun(i)}>
            <span></span><span>{it.label}</span><span class="keys">{it.hint}</span>
          </button>
        {:else}
          <span class="keys">Tidak ada yang cocok</span>
        {/each}
      </div>
    </div>
  {/if}
  {#if picker}
    <div class="confirm picker" role="listbox" aria-label="Pane baru dari profil">
      {#each picker.items as p, i (p.key)}
        <button class="opt" class:on={i === picker.sel} role="option" aria-selected={i === picker.sel} onclick={() => pick(i)}>
          <span class="num">{i < 9 ? i + 1 : ""}</span>
          <span>{p.label}</span>
          <span class="keys">{p.missing ? "tidak terpasang" : p.command || "$SHELL"}</span>
        </button>
      {/each}
      <span class="keys">↑↓ pilih · Enter buka · Esc batal · folder ikut pane yang fokus</span>
    </div>
  {/if}
  {#if settingsOpen && cfg}
    <Settings {cfg} {meta} onclose={() => (settingsOpen = false)} onquitall={askQuitAll} onlogin={loginAccount} />
  {/if}
  {#if zoomed}<span class="zoom">maksimal · ⌘⇧↩</span>{/if}
  <div class="toast" class:show={!!toast} role="status" aria-live="polite">{toast}</div>
</div>

<style>
  /* FR-34: huruf ikon cadangan (Nerd Fonts v3.4.0, MIT); dimuat hanya saat ada glyph PUA yang tidak ada di huruf utama */
  @font-face {
    font-family: "Symbols Nerd Font Mono";
    src: url("/fonts/SymbolsNerdFontMono-Regular.woff2") format("woff2");
    font-display: block;
  }
  /* Hack = font bawaan Warp (MIT + Bitstream Vera, static/fonts/Hack-LICENSE.md) */
  @font-face {
    font-family: "Hack";
    src: url("/fonts/hack-regular.woff2") format("woff2");
    font-display: block;
  }
  @font-face {
    font-family: "Hack";
    src: url("/fonts/hack-bold.woff2") format("woff2");
    font-weight: bold;
    font-display: block;
  }
  @font-face {
    font-family: "Hack";
    src: url("/fonts/hack-italic.woff2") format("woff2");
    font-style: italic;
    font-display: block;
  }
  @font-face {
    font-family: "Hack";
    src: url("/fonts/hack-bolditalic.woff2") format("woff2");
    font-weight: bold;
    font-style: italic;
    font-display: block;
  }
  /* fokus keyboard selalu terlihat; input yang punya penanda sendiri (outline: none berkelas) tidak terpengaruh */
  :global(:focus-visible) {
    outline: 1px solid #3b82f6;
    outline-offset: 1px;
  }
  /* umpan balik tekan: tombol mana pun meredup sesaat saat diklik (TECH 19 Responsif) */
  :global(button:active:not(:disabled)) {
    opacity: 0.7;
  }
  :global(:root) {
    --bg: #000000;
    --fg: #e5e5e5;
    --muted: #8b8b8b;
    --line: #262626;
    --card: #111111;
    --side: #111111;
    --dim: rgba(0, 0, 0, 0.25); /* teks redup pane tak fokus tetap terbaca; garis biru kepala pane sudah menandai fokus */
  }
  /* FR-34 tema terang = palet Latte (TECH, Gembala.pen T01–T38): bukan putih polos, sidebar lebih gelap dari pane,
     teks >= 4,5:1. --tx-* / --pri-* hanya diisi di tema terang; tema gelap memakai warna cadangan di var() */
  @media (prefers-color-scheme: light) {
    :global(:root:not([data-theme="dark"])) {
      --bg: #eff1f5;
      --fg: #4c4f69;
      --muted: #5c5f77;
      --line: #d5d9e2;
      --card: #f7f8fb;
      --side: #dce0e8;
      --dim: rgba(239, 241, 245, 0.25);
      --tx-red: #cf222e;
      --tx-green: #1a7f37;
      --tx-amber: #9a6700;
      --tx-blue: #0969da;
      --tx-purple: #8250df;
      --tx-codex: #0e8a6b;
      --tx-claude: #a8502f;
      --pri-bg: #0969da;
      --pri-line: #0969da;
      --pri-fg: #ffffff;
      --ws-mix: #000000;
      --ws-base: transparent;
    }
  }
  :global(:root[data-theme="light"]) {
    --bg: #eff1f5;
    --fg: #4c4f69;
    --muted: #5c5f77;
    --line: #d5d9e2;
    --card: #f7f8fb;
    --side: #dce0e8;
    --dim: rgba(239, 241, 245, 0.25);
    --tx-red: #cf222e;
    --tx-green: #1a7f37;
    --tx-amber: #9a6700;
    --tx-blue: #0969da;
    --tx-purple: #8250df;
    --tx-codex: #0e8a6b;
    --tx-claude: #a8502f;
    --pri-bg: #0969da;
    --pri-line: #0969da;
    --pri-fg: #ffffff;
    --ws-mix: #000000;
    --ws-base: transparent;
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
    inset: 0;
  }
  .empty {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    color: var(--muted);
    font: 13px system-ui, -apple-system, sans-serif;
  }
  .sidegrip {
    position: fixed;
    top: 0;
    bottom: 0;
    width: 7px;
    z-index: 4;
    cursor: col-resize;
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
    padding: 40px 8px 10px; /* atas: tombol jendela macOS (title bar transparan) */
    border-right: 1px solid var(--line);
    background: var(--side); /* gelap: sedikit lebih terang dari terminal (ala Warp); terang: lebih gelap dari pane */
    overflow-y: auto;
    font: 13px system-ui, -apple-system, sans-serif;
    color: var(--fg);
  }
  .drag {
    position: absolute;
    inset: 0 0 auto 0;
    height: 36px;
  }
  .side.full {
    padding-top: 10px;
  }
  .drag.top {
    position: fixed;
    height: 28px;
    z-index: 5;
  }
  /* polish ala Warp: huruf halus, scrollbar tipis, hover lembut */
  :global(body) {
    -webkit-font-smoothing: antialiased;
  }
  /* terminal: ketebalan huruf seperti Warp; antialiased membuat teks redup/tipis di layar gelap */
  :global(.xterm) {
    -webkit-font-smoothing: auto;
  }
  :global(::-webkit-scrollbar) {
    width: 8px;
    height: 8px;
  }
  :global(::-webkit-scrollbar-track) {
    background: transparent;
  }
  :global(::-webkit-scrollbar-thumb) {
    border: 2px solid transparent;
    border-radius: 4px;
    background: var(--line) padding-box;
  }
  :global(::-webkit-scrollbar-thumb:hover) {
    background: var(--muted) padding-box;
  }
  .tab,
  .add,
  .opt,
  .chip,
  .hb,
  .ws {
    transition: background-color 120ms, color 120ms;
  }
  .tab:hover:not(.on) {
    background: color-mix(in srgb, var(--line) 55%, transparent);
  }
  @media (prefers-reduced-motion: reduce) {
    .tab,
    .add,
    .opt,
    .chip,
    .hb,
    .ws {
      transition: none;
    }
  }
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
  .ws {
    display: flex;
    align-items: baseline;
    gap: 6px;
    margin-top: 8px;
    padding: 2px 8px;
    color: var(--muted);
    font-size: 12px;
  }
  .wsname {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
    font-weight: 600;
  }
  .ws {
    cursor: pointer;
    border-radius: 4px;
  }
  .ws:hover {
    color: var(--fg);
  }
  .wsadd {
    margin-left: auto;
    padding: 0 6px;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--muted);
    font: 15px/1.2 system-ui, -apple-system, sans-serif;
    visibility: hidden;
    cursor: pointer;
  }
  .wsum + .wsadd {
    margin-left: 4px;
  }
  .ws:hover .wsadd,
  .wsadd:focus-visible {
    visibility: visible;
  }
  .wsadd:hover {
    background: var(--line);
    color: var(--fg);
  }
  .empty-ws {
    margin-left: 4px;
  }
  .chev {
    width: 12px;
    margin-left: -6px;
    color: var(--muted);
    font-size: 10px;
  }
  .wsum {
    margin-left: auto;
    flex-shrink: 0; /* nama panjang yang dipotong, ringkasan tetap satu baris */
    white-space: nowrap;
  }
  .x {
    border: 0;
    background: none;
    color: var(--muted);
    visibility: hidden;
    cursor: pointer;
  }
  /* seret tab/workspace (pen layar 09): asal pudar di tempat, salinan ikut kursor, garis sisip biru; tanpa animasi */
  .lift {
    opacity: 0.4;
  }
  .ws.target {
    background: color-mix(in srgb, #3b82f6 14%, transparent);
  }
  .dropline {
    position: fixed;
    z-index: 5;
    height: 2px;
    border-radius: 1px;
    background: #3b82f6;
    pointer-events: none;
  }
  :global(.ghost) {
    position: fixed;
    z-index: 50;
    box-sizing: border-box;
    border: 1px solid #3b82f6;
    border-radius: 6px;
    background: var(--side);
    box-shadow: 0 6px 16px rgb(0 0 0 / 0.35);
    font: 13px system-ui, -apple-system, sans-serif;
    color: var(--fg);
    pointer-events: none;
    overflow: hidden;
  }
  :global(.ghost > *) {
    margin: 0 !important;
    padding: 0 !important;
  }
  :global(.ghost > * > *) {
    margin-top: 0 !important;
  }
  /* kursor tangan tetap kelihatan di atas baris mana pun selama seret */
  .side.dragging,
  .side.dragging * {
    cursor: grabbing !important;
  }
  /* sidebar = chrome app: seret tab/workspace tidak ikut memblok teks; input tetap bisa diblok */
  .side {
    -webkit-user-select: none;
    user-select: none;
  }
  .side input {
    -webkit-user-select: text;
    user-select: text;
  }
  .tab .x {
    display: none;
  }
  .tab:hover .x {
    display: inline;
    visibility: visible;
  }
  .tab:hover .k {
    display: none;
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
  /* FR-65: antrean perhatian (mockup-sidebar-status varian C) */
  .queue {
    display: flex;
    flex-direction: column;
    margin: 8px 0 2px;
    padding: 4px 0;
    border: 1px solid rgba(217, 119, 6, 0.35);
    border-radius: 6px;
    background: rgba(217, 119, 6, 0.07);
  }
  .qhead {
    display: flex;
    justify-content: space-between;
    padding: 2px 8px;
    color: var(--tx-amber, #d97706);
    font-size: 11px;
    font-weight: 600;
  }
  .qhint {
    padding: 2px 8px;
    color: var(--muted);
    font-size: 11px;
  }
  /* FR-52: butuh izin paling mencolok */
  .tab.blocked {
    background: rgba(217, 119, 6, 0.14);
  }
  /* FR-52: warna tab = garis tipis kiri */
  .tab.red { box-shadow: inset 3px 0 0 #ef4444; }
  .tab.orange { box-shadow: inset 3px 0 0 #f97316; }
  .tab.yellow { box-shadow: inset 3px 0 0 #eab308; }
  .tab.green { box-shadow: inset 3px 0 0 #22c55e; }
  .tab.blue { box-shadow: inset 3px 0 0 #3b82f6; }
  .tab.purple { box-shadow: inset 3px 0 0 #a855f7; }
  .tab.pink { box-shadow: inset 3px 0 0 #ec4899; }
  .tab.cyan { box-shadow: inset 3px 0 0 #06b6d4; }
  .tab.brown { box-shadow: inset 3px 0 0 #a16207; }
  /* FR-44 (mockup-warna-tab varian D, Gembala.pen 02): kartu tipis bertint warna workspace, tanpa garis kiri;
     tanpa warna = polos seperti dulu */
  .wsg {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .wsg.red { --c: #ef4444; }
  .wsg.orange { --c: #f97316; }
  .wsg.yellow { --c: #eab308; }
  .wsg.green { --c: #22c55e; }
  .wsg.blue { --c: #3b82f6; }
  .wsg.purple { --c: #a855f7; }
  .wsg.pink { --c: #ec4899; }
  .wsg.cyan { --c: #06b6d4; }
  .wsg.brown { --c: #a16207; }
  .wsg:is(.red, .orange, .yellow, .green, .blue, .purple, .pink, .cyan, .brown) {
    margin-top: 6px;
    padding: 0 0 4px;
    border-radius: 6px;
    /* gelap: dasar warna pane, bukan sidebar, jadi blok lebih gelap dari sidebar (masukan pengguna 4 Nov);
       terang: tint di atas sidebar (--ws-base transparent), karena sidebar terang sudah lebih gelap dari pane */
    background: color-mix(in srgb, var(--c) 5%, var(--ws-base, var(--bg)));
  }
  .wsg:is(.red, .orange, .yellow, .green, .blue, .purple, .pink, .cyan, .brown) > .ws {
    margin-top: 4px;
    color: color-mix(in srgb, var(--c) 70%, var(--ws-mix, #fff));
  }
  /* Gembala.pen 02: nama workspace tebal berwarna workspace */
  .wsg:is(.red, .orange, .yellow, .green, .blue, .purple, .pink, .cyan, .brown) > .ws .wsname {
    color: inherit;
  }
  /* tab aktif/hover di blok berwarna: tint warna blok, bukan kotak abu */
  .wsg:is(.red, .orange, .yellow, .green, .blue, .purple, .pink, .cyan, .brown) > .tab:hover:not(.on) {
    background: color-mix(in srgb, var(--c) 12%, transparent);
  }
  .wsg:is(.red, .orange, .yellow, .green, .blue, .purple, .pink, .cyan, .brown) > .tab.on {
    background: color-mix(in srgb, var(--c) 24%, transparent);
  }
  /* FR-59: pemilih folder */
  .dirs .dq {
    margin: 2px 2px 4px;
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--bg);
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
    outline: none;
  }
  .dirs .dq:focus {
    border-color: #3b82f6;
  }
  .dirs .opt {
    align-items: center;
  }
  .tt {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dot {
    display: inline-block;
    width: 7px;
    height: 7px;
    margin: 0 7px 1px 0;
    border-radius: 50%;
    background: var(--line);
  }
  .dot.blocked {
    background: #d97706;
  }
  .dot.done {
    background: #16a34a;
  }
  .dot.working {
    background: #3b82f6;
    animation: pulse 1.4s ease-in-out infinite; /* FR-52: bekerja = berdenyut */
  }
  /* FR-52: tab beberapa pane = satu titik kecil per pane; pane diam = cincin */
  /* bukan .mini: nama itu sudah dipakai tombol kecil (padding + border) */
  .dot.pdot {
    width: 6px;
    height: 6px;
    margin-right: 3px;
  }
  .dot.pdot:last-of-type {
    margin-right: 7px;
  }
  .dot.idle {
    background: transparent;
    box-shadow: inset 0 0 0 1px var(--muted);
  }
  @keyframes pulse {
    50% {
      opacity: 0.3;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dot.working {
      animation: none;
    }
  }
  /* FR-52: tanda agent di tab dan toolbar */
  .aico {
    display: inline-block;
    width: 1.1em;
    margin-right: 3px;
    font-weight: 700;
    text-align: center;
  }
  .aico.claude {
    color: var(--tx-claude, #d97757);
  }
  .aico.omp {
    color: var(--tx-purple, #a855f7);
  }
  .aico.codex {
    color: var(--tx-codex, #10a37f);
  }
  .aico.opencode {
    color: var(--tx-blue, #3b82f6);
  }
  .st.blocked {
    color: var(--tx-amber, #d97706);
    font-weight: 600;
  }
  .st.done {
    color: var(--tx-green, #16a34a);
  }
  .st.working {
    color: var(--tx-blue, #60a5fa);
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
  /* pintu ke palet ⌘P, bentuk kotak cari supaya mudah ditemukan */
  .add.find {
    border: 1px solid var(--line);
  }
  .add.wsnew {
    font-size: 12px;
  }
  /* kepala workspace pertama langsung di bawah "+ Workspace", tanpa celah ganda */
  .add.wsnew + .ws {
    margin-top: 2px;
  }
  .body {
    position: absolute;
    inset: 0;
  }
  .body.withbar {
    bottom: 26px;
  }
  .body.withhead {
    top: 22px;
  }
  /* FR-59: input editor menempel di bawah pane; baris tambahan tumbuh ke atas menutupi terminal */
  .body.withedit {
    bottom: 86px;
  }
  .body.withedit.withbar {
    bottom: 112px;
  }
  /* FR-59: kotak input jelas (bukan teks polos yang mirip output) */
  .ebox {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 6px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    transition: border-color 120ms;
  }
  .ebox:focus-within {
    border-color: rgba(59, 130, 246, 0.7);
  }
  .ebox .ps {
    color: var(--tx-blue, #3b82f6);
    font: 13px/1.5 "SF Mono", Menlo, monospace;
  }
  .ebox .ghostwrap {
    flex: 1;
    min-width: 0;
  }
  /* pane sempit: petunjuk mengalah duluan (hilang), lalu folder dipotong; branch utuh (TECH 19 #9) */
  .ehint {
    flex: 0 1000 auto;
    min-width: 0;
    margin-left: auto;
    overflow: hidden;
    color: var(--muted);
    font-size: 10.5px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* petunjuk tombol cukup di pane yang fokus; tetap memakan tempat supaya baris chip tidak bergeser */
  .ehint.off {
    visibility: hidden;
  }
  .editor {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 3;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 12px 10px 14px;
    border-top: 1px solid var(--line);
    background: var(--bg);
  }
  .editor.withbar {
    bottom: 26px;
  }
  .ctx {
    container-type: inline-size;
    display: flex;
    gap: 6px;
    font: 11px system-ui, -apple-system, sans-serif;
  }
  @container (max-width: 600px) {
    .ehint {
      display: none;
    }
  }
  .cchip {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: 5px;
    padding: 2px 7px;
    border: 1px solid var(--line);
    border-radius: 4px;
    overflow: hidden;
    color: var(--fg);
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .cchip.dir {
    flex: 0 1 auto;
    min-width: 0;
  }
  button.cchip {
    background: none;
    font: inherit;
    cursor: pointer;
  }
  button.cchip:hover {
    background: var(--line);
  }
  .cchip svg {
    flex: none;
    width: 12px;
    height: 12px;
  }
  .ghostwrap {
    position: relative;
  }
  .editor textarea,
  .ghost {
    box-sizing: border-box;
    width: 100%;
    margin: 0;
    padding: 0;
    border: 0;
    font: 13px/1.5 "SF Mono", Menlo, monospace;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .editor textarea {
    position: relative;
    display: block;
    background: transparent;
    color: var(--fg);
    resize: none;
    outline: none;
  }
  /* saran abu-abu di belakang teks: bagian yang sudah diketik transparan, sisanya samar */
  .ghost {
    position: absolute;
    inset: 0;
    color: var(--muted);
    opacity: 0.6;
    pointer-events: none;
  }
  .ghost .typed {
    color: transparent;
  }
  /* daftar Tab: di atas kotak editor */
  /* FR-59: panel Mulai di tengah pane shell baru; klik di luar tombol tembus ke terminal */
  .start {
    position: absolute;
    inset: 0 0 86px 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    font: 13px system-ui, -apple-system, sans-serif;
    pointer-events: none;
  }
  .start.withhead {
    top: 22px;
  }
  .starts {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
    max-width: 80%;
    pointer-events: auto;
  }
  .sbtn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 7px 12px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  .sbtn:hover {
    border-color: var(--muted);
  }
  .toast {
    position: absolute;
    left: 50%;
    bottom: 96px;
    z-index: 9;
    padding: 6px 14px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--card);
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.25);
    opacity: 0;
    transform: translate(-50%, 6px);
    transition: opacity 150ms, transform 150ms;
    pointer-events: none;
  }
  .toast.show {
    opacity: 1;
    transform: translate(-50%, 0);
  }
  @media (prefers-reduced-motion: reduce) {
    .toast {
      transition: none;
    }
  }
  /* di atas kotak editor (baris folder/branch), tidak menutupi teks yang diketik; `.mention` (bottom: 8px) kalah */
  .mention.complist {
    left: 10px;
    bottom: calc(100% + 6px);
    min-width: 320px;
    max-height: 260px;
    overflow-y: auto;
  }
  .complist .opt {
    grid-template-columns: 14px 1fr auto;
    font-family: "SF Mono", Menlo, monospace;
  }
  .complist .ico {
    color: var(--muted);
  }
  /* FR-61 varian C: saran akun ala mockup-pilih-akun (huruf sistem, dua baris, ikon oranye) */
  .complist.acct {
    min-width: 300px;
    padding: 4px;
    border-radius: 8px;
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.5);
  }
  .complist.acct .opt {
    grid-template-columns: 14px 1fr auto;
    gap: 8px;
    align-items: start;
    padding: 6px 8px;
    border-radius: 5px;
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .complist.acct .ico.claude {
    color: var(--tx-claude, #d97757);
    font-weight: 700;
  }
  .complist.acct .two {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .complist.acct small {
    overflow: hidden;
    color: var(--muted);
    font-size: 11px;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  /* FR-37: kepala pane; tombol muncul saat kursor di atas pane */
  .head {
    position: absolute;
    inset: 0 0 auto 0;
    height: 22px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 6px 0 10px;
    border-bottom: 1px solid var(--line);
    color: var(--muted);
    font: 11px system-ui, -apple-system, sans-serif;
    white-space: nowrap;
  }
  /* pane yang fokus di tab ber-split: garis bawah biru tipis */
  .head.on {
    border-bottom-color: rgba(59, 130, 246, 0.6);
  }
  .hname {
    color: var(--fg);
  }
  .hcwd {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .hb {
    padding: 0 5px;
    border: 0;
    border-radius: 3px;
    background: none;
    color: var(--muted);
    font: 13px system-ui, -apple-system, sans-serif;
  }
  .hb:hover {
    background: var(--line);
    color: var(--fg);
  }
  .head .hb {
    visibility: hidden;
  }
  .tile:hover .head .hb,
  .head.on .hb,
  .head .hb:focus-visible {
    visibility: visible;
  }
  .bar.rest > * {
    visibility: hidden;
  }
  .tile:hover .bar.rest > *,
  .bar.rest:focus-within > *,
  /* nama pane dan akun (FR-61) selalu tampil: pane sebelah tetap jelas siapa dan pakai akun apa */
  .bar.rest > .pname,
  .bar.rest > .acc {
    visibility: visible;
  }
  /* FR-56: blok agent selesai */
  .ended {
    position: absolute;
    inset: 6px 10px auto 10px;
    z-index: 3;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 6px 4px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .ended.withhead {
    top: 28px;
  }
  .ended .mini {
    padding: 2px 8px;
    white-space: nowrap;
  }
  .ended code {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font: 12px "SF Mono", Menlo, monospace;
  }
  .etitle {
    font-weight: 600;
    white-space: nowrap;
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
  /* FR-61: nama akun pane */
  .chip.acc {
    border: 1px solid rgba(59, 130, 246, 0.5);
    color: var(--tx-blue, #93c5fd);
  }
  /* chip berbingkai + ikon ala toolbar CLI agent Warp */
  .chip.box {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 20px;
    padding: 0 7px;
    border: 1px solid var(--line);
    color: var(--fg);
  }
  .chip.box.quiet {
    max-width: 40%;
  }
  .ic {
    flex: none;
    width: 12px;
    height: 12px;
  }
  button.chip:hover {
    background: var(--line);
    color: var(--fg);
  }
  .chip.changed {
    color: var(--tx-amber, #d97706);
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
    color: var(--tx-green, #16a34a);
  }
  .minus {
    color: var(--tx-red, #dc2626);
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
  .mini.pri {
    border-color: #1d4ed8;
    background: rgba(59, 130, 246, 0.15);
  }
  .mini.danger {
    border-color: rgba(220, 38, 38, 0.5);
    color: var(--tx-red, #ef4444);
  }
  .mini .keys {
    margin-left: 4px;
    font-size: 11px;
  }
  .mini:hover {
    background: var(--line);
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
    color: var(--tx-amber, #d97706);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .compose {
    min-width: 420px;
  }
  /* FR-57: daftar @ dan bar lempar, menempel di bawah pane (di atas toolbar agent) */
  .mention,
  .lempar {
    position: absolute;
    left: 10px;
    bottom: 8px;
    z-index: 4;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 6px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.25);
  }
  .mention {
    min-width: 260px;
  }
  .mention .opt {
    grid-template-columns: 10px 1fr auto;
  }
  .lempar {
    right: 10px;
    gap: 6px;
  }
  .mention.withbar,
  .lempar.withbar {
    bottom: 34px;
  }
  /* daftar @ dari input editor: di atas kotak editor */
  .mention.withedit {
    bottom: 94px;
  }
  .mention.withedit.withbar {
    bottom: 120px;
  }
  .lempar .to {
    font-weight: 600;
  }
  .lempar textarea {
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: 13px/1.4 "SF Mono", Menlo, monospace;
    resize: vertical;
    outline: none;
  }
  .targets {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 200px;
    overflow-y: auto;
  }
  .targets .opt {
    grid-template-columns: 10px 1fr auto;
  }
  .tpldlg {
    top: 45%;
    width: min(760px, 92%);
    gap: 12px;
  }
  .tpldlg .thd {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .tpls {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 8px;
  }
  .tplc {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: none;
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
    text-align: left;
    cursor: pointer;
  }
  .tplc.on {
    border-color: #3b82f6;
    background: rgba(59, 130, 246, 0.1);
  }
  .roles {
    border: 1px solid var(--line);
    border-radius: 6px;
    overflow: hidden;
  }
  .rrow {
    display: grid;
    grid-template-columns: 90px 190px 1fr 110px;
    align-items: center;
    gap: 10px;
    padding: 6px 10px;
    border-top: 1px solid var(--line);
    font-size: 12px;
  }
  .rrow.rh {
    border-top: 0;
    background: rgba(255, 255, 255, 0.03);
    color: var(--muted);
  }
  .rrow .edit {
    min-width: 0;
    font-size: 12px;
  }
  .lim.lock {
    color: var(--tx-green, #16a34a);
  }
  .lim.warn {
    color: var(--tx-amber, #d97706);
  }
  .lim.edit {
    color: var(--muted);
  }
  .brief {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .brief .m {
    font-size: 11px;
  }
  .brief textarea {
    resize: vertical;
    font: 12px/1.5 "SF Mono", Menlo, monospace;
  }
  .tpldlg .acts .m {
    flex: 1;
    font-size: 11px;
  }
  .chip.rwarn {
    color: var(--tx-amber, #d97706);
  }
  .wtdlg {
    top: 35%;
    width: min(460px, 86%);
    gap: 10px;
  }
  .wtdlg label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    color: var(--muted);
    font-size: 12px;
  }
  .wtdlg .edit {
    color: var(--fg);
    font: 12px "SF Mono", Menlo, monospace;
  }
  .wtdlg .note {
    padding: 6px 8px;
    border-radius: 5px;
    background: rgba(255, 255, 255, 0.03);
    font-size: 11px;
  }
  .wtdlg .err {
    color: var(--tx-red, #f87171);
    font-size: 12px;
  }
  .wtdlg .files {
    max-height: 30vh;
    margin: 0;
    overflow: auto;
    font: 11px "SF Mono", Menlo, monospace;
  }
  .palette {
    top: 30%;
    width: min(520px, 80%);
  }
  .pallist {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 50vh;
    overflow-y: auto;
  }
  .palette .opt {
    grid-template-columns: 0 1fr auto;
  }
  .compose textarea {
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
    color: var(--tx-red, #dc2626);
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
  /* di atas editor, kartu, dan bar (z-index 3): garis pemisah utuh sampai bawah, input antar pane tidak menyambung */
  .divider {
    position: absolute;
    z-index: 4;
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
  /* pemisah menyala saat disentuh: jelas bisa diseret */
  .divider.row:hover {
    background: linear-gradient(to right, transparent 3px, var(--muted) 3px, var(--muted) 4px, transparent 4px);
  }
  .divider.col:hover {
    background: linear-gradient(to bottom, transparent 3px, var(--muted) 3px, var(--muted) 4px, transparent 4px);
  }
  .sidegrip:hover {
    background: linear-gradient(to right, transparent 3px, var(--muted) 3px, var(--muted) 4px, transparent 4px);
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
    z-index: 8; /* dialog dan palet di atas pemisah pane (4) dan panel samping (5) */
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
    color: var(--tx-red, #f87171);
    font: 13px Menlo, monospace;
    padding: 8px 12px;
  }
</style>
