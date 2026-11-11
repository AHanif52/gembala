<script lang="ts">
  // Fase 1: workspace → tab → pohon split pane (FR-1, 37, 38, 43–46, 50, 52). Tata letak disimpan daemon (FR-45).
  import { onMount, tick, untrack } from "svelte";
  import Tile from "$lib/Tile.svelte";
  import Settings from "$lib/Settings.svelte";
import changelog from "../../../CHANGELOG.md?raw";
import { inline, sections, upgradeNotes, type ReleaseNotes } from "$lib/whatsnew";
  import Rail from "$lib/Rail.svelte";
  import Changes from "$lib/Changes.svelte";
  import OutputView from "$lib/OutputView.svelte";
  import Messages from "$lib/Messages.svelte";
  import NewTask from "$lib/NewTask.svelte";
  import Report from "$lib/Report.svelte";
  import HarnessFlow from "$lib/HarnessFlow.svelte";
  import { live as runLive, stagePanes, type AiAsk, type AiDraft, type Snap } from "$lib/harness";
  import ContextMenu, { type MenuAt, type MItem } from "$lib/ContextMenu.svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { getVersion } from "@tauri-apps/api/app";
  import { activeAddons, pretty, UI_DEFAULT, type Agent, type Config, type Profile } from "$lib/config";
  import { homeDir } from "@tauri-apps/api/path";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { call, b64 } from "$lib/daemon";
  import {
    dividers, neighbor, nudge, panes, place, rects, remove, replace, setRatio, split, swap, valid,
    type Arrow, type Dir, type Divider, type Node,
  } from "$lib/layout";
  import { safeResume, suggestion } from "$lib/resume";
  import { newer, noteLines, upgradeCmd, type Upd } from "$lib/update";
  import { cleanTitle } from "$lib/label";
  import { KINDS, actKey, blockedKind, clear, enabled, newState, plan, settle, soundOn, where as whereOf, wordKey, type Event as NoticeEvent, type NoticeKind } from "$lib/notice";
  import type { UsageRow } from "$lib/usage";
  import { subtabRows } from "$lib/subtabs";
  import { resolveLang, waitingReasonKey } from "$lib/i18n";
  import { errText, i18n, loc, t as tt, tn as ttn, tOr } from "$lib/i18n/state.svelte";
  import type { Key } from "$lib/i18n";

  type Tab = { id: string; title: string; tree: Node; focus: number; color?: string; parent?: string; helpersOpen?: boolean; doneOpen?: boolean };
  type Ws = { id: string; name: string; root: string; tabs: Tab[]; collapsed?: boolean; accounts?: Record<string, string>; color?: string | null; brief?: string }; // FR-61: default account per agent; FR-44: block color; FR-13: brief
  /// Per-pane data to restore (FR-45): profile and folder, no screen content (SEC-13).
  type Meta = {
    profile: string;
    cwd: string | null;
    agent: string;
    name: string;
    busy?: string | null;
    shell?: string | null; // process name of the pane shell (`zsh`), for the subtitle of an idle shell tab
    status?: string;
    waitingShell?: boolean;
    reason?: string;
    session?: string; // FR-53, FR-68: agent session id in the pane (Claude from Gembala; Codex/OpenCode/typed from the daemon)
    git?: any; // FR-55: git summary in the toolbar
    pastes?: string[]; // SEC-34: pasted images, deleted when the pane closes
    output?: boolean; // FR-56: there's output from an earlier agent session
    since?: number; // FR-40: when the process in a shell pane started running
    sig?: string; // FR-51: profile fingerprint when the pane was created
    roleWarn?: boolean; // FR-13: read-only role that can't be locked in this agent
    prev?: { profile: string; agent: string; session?: string; acct?: string }; // FR-45: old pane agent that can be resumed
    zoom?: number; // FR-34: this pane's zoom on top of the global zoom
    policy?: string; // SEC-5
    readable?: boolean; // SEC-21
    hist?: string; // FR-62: history key from the daemon, sent again when this pane comes back (rerun, daemon restart)
    from?: { boot: string; pane: number }; // FR-62: old pane (previous daemon) of a pane without `hist` (daemon from before keys)
    ended?: { resume: string | null }; // FR-56: finished agent block with buttons, gone once a new process runs
    acct?: string; // FR-61: account id picked when the pane was created (used again on rerun)
    aliases?: string[]; // FR-79: old pane names, carried to the replacement pane so they stay addresses
    accountName?: string; // FR-61: account name from the daemon for the chip; empty = Utama
    accountId?: string; // FR-68: id of the running Claude account (`claude-<id>`); empty = Utama
    owner?: { name: string; id: string }; // FR-61: the running Claude session belongs to another account (Resume uses that account)
    login?: string; // FR-61: "log in to account …" card after the Login button
    saver?: boolean; // FR-27: the token saver is on for this pane (badge in the head)
  };
  const AGENTS = ["claude", "codex", "opencode", "omp"];
  // FR-4/52: status dot + text label, not color alone (BRD section 11)
  const stText = (s?: string) => (s === "blocked" || s === "done" || s === "working" ? tt(`status.${s}`) : "");
  // FR-52: tab color picked by the user (thin line on the left)
  const COLORS = ["red", "orange", "yellow", "green", "blue", "purple", "pink", "cyan", "brown"] as const; // label: color.<c>
  /// FR-44: a new workspace gets a color automatically: the one least used by other workspaces, starting with blue (Gembala.pen 02)
  const FIRST = "blue";
  const nextColor = (list: Ws[]) => {
    const used = (c: string) => list.filter((w) => w.color === c).length;
    return COLORS.reduce<string>((best, c) => (used(c) < used(best) ? c : best), FIRST);
  };
  // FR-52: mark per agent (not a brand logo), color close to the agent's signature color
  const GLYPH: Record<string, string> = { claude: "✳", omp: "π", codex: "◎", opencode: "◇" };
  /// agent running in the pane (from a profile or typed by hand); "" = none
  const agentOf = (p: number) => (AGENTS.includes(meta[p]?.busy ?? "") ? meta[p].busy! : "");
  /// FR-61: Claude account name of a pane running Claude; once another account exists, Utama gets a label too
  /// FR-78: by current config; a pane launched before a change keeps its old flags until it is restarted
  const addonsOf = (p: number) => activeAddons(cfg?.values.addons, agentOf(p), wsIdOf(p) ?? "", { none: tt("pane.addonNone"), only: tt("pane.addonOnly") });
  const acctOf = (p: number) =>
    meta[p]?.accountName || (agentOf(p) === "claude" && (cfg?.values.accounts ?? []).some((a) => a.agent === "claude") ? tt("acc.main") : "");
  /// Color submenu = swatch panel (Gembala.pen 08): colors, then the empty circle for no color; ring = current
  const colorMenu = (cur: string | null | undefined, set: (c: string | undefined) => void): MItem[] => [
    ...COLORS.map((c) => [tt(`color.${c}`), () => set(c), true, c, { on: cur === c }] as MItem),
    [tt("color.none"), () => set(undefined), true, "none", { on: !cur }],
  ];
  const rank = (s?: string) => ["working", "done", "blocked"].indexOf(s ?? "");
  /// status shown: a "done" that's already been seen fades (FR-40)
  const stat = (p: number) => (ack[p] && meta[p]?.status === "done" && !meta[p]?.waitingShell ? "" : (meta[p]?.status ?? ""));
  // FR-87: a pane that waits on you says the same word as the banner, bell and strip (a shell's password prompt has no reason)
  const paneStatusText = (p: number) =>
    meta[p]?.waitingShell ? `⌛ ${tt("status.shell")}`
    : stat(p) === "blocked" ? kindText(blockedKindOf(p), meta[p]?.reason ? (waitingReasonKey(meta[p].reason) ?? meta[p].reason) : "")
    : meta[p]?.reason ? tOr(waitingReasonKey(meta[p].reason) ?? meta[p].reason, meta[p].reason) : stText(stat(p));
  const worst = (ps: number[]): string => ps.reduce((a, p) => (rank(stat(p)) > rank(a) ? stat(p) : a), "");
  const tabStatus = (t: Tab) => worst(panes(t.tree));

  let wss = $state<Ws[]>([]);
  let at = $state({ ws: 0, tab: 0 });
  let meta = $state<Record<number, Meta>>({});
  let blockedAt = $state<Record<number, number>>({}); // FR-65: sejak kapan pane butuh izin
  let statusAt = $state<Record<number, number>>({}); // FR-101: daemon status time, milliseconds in the UI
  let previews = $state<Record<number, string>>({}); // hook answer, RAM only (SEC-13)
  let quietAt = $state<Record<number, number>>({}); // FR-101: keep a new completion visible for one minute
  let now = $state(Date.now()); // FR-65: time waiting in the queue, refreshed every poll
  let ack = $state<Record<number, true>>({});
  const readStatus = new Set<number>(); // FR-87: panes whose status was read once since the UI connected
  let observingSince = 0; // FR-87: when this UI started reading pane status (unix ms)
  let labels = $state<Record<number, string>>({}); // FR-69: from the agent's terminal title; not in layout.json (SEC-13) // panes whose "done" status was already seen; cleared when the status changes
  let dead = $state<Record<number, true>>({}); // panes from another daemon run or already exited (negative id)
  let boot = $state(""); // state: the layout is saved again when a new daemon starts
  let zoomed = $state(false);
  let confirm = $state<{ pane: number; name: string; close: number[]; what: string } | null>(null);
  let quitAsk = $state(0); // number of panes still running a process (FR-46)
  let linkAsk = $state(""); // SEC-14: the real URL is shown before opening
  let zoom = $state(0); // FR-34 ⌘=/⌘- temporary on top of the config size; ponytail: per-pane zoom comes later
  let cfg = $state<Config | null>(null);
  let settingsOpen = $state(false);
  let settingsSec = $state("umum"); // section Settings opens on (the phone icon opens Telegram)
  let tg = $state<any>({ state: "off" }); // FR-80: Telegram bot status from the daemon
  let usageRows = $state<UsageRow[]>([]); // FR-102: daemon-owned usage readings; old daemons return no rows
  let hintReset = $state(0);
  // FR-82: newer release on GitHub (daemon checks); "Lewatin versi ini" remembers one version on this Mac
  let upd = $state<Upd | null>(null);
  let appVer = $state("");
  let whatsNew = $state<ReleaseNotes[] | null>(null);
  /// FR-103: the card opened itself on an upgrade (only that one names the releases it skipped)
  let whatsNewAuto = $state(false);
  const SEEN_VERSION = "gembala.seen_version";
  let seenVersion = $state((() => { try { return localStorage.getItem(SEEN_VERSION) ?? ""; } catch { return ""; } })());
  let restored = $state(false);
  /// FR-103: restore found no layout = a new install, where the welcome card (FR-43) stands in for this one
  let freshInstall = $state(false);
  function saveSeenVersion(version: string) {
    seenVersion = version;
    try { localStorage.setItem(SEEN_VERSION, version); } catch {}
  }
  function dismissWhatsNew() {
    saveSeenVersion(appVer);
    whatsNew = null;
  }
  function openWhatsNew() {
    whatsNewAuto = false;
    whatsNew = sections(changelog, "", appVer, i18n.lang);
  }
  let updOpen = $state(false);
  const SKIP = "gembala.update.lewati";
  let skipped = $state((() => {
    try {
      return localStorage.getItem(SKIP) ?? "";
    } catch {
      return "";
    }
  })());
  const updNew = $derived(!!upd?.latest && newer(upd.latest, appVer) && upd.latest !== skipped);
  function skipUpdate() {
    skipped = upd?.latest ?? "";
    updOpen = false;
    try {
      localStorage.setItem(SKIP, skipped);
    } catch {}
  }
  let cfgSeen = $state(""); // config errors already dismissed (FR-42 card)
  let picker = $state<{ items: (Profile & { missing: boolean; key: string; label: string; account?: string })[]; sel: number } | null>(null);
  let sidebar = $state(true);
  let sideW = $state(220); // FR-52: sidebar width, saved in the layout
  let pal = $state<{ q: string; sel: number; keys: [string, string, string][] } | null>(null); // FR-39 palet
  /// `line` = insertion line (screen coordinates) where it drops
  let dragging = $state<{ kind: "tab" | "ws"; wi: number; ti: number; over: { wi: number; ti: number }; line: { top: number; left: number; width: number } } | null>(null);
  /// FR-37: drag a pane head; `zone` = highlight box (screen coordinates) + target label
  let paneDrag = $state<{ zone: { left: number; top: number; width: number; height: number; label: string } | null } | null>(null);
  let editing = $state<{ kind: "tab" | "ws" | "pane"; id: string; value: string } | null>(null);
  let renameError = $state("");
  let welcome = $state<Agent[] | null>(null); // FR-43: short card on first open
  let upgrade = $state<{ daemon: string; app: string; panes: number } | null>(null); // FR-21
  let wsAsk = $state<{ wi: number; busy: number } | null>(null); // FR-44: delete a workspace that's still running
  let rich = $state<{ pane: number; text: string; files?: string[]; error?: string; args?: any } | null>(null); // FR-14; files = attachment chips (pen 04)
  let changes = $state<{ pane: number; diff: any; turns?: any[]; turn?: number | null; ask?: any; act?: any } | null>(null); // FR-22, FR-70, FR-23
  let outputOf = $state<number | null>(null); // FR-56

  const ui = $derived(cfg?.values.ui ?? UI_DEFAULT);
  /// FR-97: everyday words and folded technical fields; the question is asked once while the mode is "ask"
  const simple = $derived(ui.mode === "simple");
  const askMode = $derived(!!cfg && ui.mode === "ask");
  const setMode = (m: "simple" | "full") => call("config.set", { section: "ui", key: "mode", value: m }).catch(() => {});
  const ws = $derived(wss[at.ws]);
  const tab = $derived(ws?.tabs[at.tab]);
  const owner = $derived(tab?.parent ? ws?.tabs.find((t) => t.id === tab.parent) : null);
  let compareId = $state(""); // temporary two-pane view; never saved to layout.json
  const compareHelper = $derived(ws?.tabs.find((t) => t.id === compareId && t.parent));
  const compareOwner = $derived(compareHelper ? ws?.tabs.find((t) => t.id === compareHelper.parent) : null);
  const TIP_KEY = "gembala.hint.helper";
  let latestHelper = $state<{ id: string; parent: string } | null>(null);
  let helperTip = $state((() => { try { return localStorage.getItem(TIP_KEY) !== "done"; } catch { return true; } })());
  const dismissHelperTip = () => { helperTip = false; try { localStorage.setItem(TIP_KEY, "done"); } catch {} };
  const openTab = (wi: number, t: Tab) => {
    at = { ws: wi, tab: wss[wi].tabs.indexOf(t) };
    if (latestHelper?.id === t.id) latestHelper = null;
    if (compareId) compareId = t.parent ? t.id : "";
    zoomed = false;
  };
  const goOwner = () => { if (owner && ws) openTab(at.ws, owner); };
  const nextHelper = (step: number) => {
    const root = owner ?? tab;
    const kids = ws?.tabs.filter((t) => t.parent === root?.id) ?? [];
    if (!kids.length) return;
    const i = kids.findIndex((t) => t.id === tab?.id);
    openTab(at.ws, kids[i < 0 ? (step > 0 ? 0 : kids.length - 1) : (i + step + kids.length) % kids.length]);
  };
  const toggleCompare = () => { if (compareId) { const t = compareHelper; if (t && ws) at = { ws: at.ws, tab: ws.tabs.indexOf(t) }; compareId = ""; } else if (tab?.parent) compareId = tab.id; };
  const helperQuiet = (t: Tab) => ["done", "idle"].includes(meta[t.focus]?.status ?? "") && (!quietAt[t.focus] || now - quietAt[t.focus] >= 60_000);
  const helperStatus = (p: number) => {
    const s = meta[p]?.status;
    if (s === "blocked") return `${paneStatusText(p) || tt("status.blocked")} · ${ago(now - (blockedAt[p] ?? now))}`;
    if (s === "working" && statusAt[p]) return tt("helper.workingFor", { time: ago(now - statusAt[p]) });
    if (s === "done" && quietAt[p]) return now - quietAt[p] >= 86_400_000 ? tt("helper.finishedYesterday") : tt("helper.finishedAt", { time: new Date(quietAt[p]).toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" }) });
    if (s === "idle" && quietAt[p]) return tt("helper.idleFor", { time: ago(now - quietAt[p]) });
    return s === "done" ? tt("status.done") : paneStatusText(p) || tt("status.idle");
  };
  const helperRows = (w: Ws) => subtabRows(w.tabs, helperQuiet, (t) => meta[t.focus]?.status === "blocked", tab?.id);
  // saves RAM/CPU: xterm only for the active tab + the last 3 opened; other tabs are detached (the daemon keeps the panes),
  // reopening = re-attach from the daemon snapshot
  const LIVE_TABS = 4;
  let liveTabs = $state<string[]>([]);
  $effect(() => {
    const id = tab?.id;
    if (id) liveTabs = [id, ...untrack(() => liveTabs).filter((x) => x !== id)].slice(0, LIVE_TABS);
  });
  // Phase 2: message drawer, manual send, message engine data from the daemon
  let drawer = $state(false);
  // FR-16, FR-26, FR-73: harness. `runs` = latest run per workspace id (the daemon pushes harness.state)
  let newTask = $state(false);
  let flow = $state(false);
  let runs = $state<Record<string, Snap>>({});
  const runOwner: Record<string, string> = {}; // run id -> owner tab id, for stages opened after the first
  let msgData = $state<any>(null);
  let compose = $state<{ to: number | null; body: string; force: boolean; error: string } | null>(null);
  async function refreshMsgs() {
    const picked = new Map((msgData?.opens ?? []).map((o: any) => [o.id, o]));
    msgData = await call("msg.list").catch(() => msgData);
    // FR-75: a model/effort the user changed on an open card survives the refresh
    for (const o of msgData?.opens ?? []) {
      const p: any = picked.get(o.id);
      if (p) Object.assign(o, { model: p.model, effort: p.effort });
    }
  }
  const pendingFor = (pane: number) => (msgData?.pending ?? []).filter((m: any) => m.to === meta[pane]?.name);
  /// FR-75: agent asked for an agent that isn't open; the card sits in the asking pane
  const opensFor = (pane: number) => (msgData?.opens ?? []).filter((o: any) => o.from_pane === pane);
  /// FR-86: an agent asked to close another pane; the card sits in the asking pane too
  const closesFor = (pane: number) => (msgData?.closes ?? []).filter((c: any) => c.from_pane === pane);
  const whatsNewBlocked = $derived(askMode || !!welcome || Object.values(meta).some((m) => m.status === "blocked") || !!confirm || !!linkAsk || !!quitAsk || !!wsAsk || !!(msgData?.pending?.length || msgData?.opens?.length || msgData?.closes?.length));
  const openWho = (o: any) => {
    const p = cfg?.values.profiles.find((x) => x.id === o.profile);
    const accs = cfg?.values.accounts ?? [];
    const acct = o.account || wss.find((w) => w.id === o.workspace)?.accounts?.[p?.agent ?? ""] || "";
    const name = acct && acct !== "utama" ? accs.find((a) => a.id === acct)?.name : accs.some((a) => a.agent === p?.agent) ? tt("acc.main") : "";
    return [p?.name ?? o.profile, name].filter(Boolean).join(" · ");
  };
  const agentForOpen = (o: any) => cfg?.values.profiles.find((x) => x.id === o.profile)?.agent;
  /// FR-101: a helper gets its own tab under the asker's tab; the asker's focus stays put.
  /// FR-75 requests being opened right now: a second click (card, drawer, double click) must not open a second pane
  const opening = new Set<string>();
  let openModels = $state<string[]>([]);
  let openEfforts = $state<string[]>([]);
  async function approveOpen(o: any) {
    const w = wss.find((w) => w.tabs.some((x) => panes(x.tree).includes(o.from_pane)));
    const t = w?.tabs.find((x) => panes(x.tree).includes(o.from_pane));
    if (!t || opening.has(o.id) || !(msgData?.opens ?? []).some((x: any) => x.id === o.id)) return;
    opening.add(o.id);
    try {
      const id = await createPane({ workspace: o.workspace, profile: o.profile, account: o.account || undefined, cwd: o.cwd || undefined, model: o.model, effort: o.effort });
      quietAt[id] = Date.now();
      const child: Tab = { id: uid(), title: "", tree: { pane: id }, focus: id, parent: t.id };
      t.helpersOpen = true;
      w!.tabs.splice(w!.tabs.indexOf(t) + 1 + w!.tabs.filter((x) => x.parent === t.id).length, 0, child);
      const r = await call("open.approve", { id: o.id, pane: id }).catch((e) => ({ error: String(e) }));
      // refused (already answered, gone): the pane would sit there unowned, so it goes again
      if (r?.error) {
        w!.tabs.splice(w!.tabs.indexOf(child), 1);
        delete quietAt[id];
        call("pane.close", { pane: id }).catch(() => {});
      } else latestHelper = { id: child.id, parent: t.id };
    } finally {
      opening.delete(o.id);
      refreshMsgs();
    }
  }
  const wsIdOf = (pane: number) => wss.find((w) => w.tabs.some((t) => panes(t.tree).includes(pane)))?.id ?? ws?.id;
  /// status panel (FR-10): every pane in the active workspace
  const rows = $derived(
    (ws?.tabs ?? [])
      .flatMap((t) => panes(t.tree))
      .filter((p) => !dead[p] && meta[p])
      .map((p) => ({ pane: p, name: meta[p].name, agent: meta[p].agent || "shell", status: meta[p].status, policy: meta[p].policy, readable: meta[p].readable })),
  );
  // send targets: agent panes other than the focused one, including agents typed by hand in a shell pane (status tracked by the daemon)
  const targets = $derived(rows.filter((r) => r.pane !== tab?.focus && (r.agent !== "shell" || r.status)));

  /// Manual send from the user (⌘⇧K, FR-57); "" = sent, otherwise an error code (its text goes through errText when shown)
  async function sendMsg(to: number, body: string, force: boolean): Promise<string> {
    try {
      await call("msg.send", { to, body, force });
      refreshMsgs();
      say(tt("main.sent", { name: meta[to]?.name ?? "agent" }));
      return "";
    } catch (e) {
      return String(e);
    }
  }

  async function sendCompose() {
    if (!compose || compose.to === null) return;
    const error = await sendMsg(compose.to, compose.body, compose.force);
    if (!error) compose = null;
    else if (compose) compose.error = error;
  }

  // FR-59: Warp-style input editor in a shell pane waiting at the prompt (OSC 133 markers from FR-58)
  const editorOn = $derived(ui.input_editor); // [ui] input_editor: the daemon also hides the zsh prompt of new panes
  let atPrompt = $state<Record<number, boolean>>({});
  let draft = $state<Record<number, string>>({});
  const history: string[] = []; // ponytail: app session history only; zsh history stays on the terminal's ↑
  let hi = -1;
  const editorShown = (pane: number) => editorOn && atPrompt[pane] && !dead[pane] && lempar?.pane !== pane;
  /// text is sent as bracketed paste (multiple lines don't run one by one); ESC/C0/C1 dropped like FR-14
  function toShell(pane: number, text: string, enter: boolean) {
    const clean = text.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "");
    const body = clean ? `\x1b[200~${clean}\x1b[201~` : "";
    call("pane.write", { pane, data: b64(new TextEncoder().encode(body + (enter ? "\r" : ""))) });
  }
  let editorErr = $state<Record<number, string>>({});
  async function runDraft(pane: number) {
    const text = draft[pane] ?? "";
    // `@name message` = hand off to that agent (FR-57); text starting with `@` is never run by the shell
    const m = /^@([\w.-]*)\s*([\s\S]*)$/.exec(text);
    if (m) {
      const to = targets.find((r) => [r.name, r.agent, meta[r.pane]?.busy ?? ""].includes(m[1]));
      if (!to) return void (editorErr[pane] = tt("main.noAgent", { name: m[1] }));
      if (!m[2].trim()) return void (editorErr[pane] = tt("main.writeAfter", { name: to.name }));
      const error = await sendMsg(to.pane, m[2], false);
      if (error) return void (lempar = { pane, to: to.pane, name: to.name, body: m[2], error });
      draft[pane] = "";
      return;
    }
    if (text.trim() && history.at(-1) !== text) history.push(text);
    if (history.length > 200) history.shift();
    hi = -1;
    draft[pane] = "";
    atPrompt[pane] = false; // shows again at the next prompt
    toShell(pane, text, true);
  }
  // FR-59: "Start" panel in a shell pane that hasn't run anything yet
  let ran = $state<Record<number, boolean>>({});
  let installed = $state<string[]>([]); // installed agents (agents.detect), filled when the app opens
  const usageActive = $derived(Object.entries(meta).filter(([id]) => !dead[+id]).map(([, m]) => ({ agent: AGENTS.includes(m.busy ?? "") ? m.busy! : m.agent, login: m.acct || m.accountId || "utama" })));
  /// focused pane that shows the FR-59 Start panel (the welcome card joins it); null = none
  const startHere = $derived(tab && editorShown(tab.focus) && !ran[tab.focus] && !draft[tab.focus] ? tab.focus : null);
  // FR-61: one button per account when that agent has accounts besides Utama (like the palette)
  const startProfiles = $derived(
    (cfg?.values.profiles ?? [])
      .filter((p) => p.agent !== "shell" && installed.includes(p.agent))
      .flatMap((p) => {
        const own = (cfg?.values.accounts ?? []).filter((a) => a.agent === p.agent);
        const base = { ...p, key: p.id, label: p.name, account: undefined as string | undefined };
        return own.length ? [{ id: "utama", name: tt("acc.main") }, ...own].map((a) => ({ ...base, key: `${p.id}:${a.id}`, label: `${p.name} · ${a.name}`, account: a.id })) : [base];
      }),
  );
  // FR-72: agents with their own session picker (OpenCode has none from the command line)
  const pickable = $derived(startProfiles.filter((p) => ["claude", "codex", "omp"].includes(p.agent)));
  // FR-71: agent in a new worktree (pen screen 07): name, folder (default `<repo>.<name>`), profile
  let wt = $state<{ pane: number; wi: number; root: string; name: string; dir: string; dirSet: boolean; prof: number; err: string } | null>(null);
  // worktree errors have their own wording (wt.err.<code>), anything else goes through errText
  function openWorktree(wi = at.ws) {
    const w = wss[wi];
    const pane = wi === at.ws ? tab?.focus : w?.tabs[0]?.focus;
    const root = pane === undefined ? "" : meta[pane]?.git?.root;
    if (pane === undefined || !root) return say(tt("wt.err.not_repo"));
    const name = `agent-${(w.tabs.length + 1).toString()}`;
    wt = { pane, wi, root, name, dir: `${root}.${name}`, dirSet: false, prof: 0, err: "" };
    tick().then(() => document.querySelector<HTMLInputElement>(".wtdlg input")?.select());
  }
  async function createWorktree() {
    if (!wt) return;
    const p = startProfiles[wt.prof];
    const r = await call("git.worktree_add", { pane: wt.pane, name: wt.name.trim(), dir: wt.dirSet ? wt.dir : undefined }).catch((e) => ({ error: String(e?.message ?? e) }));
    if (r.error) return void (wt.err = tOr(`wt.err.${r.error}`, errText(r.error, r.args)));
    const wi = wt.wi;
    wt = null;
    await newTab(p?.id, wi, p?.account, { cwd: r.dir, from_pane: undefined });
    say(`Worktree ${home(r.dir)} · branch ${r.branch}`);
  }
  /// FR-71: close the tab, then remove its worktree; uncommitted changes = a confirmation listing the files
  async function removeWorktree(wi: number, t: Tab) {
    const dir = meta[t.focus]?.git?.root;
    if (!dir) return;
    const go = async (force: boolean) => {
      for (const p of panes(t.tree)) await call("pane.close", { pane: p }).catch(() => {});
      await new Promise((r) => setTimeout(r, 500));
      const r = await call("git.worktree_remove", { dir, force }).catch((e) => ({ error: String(e?.message ?? e) }));
      if (r.error) return say(r.error === "pane_in_worktree" ? tt("err.pane_in_worktree") : tt("wt.removeFailed", { why: errText(r.error, r.args) }));
      if (r.dirty) return void (wtDel = { wi, dir, files: r.dirty });
      say(tt("wt.removed", { dir: home(dir) }));
    };
    wtDel = null;
    await go(false);
    wtForce = () => go(true);
  }
  let wtDel = $state<{ wi: number; dir: string; files: string[] } | null>(null);
  let wtForce: () => void = () => {};
  // FR-13: role templates (pen screen 17): one pane per role in a new tab, the workspace brief goes into the launch hint
  // name/desc: tpl.<key>, tpl.<key>Desc; role label: tpl.role.<label key> + number; role id goes to the daemon as is;
  // pane names are addresses agents type (`gembala send worker-1`), so they stay English in both languages
  const TEMPLATES: { key: string; roles: [string, string, string, string][] }[] = [
    { key: "coord", roles: [["coord", "koordinator", "claude", "coord"], ["worker 1", "pekerja", "claude", "worker-1"], ["worker 2", "pekerja", "codex", "worker-2"], ["reviewer", "reviewer", "codex", "review"]] },
    { key: "plan", roles: [["planner", "planner", "claude", "plan"], ["worker", "pekerja", "claude", "worker"]] },
    { key: "pair", roles: [["reviewer 1", "reviewer", "claude", "review-1"], ["reviewer 2", "reviewer", "codex", "review-2"]] },
  ];
  const roleLabel = (l: string) => {
    const [k, n] = l.split(" ");
    return `${tt(`tpl.role.${k}` as Key)}${n ? ` ${n}` : ""}`;
  };
  const READONLY = ["koordinator", "planner", "reviewer"];
  let tpl = $state<{ wi: number; t: number; rows: { label: string; role: string; prof: number; name: string }[]; brief: string } | null>(null);
  function pickTemplate(t: number) {
    if (!tpl) return;
    const prefer = (agent: string) => Math.max(0, startProfiles.findIndex((p) => p.agent === agent));
    tpl.t = t;
    tpl.rows = TEMPLATES[t].roles.map(([label, role, agent, name]) => ({ label, role, prof: prefer(agent), name }));
  }
  function openTemplate(wi = at.ws) {
    if (!startProfiles.length) return say(tt("main.noAgents"));
    tpl = { wi, t: 0, rows: [], brief: wss[wi]?.brief ?? "" };
    pickTemplate(0);
  }
  const limitOf = (role: string, prof: number) =>
    !READONLY.includes(role) ? "edit" : startProfiles[prof]?.agent === "claude" ? "lock" : "warn";
  async function runTemplate() {
    if (!tpl) return;
    const { wi, rows } = tpl;
    const title = tt(`tpl.${TEMPLATES[tpl.t].key}` as Key);
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
      if (r.name.trim()) call("pane.rename", { pane: id, name: r.name.trim().toLowerCase() }).then((x) => renamed(id, x)).catch(() => {});
    }
    if (!ids.length) return;
    // 2 = left|right, 3 = left | right top/bottom, 4 = 2x2 grid
    let tree: Node = { pane: ids[0] };
    if (ids[1] !== undefined) tree = split(tree, ids[0], "row", ids[1]);
    if (ids[2] !== undefined) tree = split(tree, ids.length > 3 ? ids[0] : ids[1], "col", ids[2]);
    if (ids[3] !== undefined) tree = split(tree, ids[1], "col", ids[3]);
    w.tabs.push({ id: uid(), title, tree, focus: ids[0] });
    at = { ws: wi, tab: w.tabs.length - 1 };
    zoomed = false;
  }
  /// the shell pane is replaced by a profile pane in the same spot (agent runs with official hooks, FR-4)
  async function swapToProfile(old: number, profile: string, account?: string, pick = false): Promise<number | undefined> {
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
    call("pane.close", { pane: old }).catch(() => {}); // its pane.exit doesn't find the tab anymore: ignored
    return id;
  }

  /// FR-99 P1: a sentence typed into the terminal by mistake. zsh answers 127 (command not found); three words or more,
  /// so a mistyped short command (`gti status`) doesn't get the offer. Shown in Simple and Full (user decision 10 Nov 2026).
  let offer = $state<Record<number, string>>({});
  /// FR-100: the report dialog (the user's own, or an agent's draft from `gembala report`)
  let reportDraft = $state<{ title: string; body: string; from?: string; id?: string } | null>(null);
  const reportsFor = (pane: number) => (msgData?.reports ?? []).filter((r: any) => r.from_pane === pane);
  // a word starting with "-" is a flag: a command that wasn't found (`claude-kantor --permission-mode default`), not a sentence
  const askOffer = (cmd: string, code: number) => code === 127 && cmd.trim().split(/\s+/).length >= 3 && !/(^|\s)-/.test(cmd);
  /// ponytail: the first installed agent is offered; remember the last one used per workspace when people ask for it
  const offerAgent = $derived((cfg?.values.profiles ?? []).find((p) => p.agent !== "shell" && installed.includes(p.agent)));
  /// "Kirim": this terminal pane becomes the agent and the sentence is its first message (same wait as a manual task)
  async function sendOffer(pane: number) {
    const text = offer[pane];
    delete offer[pane];
    if (!offerAgent) return void (welcome = await call("agents.detect", {}).catch(() => []));
    const id = await swapToProfile(pane, offerAgent.id);
    if (id !== undefined) await firstMessage(id, text);
  }
  async function firstMessage(id: number, task: string) {
    await new Promise((r) => setTimeout(r, 4000));
    const sent = await call("msg.send", { to: id, body: task }).then((r) => !r?.error, () => false);
    if (!sent) rich = { pane: id, text: task };
  }
  /// FR-101 (5): what the "⋯" of a narrow pane bar offers (the chips the width hid, and the rest)
  function barMenu(pane: number, g: any): MItem[] {
    const m: MItem[] = [[tt("chip.attach"), async () => attach(pane, await invoke<string[]>("pick_files"), true)]];
    if (g?.repo && g.files) m.push([`${tt("chip.diffTip")} · ${g.files}`, () => openChanges(pane)]);
    m.push(["Rich Input ⌘G", () => openRich(pane)]);
    if (meta[pane]?.cwd) m.push([`${tt("chip.copyCwd")} · ${short(meta[pane].cwd)}`, () => copy(meta[pane]?.cwd ?? "")]);
    if (g?.repo && g.branch) m.push([`${tt("chip.branch")} ${g.branch}`, () => copy(g.branch)]);
    m.push([tt("set.title"), () => (settingsOpen = true)]);
    return m;
  }

  async function startInFolder(pane: number) {
    const path = await invoke<string | null>("pick_folder").catch(() => null);
    if (path) toShell(pane, `cd ${quote(path)}`, true);
  }

  /// FR-99 P1: "Buka Claude Code" on the welcome card = the Start panel's pane becomes that agent, or a new tab
  async function openAgent(agent: string) {
    const p = (cfg?.values.profiles ?? []).find((x) => x.agent === agent);
    if (!p) return;
    welcome = null;
    if (startHere !== null) await swapToProfile(startHere, p.id);
    else await newTab(p.id);
  }
  /// official install guide per agent (opened through the SEC-14 link check like any link)
  const GUIDE: Record<string, string> = {
    claude: "https://code.claude.com/docs/en/overview",
    codex: "https://github.com/openai/codex",
    omp: "https://github.com/can1357/oh-my-pi",
    opencode: "https://opencode.ai/docs",
  };
  let installing = $state(false);
  async function redetect() {
    welcome = await call("agents.detect", {}).catch(() => welcome);
    if (welcome?.some((a) => a.installed)) installing = false;
  }
  /// FR-99 P1: a terminal tab "Pasang <agent>" with the install command typed but not run: the user reads it and presses Enter.
  /// Detection runs again every 5 s while the welcome card waits (ponytail: polling; hook it to the block's exit if it gets in the way).
  async function installInTerminal(a: Pick<Agent, "name" | "install">) {
    const id = await newTab();
    const t = ws?.tabs.find((x) => panes(x.tree).includes(id));
    if (t) t.title = tt("wel.installTab", { name: a.name });
    if (editorShown(id)) draft[id] = a.install;
    else toShell(id, a.install, false);
    if (installing) return;
    installing = true;
    const until = Date.now() + 10 * 60_000;
    while (installing && welcome && Date.now() < until) {
      await new Promise((r) => setTimeout(r, 5000));
      await redetect();
    }
    installing = false;
  }

  /// FR-59: attachments in the editor = quoted paths inserted into the text (+ button, file drag, ⌘V image)
  function insertPaths(pane: number, paths: string[]) {
    if (!paths.length) return;
    const d = draft[pane] ?? "";
    draft[pane] = d + (d && !d.endsWith(" ") ? " " : "") + paths.map(quote).join(" ") + " ";
    document.querySelector<HTMLTextAreaElement>(`[data-editor="${pane}"]`)?.focus();
  }
  const IMG: Record<string, string> = { "image/png": "png", "image/jpeg": "jpg", "image/gif": "gif", "image/webp": "webp" };
  /// ⌘V image in the editor: saved to Gembala's temp folder (SEC-34), then its path inserted, like in the terminal (FR-54)
  async function pasteToEditor(pane: number, e: ClipboardEvent, add = (path: string) => insertPaths(pane, [path])) {
    const item = [...(e.clipboardData?.items ?? [])].find((i) => i.kind === "file" && IMG[i.type]);
    const file = item?.getAsFile();
    if (!item || !file) return; // plain text: the textarea handles it
    e.preventDefault();
    const path = await invoke<string>("save_paste", { data: b64(new Uint8Array(await file.arrayBuffer())), ext: IMG[item.type] }).catch(() => "");
    if (!path) return say(tt("main.imageFailed"));
    if (meta[pane]) meta[pane].pastes = [...(meta[pane].pastes ?? []), path];
    add(path);
  }

  /// Tab/Esc: text moves to the zsh line, control goes to the terminal (zsh autocomplete still works)
  function handOff(pane: number, tab: boolean) {
    toShell(pane, draft[pane] ?? "", false);
    if (tab) call("pane.write", { pane, data: b64(new Uint8Array([9])) });
    draft[pane] = "";
    atPrompt[pane] = false;
    tiles[pane]?.focus();
  }
  // FR-59 Warp-style autocomplete: gray suggestion from history (→ accepts), Tab = folder/file/command list
  let zhist: string[] = []; // zsh history, loaded once when the app opens
  let homePath = $state("");
  type Comp = { label: string; kind: string; insert: string };
  // `auto` = account suggestions that show up on their own while typing `cl…` (FR-61): Tab/Enter fills the command, then
  // arguments can be added before the next Enter runs it
  let comp = $state<{ pane: number; items: Comp[]; sel: number; auto?: boolean } | null>(null);
  /// FR-59: folder picker from the folder chip (Warp-style): search, "..", subfolders; Enter = `cd` in the pane
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
    // hidden folders are only searched when the search starts with a dot (like Tab in the editor)
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
  /// FR-61 variant C: `cl…` in the input = one suggestion per Claude account; first selected = the workspace default account
  async function accountSuggest(pane: number) {
    const r = await call("account.list", {}).catch(() => null);
    const rows: { account: { id: string; name: string; agent: string }; status: { email?: string } }[] = r?.accounts ?? [];
    const items = rows
      .filter((x) => x.account.agent === "claude")
      // FR-61: Utama = `claude-utama`, not plain `claude`: the pane shell follows the workspace default account (CLAUDE_CONFIG_DIR),
      // so `claude` there runs as that account even if the user picked Utama
      .map((x) => ({ label: `claude · ${x.account.name}`, kind: x.status.email ?? "", insert: `claude-${x.account.id} ` }));
    const d = draft[pane] ?? "";
    if (items.length < 2 || d.length < 2 || !"claude".startsWith(d)) return;
    // this pane's session that just finished (FR-56) = first row and selected, so Enter doesn't override the intent to resume
    const resume = meta[pane]?.ended?.resume;
    if (resume?.startsWith("claude")) return void (comp = { pane, items: [{ label: resume.replace(/([0-9a-f]{8})-[0-9a-f-]{27}$/, "$1…"), kind: tt("comp.lastSession"), insert: `${resume} ` }, ...items], sel: 0, auto: true });
    const def = wss.find((w) => w.id === wsIdOf(pane))?.accounts?.claude;
    comp = { pane, items, sel: Math.max(0, items.findIndex((it) => it.insert === `claude-${def} `)), auto: true };
  }
  const ghost = (pane: number) => suggestion(draft[pane] ?? "", meta[pane]?.ended?.resume, history, zhist);
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
    // autocomplete list open: ↑↓ moves, Tab/Enter picks, Esc closes, any other key = list closed
    if (comp?.pane === pane) {
      const n = comp.items.length;
      if (["ArrowDown", "ArrowUp", "Tab", "Enter", "Escape"].includes(e.key)) e.preventDefault();
      if (e.key === "ArrowDown") return void (comp.sel = (comp.sel + 1) % n);
      if (e.key === "ArrowUp") return void (comp.sel = (comp.sel + n - 1) % n);
      if (e.key === "Tab" || e.key === "Enter") return void ((draft[pane] = comp.items[comp.sel].insert), (comp = null));
      if (e.key === "Escape") return void (comp = null);
      // account suggestions stay open while the text still starts with `claude` (handled by onEditorInput)
      if (!comp.auto) comp = null;
    }
    // gray suggestion: → at the end of the text = accept
    if (e.key === "ArrowRight" && ta.selectionStart === ta.value.length && ghost(pane)) {
      e.preventDefault();
      draft[pane] = (draft[pane] ?? "") + ghost(pane);
      comp = null; // the text no longer starts with `claude`: account suggestions close so Enter runs this text
      return;
    }
    // @ list open: Tab/Enter picks, arrows move, Esc closes
    if (mention?.editor && mention.pane === pane && mentionItems.length && ["Tab", "Enter", "ArrowUp", "ArrowDown", "Escape"].includes(e.key)) {
      e.preventDefault();
      if (e.key === "Tab" || e.key === "Enter") chooseMention(mention.sel);
      else onMentionKey(pane, e.key);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.altKey) return void (e.preventDefault(), runDraft(pane));
    // Tab = autocomplete in the editor (`@…` text belongs to the agent hand-off, not completed)
    if (e.key === "Tab" && !e.shiftKey) return void (e.preventDefault(), !ta.value.startsWith("@") && complete(pane));
    if (e.key === "Escape") return void (e.preventDefault(), handOff(pane, false));
    if (e.key === "c" && e.ctrlKey) return void (e.preventDefault(), (draft[pane] = ""));
    const first = !ta.value.slice(0, ta.selectionStart).includes("\n");
    const last = !ta.value.slice(ta.selectionEnd).includes("\n");
    // ↑↓ = app session history (newest), then the user's zsh history; hi 0 = newest
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

  // FR-59: a focused pane with the editor shown = cursor to the editor
  $effect(() => {
    const f = tab?.focus;
    if (f !== undefined && editorShown(f)) setTimeout(() => document.querySelector<HTMLTextAreaElement>(`[data-editor="${f}"]`)?.focus(), 0);
  });

  // FR-57: `@` at the start of a pane prompt = list of other agents; picking one = send bar in that pane
  // editor = the list is triggered from the input editor (FR-59), not from typing in the terminal
  // picked = names already completed from this `@@` list (pick several agents at once)
  let mention = $state<{ pane: number; q: string; sel: number; editor?: boolean; inline?: boolean; picked?: string[] } | null>(null);
  let lempar = $state<{ pane: number; to: number; name: string; body: string; error: string } | null>(null);
  const mentionItems = $derived(
    mention
      ? targets.filter(
          (r) => !mention!.picked?.includes(r.name) && [r.name, r.agent, meta[r.pane]?.busy ?? ""].some((s) => s.toLowerCase().startsWith(mention!.q.toLowerCase())),
        )
      : [],
  );
  /// In the terminal the trigger is `@@` (a single `@` belongs to the agent: file picker in Claude/OMP). `@@` at line start = send bar;
  /// mid-sentence (e.g. "check for @@om") = replaced by `@<pane name>` for that agent
  function onLine(pane: number, line: string) {
    const m = /(?:^|\s)@@([\w.-]*)$/.exec(line);
    // always complete the name: Gembala can't be sure of the TUI prompt line content, so "line start" can't be relied on
    mention = m ? { pane, q: m[1], sel: 0, inline: true } : null;
  }
  function onMentionKey(pane: number, key: string): boolean {
    if (!mention || mention.pane !== pane || !mentionItems.length) return false;
    const n = mentionItems.length;
    if (key === "ArrowDown") mention.sel = (mention.sel + 1) % n;
    else if (key === "ArrowUp") mention.sel = (mention.sel + n - 1) % n;
    else if (key === "Tab" || key === "Enter") chooseMention(mention.sel);
    else mention = null; // Escape: close the list, the typed `@…` stays in the prompt
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
      // in the editor: name completed, message written in the same box, Enter sends
      draft[pane] = `@${r.name} `;
      mention = null;
      return;
    }
    if (mention.inline) {
      // mid-sentence in an agent prompt: `@…` replaced by the full pane name; the agent uses `gembala send` (FR-9 hint)
      // `@@q` → `@name ` (one `@` left); the next pick from the same list = `@name2 ` after it
      const picked = mention.picked ?? [];
      // OpenCode opens its own mention menu on `@` and keeps it open, so the next Enter picks `@general` (9 Nov): both `@`
      // go and the bare name is written (the hint tells agents `shell-N` names are Gembala panes)
      const at = (meta[pane]?.busy ?? meta[pane]?.agent) === "opencode" ? "" : "@";
      const cut = q.length + (at ? 1 : 2);
      const text = picked.length ? `${at}${r.name} ` : "\x7f".repeat(cut) + r.name + " ";
      call("pane.write", { pane, data: b64(new TextEncoder().encode(text)) });
      // the tracked line changes too, so the next @@ list reads the right text
      tiles[pane]?.setLine((l) => (picked.length ? l + text : l.slice(0, -cut) + `${r.name} `));
      // the list stays open without the ones already picked: Tab/Enter adds an agent, Esc or keep typing = close
      const next = [...picked, r.name];
      mention = targets.some((t) => !next.includes(t.name)) ? { pane, q: "", sel: 0, inline: true, picked: next } : null;
      return;
    }
    // `@@…` already sent to the TUI is deleted again with backspace
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
  // TECH 20: [ui] language; "system" uses the macOS list from Rust (WKWebView reports a single language)
  let sysLangs = $state<string[]>([...navigator.languages]);
  invoke<string[]>("system_lang").then((l) => void (l?.length && (sysLangs = l)), () => {});
  $effect(() => {
    i18n.lang = resolveLang(ui.language ?? "system", sysLangs);
    document.documentElement.lang = i18n.lang;
  });
  const tiles: Record<number, Tile> = $state({});
  let error = $state("");
  let box = $state<HTMLDivElement>();

  const lines = $derived(tab && !zoomed ? dividers(tab.tree) : []);
  const uid = () => Math.random().toString(36).slice(2, 10);
  const base = (p: string | null | undefined) => (p ? p.split("/").filter(Boolean).at(-1) ?? "/" : "");
  const home = (p: string | null | undefined) => (p ?? "").replace(/^\/Users\/[^/]+/, "~");
  /// pane head and bottom bar: last two folders only (`~/…/gembala/app`, Gembala.pen 02); full path in the tooltip
  const short = (p: string | null | undefined) => {
    const a = home(p).split("/");
    return a.length > 4 ? [a[0], "…", ...a.slice(-2)].join("/") : a.join("/");
  };
  /// FR-52 tab subtitle: only what differs from the workspace. Folder = workspace root → empty; otherwise just the folder name
  /// (Gembala.pen 02: `main · requirements`); full path in the tooltip
  const where = (cwd: string | null | undefined, root: string) => {
    const c = home(cwd);
    return !c || c === root ? "" : base(c);
  };
  /// FR-52: default title = agent running in the focused pane, then the folder (`~` for home)
  const title = (t: Tab) => {
    const m = meta[t.focus];
    if (t.title) return t.title;
    if (labels[t.focus]) return labels[t.focus]; // FR-69
    if (m?.busy && AGENTS.includes(m.busy)) return m.busy;
    return home(m?.cwd) === "~" ? "~" : base(m?.cwd) || "~";
  };
  /// Global tab order for ⌘1–⌘9
  const order = $derived(wss.flatMap((w, wi) => w.tabs.flatMap((t, ti) => t.parent ? [] : [{ wi, ti, t }])));

  async function createPane(opts: Record<string, unknown> = {}): Promise<number> {
    const profile = (opts.profile as string) ?? "shell";
    const agent = cfg?.values.profiles.find((p) => p.id === profile)?.agent ?? "shell";
    // FR-53: Claude Code accepts a session id from outside; used again when the pane is rerun
    // FR-72: the agent's own session picker = session id not known yet
    const session = opts.pick ? undefined : ((opts.session as string) ?? (agent === "claude" ? crypto.randomUUID() : undefined));
    // FR-61, C-9: account = choice when opening (or the rerun pane's account) > workspace default account > profile/Utama (daemon)
    const w = wss.find((x) => x.id === ((opts.workspace as string) ?? ws?.id));
    // shell panes follow the workspace default Claude account: a typed `claude` uses that account
    const acct = (opts.account as string | undefined) || w?.accounts?.[agent === "shell" ? "claude" : agent] || undefined;
    // FR-13: the workspace brief goes into the launch hint of every agent in that workspace
    const brief = agent !== "shell" ? w?.brief : undefined;
    const r = await call("pane.create", { workspace: ws?.id, ...opts, session, account: acct, brief });
    // FR-13: a read-only role on an agent without tool limits = only a hint, flagged in the pane head
    const roleWarn = ["koordinator", "planner", "reviewer"].includes(opts.role as string) && agent !== "claude";
    meta[r.pane] = { profile, cwd: null, agent, name: r.name, aliases: r.aliases, session, sig: sig(profile), acct, roleWarn, hist: r.hist, saver: r.saver === true };
    return r.pane;
  }

  /// FR-79: same as the daemon: the old name becomes this pane's alias, the new name is no longer another pane's alias
  function renamed(pane: number, r: { name: string; aliases: string[] }) {
    for (const [k, m] of Object.entries(meta)) if (+k !== pane && m.aliases?.includes(r.name)) m.aliases = m.aliases.filter((a) => a !== r.name);
    if (meta[pane]) Object.assign(meta[pane], { name: r.name, aliases: r.aliases });
  }
  /// FR-79: a replacement pane takes the old pane's name (if no other pane uses it) and carries its aliases; layout.json is untrusted
  const oldNames = (m?: Meta) => ({ name: m?.name || undefined, aliases: Array.isArray(m?.aliases) ? m.aliases : [] });

  const sig = (id: string) => JSON.stringify(cfg?.values.profiles.find((p) => p.id === id) ?? null);

  /// FR-51: a pane whose profile changed is replaced by a new pane from the latest profile in the same spot
  const relaunching = new Set<number>(); // the old pane being replaced: its pane.exit doesn't close the tab
  async function relaunch(old: number) {
    const m = meta[old];
    if (!m || relaunching.has(old)) return;
    relaunching.add(old);
    // close first: the old and new agent must not use the same session at once
    await call("pane.close", { pane: old }).catch(() => {});
    await new Promise((r) => setTimeout(r, 400));
    const id = await createPane({
      workspace: wsIdOf(old),
      profile: m.profile === "shell" ? undefined : m.profile,
      cwd: m.cwd ?? undefined,
      session: m.session,
      resume: AGENTS.includes(m.agent),
      account: m.owner?.id ?? m.acct, // FR-61: a session owned by another account resumes in its owning account
      history: m.hist, // FR-62: same spot, same history (e.g. "Resume session" on a restored shell keeps its old log)
      ...oldNames(m),
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
    if (!meta[pane] || dead[pane]) return; // every pane: the branch also becomes the tab subtitle (FR-52)
    const g = await call("git.status", { pane }).catch(() => null);
    if (g && meta[pane]) meta[pane].git = g;
  }

  async function openChanges(pane: number) {
    if (changes) return void (changes = null);
    loadChanges(pane);
  }

  /// FR-23/FR-26: the Ship card in the flow view asks for the panel, not the toggle (the panel may already be open on
  /// another pane). Depends on that pane's folder, which is the run folder.
  function showChanges(pane: number) {
    if (changes && changes.pane === pane) return void refreshGit(pane);
    loadChanges(pane);
  }

  /// FR-22: the docked panel follows the focused pane; the old diff stays until the new one arrives
  async function loadChanges(pane: number) {
    const same = changes?.pane === pane;
    // FR-23: an open card (commit message being typed) survives the refresh after an agent's turn
    changes = { pane, diff: same ? changes!.diff : null, turns: same ? changes!.turns : [], turn: same ? changes!.turn : null, act: same ? changes!.act : null };
    refreshGit(pane);
    const turns = await call("git.turns", { pane }).catch(() => []);
    if (changes?.pane !== pane) return;
    changes.turns = turns;
    if (changes.turn != null && !turns.some((t: any) => t.i === changes!.turn)) changes.turn = null;
    const diff = await (changes.turn == null ? call("git.diff", { pane }) : call("git.turn_diff", { pane, i: changes.turn })).catch(() => ({ repo: false }));
    if (changes?.pane === pane) changes.diff = diff;
  }
  /// FR-70: pick a turn (null = everything since the commit)
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
    if (r.error) return say(tt("turn.checkFailed", { why: errText(r.error, r.args) }));
    if (!r.files.length) return say(tt("turn.same"));
    if (changes?.pane === pane) changes.ask = { i, ...r };
  }
  async function doRestore() {
    const c = changes;
    if (!c?.ask) return;
    const r = await call("git.turn_restore", { pane: c.pane, i: c.ask.i }).catch((e) => ({ error: String(e) }));
    c.ask = null;
    say(r.error ? tt("turn.restoreFailed", { why: errText(r.error, r.args) }) : ttn("turn.restored", r.restored));
    pickTurn(null);
  }
  /// FR-23: confirmation card for restoring one file / committing files; the daemon names other panes in the repo
  async function askFiles(op: "restore" | "commit", paths: string[]) {
    if (!changes) return;
    const pane = changes.pane;
    const r = await call("git.file_preview", { pane, paths }).catch((e) => ({ error: String(e) }));
    if (r.error) return say(errText(r.error));
    if (changes?.pane === pane) changes.act = { op, ...r, message: "" };
  }
  async function doFiles() {
    const c = changes;
    const a = c?.act;
    if (!c || !a || (a.op === "commit" && !a.message.trim())) return;
    const paths = a.files.map((f: any) => f.path);
    const r = await (a.op === "restore"
      ? call("git.file_restore", { pane: c.pane, path: paths[0] })
      : call("git.file_commit", { pane: c.pane, paths, message: a.message })
    ).catch((e) => ({ error: String(e) }));
    if (r.error) return void ((a.error = r.error), (a.args = r.args)); // card stays: e.g. a hook said why
    c.act = null;
    say(a.op === "restore" ? tt("chg.restored", { file: paths[0] }) : tt("chg.committed", { commit: r.commit }));
    loadChanges(c.pane);
  }
  let panelW = $state(460); // FR-22: changes panel width, saved in the layout
  $effect(() => {
    const f = tab?.focus;
    if (changes && f !== undefined && f !== changes.pane && !dead[f]) loadChanges(f);
  });

  /// FR-14: text is sent as bracketed paste + one Enter. ESC, C0 controls other than newline/tab,
  /// and C1 controls are dropped so the content can't close the paste early.
  async function sendRich() {
    const r0 = rich;
    if (!r0) return;
    const files = (r0.files ?? []).map(quote).join(" ");
    let text = [r0.text, files].filter(Boolean).join(" ");
    // FR-15: `@file:` / `@pane:` = the daemon appends their content as labeled blocks (checks access, redacts, 32 KB)
    if (/(^|\s)@(file|pane):\S/.test(r0.text)) {
      r0.error = undefined;
      const r = await call("ctx.compose", { pane: r0.pane, text }).catch((e) => ({ error: String(e) }));
      if (r.error) return void ((r0.error = r.error), (r0.args = r.args)); // stays open, nothing sent
      text = r.body;
    }
    const clean = text.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "");
    const bytes = new TextEncoder().encode(`\x1b[200~${clean}\x1b[201~\r`);
    call("pane.write", { pane: r0.pane, data: b64(bytes) });
    if (rich === r0) rich = null;
  }

  /// autofocus loses to the terminal that held focus (same as the palette): focus explicitly
  async function openRich(pane: number) {
    rich = { pane, text: "" };
    await tick();
    document.querySelector<HTMLTextAreaElement>(".rich textarea")?.focus();
  }
  const addRichFiles = (paths: string[]) => rich && (rich.files = [...new Set([...(rich.files ?? []), ...paths])]);

  /// FR-15: the real path (symlinks resolved), quoted, pasted into the prompt as bracketed paste, no Enter
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

  /// FR-101: harness stages are separate helper tabs under the tab that started the run.
  function adoptRun(snap: Snap) {
    const wi = wss.findIndex((w) => w.id === snap.workspace);
    // FR-86: a finished run's panes are closing; a late snapshot (the gate reply) must not bring them back
    if (wi < 0 || snap.state === "done") return;
    const w = wss[wi];
    const placed = new Set(wss.flatMap((x) => x.tabs.flatMap((t) => panes(t.tree))));
    const stages = stagePanes(snap);
    const found = w.tabs.find((t) => stages.some((s) => panes(t.tree).includes(s.pane)))?.parent;
    const parent = [runOwner[snap.run], found, newTask && wi === at.ws ? (owner ?? tab)?.id : undefined].find((id) => id && w.tabs.some((t) => t.id === id && !t.parent));
    if (parent) runOwner[snap.run] = parent;
    for (const sp of stages) {
      if (placed.has(sp.pane) || dead[sp.pane]) continue;
      const agent = sp.agent || "shell";
      meta[sp.pane] ??= { profile: agent, cwd: snap.cwd, agent, name: sp.name, status: "idle", saver: snap.saver };
      const child: Tab = { id: uid(), title: sp.name, tree: { pane: sp.pane }, focus: sp.pane, parent };
      w.tabs.push(child);
      placed.add(sp.pane);
    }
  }
  function onRun(snap: Snap) {
    runs[snap.workspace] = snap;
    adoptRun(snap);
  }
  /// banner for the moments that need you (a stage waiting on a permission already has one from its pane's status)
  function runNotice(p: { run?: string; workspace: string; notice: string; step: number | null }) {
    // FR-74 (4): switches in Pengaturan › Harness; a stage asking for permission is already the pane's own notice (FR-40),
    // so it only comes from here when that one is off
    const h = cfg?.values.harness;
    const on = p.notice === "gate" ? h?.notify_gate !== false : p.notice === "blocked" ? h?.notify_blocked !== false && cfg?.values.notify.on_blocked === false : h?.notify_done !== false;
    if (!on) return;
    const snap = runs[p.workspace];
    const ps = snap ? stagePanes(snap) : [];
    const pane = (ps.find((x) => x.step === p.step) ?? ps[ps.length - 1])?.pane;
    // FR-87: gate = "waiting for your OK", halted = "failed"; the harness switches above decide, not [notify]'s
    const kind: NoticeKind = p.notice === "gate" ? "approve" : p.notice === "blocked" ? "permission" : p.notice === "halted" ? "failed" : "done";
    notice(pane, kind, { force: true, bell: false, run: snap?.name });
  }
  /// FR-73 manual: one agent pane in the folder, the task as its first prompt. If the agent isn't ready to take it,
  /// the task lands in Rich Input so nothing is lost.
  async function startManual(o: { agent: string; task: string; cwd: string }) {
    newTask = false;
    const id = await newTab(o.agent, at.ws, undefined, { cwd: o.cwd });
    await firstMessage(id, o.task);
  }
  const absPath = (p: string) => (p.startsWith("~") ? homePath + p.slice(1) : p);

  /// FR-38: confirm when there's a process other than an idle shell
  /// pane the daemon no longer has (e.g. lost across a restart): null, so closing just drops it from its tab
  const paneInfo = (pane: number) =>
    call("pane.info", { pane }).catch((e) => {
      if (String(e).includes("no_such_pane")) return null;
      throw e;
    });
  async function askClose(pane: number) {
    if (dead[pane]) return onexit(pane);
    const info = await paneInfo(pane);
    if (!info) return onexit(pane);
    if (info.busy) confirm = { pane, name: info.name ?? "proses", close: [pane], what: "pane" };
    else await call("pane.close", { pane });
  }

  /// FR-38: closing a tab = closing all its panes, one confirmation listing the processes still running
  async function closeTabs(wi: number, ts: Tab[]) {
    ts = [...new Set(ts.flatMap((t) => [t, ...wss[wi].tabs.filter((x) => x.parent === t.id)]))];
    const ps = ts.flatMap((t) => panes(t.tree));
    const known = await Promise.all(ps.filter((p) => !dead[p]).map((p) => paneInfo(p).then((i) => ({ p, i }))));
    ps.filter((p) => dead[p]).concat(known.filter((x) => !x.i).map((x) => x.p)).forEach(onexit);
    const infos = known.filter((x) => x.i);
    const live = infos.map((x) => x.p);
    const busy = infos.filter((x) => x.i.busy);
    if (!busy.length) return live.forEach((p) => call("pane.close", { pane: p }).catch(() => {}));
    const t = ts.find((t) => panes(t.tree).includes(busy[0].p))!;
    at = { ws: wi, tab: wss[wi].tabs.indexOf(t) };
    zoomed = false;
    confirm = { pane: t.focus, name: busy.map((x) => x.i.name ?? "proses").join(", "), close: live, what: ts.length > 1 ? `${ts.length} tab` : "tab" };
  }

  /// Pane exits: gone from its tab; an empty tab closes; a workspace may be empty (FR-44).
  async function onexit(pane: number) {
    if (relaunching.has(pane)) return;
    if (confirm?.pane === pane) confirm = null;
    withdrawTitles(settle(nst, pane));
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
        for (const child of w.tabs.filter((x) => x.parent === t.id)) child.parent = undefined;
        if (compareId === t.id) compareId = "";
        if (at.ws === wi && at.tab >= ti) at.tab = Math.max(0, at.tab - 1);
      }
      if (at.ws === wi) zoomed = false;
      if (meta[pane]?.pastes?.length) invoke("forget_paste", { paths: meta[pane].pastes }).catch(() => {});
      delete meta[pane];
      delete dead[pane];
      return;
    }
  }

  /// FR-45: a dead pane is replaced by a new pane from the same profile and folder; `shell` = shell only,
  /// its old agent recorded in `prev` for the "Resume session" chip
  async function restart(old: number, shell = false) {
    const m = meta[old];
    const id = await createPane({
      profile: shell || m?.profile === "shell" ? undefined : m?.profile,
      cwd: m?.cwd ?? undefined,
      session: shell ? undefined : m?.session,
      resume: !shell && AGENTS.includes(m?.agent ?? ""), // FR-53: resume this pane's conversation
      workspace: wsIdOf(old),
      history: m?.hist ?? m?.from, // FR-62: same key = same history file, however many daemons stopped in between
      account: shell ? undefined : m?.acct,
      ...oldNames(m),
    });
    if (shell && m && AGENTS.includes(m.agent)) meta[id].prev = { profile: m.profile, agent: m.agent, session: m.session, acct: m.acct };
    if (shell && m?.ended) meta[id].ended = m.ended; // FR-56: the finished session card carries over to the replacement shell
    // FR-68: a typed agent that stopped with the daemon = resume card from its session id (command passes the safe pattern)
    else if (shell && m && !AGENTS.includes(m.agent) && m.busy && m.session) {
      // same as the daemon (FR-61): session owned by another account = `claude-<owner>`, otherwise the process account; plain `claude` = shell account
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

  /// FR-45: "Resume session" chip: the shell is replaced by its old agent with the same session id (FR-53)
  function resumePrev(pane: number) {
    const { prev, ...m } = meta[pane];
    if (!prev) return;
    meta[pane] = { ...m, ...prev };
    relaunch(pane);
  }

  /// Criterion 17: daemon force-killed = every pane shows as a dead pane (rerun button),
  /// then a new daemon is launched by the next call.
  /// A dropped connection doesn't have to mean the daemon is gone: the panes keep running inside it (a daemon before
  /// 0.1.9 shut the socket of a UI client that fell behind during a big burst, TECH 16; now it sends `pane.lag`). So
  /// ask who the daemon is first — same boot id and the pane still listed = re-attach it; a different boot id = the
  /// panes stopped with it.
  let lost = false;
  async function onLost() {
    if (lost) return;
    lost = true;
    confirm = null;
    changes = null;
    rich = null;
    await new Promise((r) => setTimeout(r, 500));
    // the next call reconnects (no answer = the daemon is really gone and this says nothing)
    let alive = new Set<number>();
    let same = false;
    try {
      same = (await call("daemon.info")).boot === boot;
      alive = new Set<number>(await call("pane.list"));
    } catch {
      same = false;
    }
    let next = Math.min(-1, ...Object.keys(meta).map(Number), ...Object.keys(dead).map(Number)) - 1;
    const again: number[] = [];
    for (const w of wss)
      for (const t of w.tabs)
        for (const p of panes(t.tree)) {
          if (dead[p]) continue;
          // the same daemon and the pane is still there: it outlived the connection (only its events were missed)
          if (same && alive.has(p)) {
            again.push(p);
            continue;
          }
          const id = next--;
          t.tree = replace(t.tree, p, id);
          if (t.focus === p) t.focus = id;
          meta[id] = { ...(meta[p] ?? { profile: "shell", cwd: null, agent: "", name: "" }), from: { boot, pane: p } };
          delete meta[p];
          dead[id] = true;
        }
    if (same) {
      // a new connection has no pane subscriptions: attach again (status and cwd follow from the 2 s poll)
      cfg = await call("config.get").catch(() => cfg);
      for (const p of again) tiles[p]?.reattach();
      lost = false;
      return;
    }
    try {
      boot = (await call("daemon.info")).boot;
      cfg = await call("config.get");
    } catch (e) {
      error = tt("main.daemonRestartFailed", { why: errText(e) });
    }
    lost = false;
  }

  /// FR-46: confirm only when a process is still running
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

  /// FR-30: pick a profile; agents not detected can still be picked (the pane shows how to install)
  async function openPicker() {
    const agents: Agent[] = await call("agents.detect", {}).catch(() => []);
    const missing = new Set(agents.filter((a) => !a.installed).map((a) => a.agent));
    const accs = cfg?.values.accounts ?? [];
    // FR-61: one row per account when that agent has accounts besides Utama
    const items = (cfg?.values.profiles ?? []).flatMap((p) => {
      const own = accs.filter((a) => a.agent === p.agent);
      const base = { ...p, missing: missing.has(p.agent), key: p.id, label: p.name, account: undefined as string | undefined };
      return own.length ? [{ id: "utama", name: tt("acc.main") }, ...own].map((a) => ({ ...base, key: `${p.id}:${a.id}`, label: `${p.name} · ${a.name}`, account: a.id })) : [base];
    });
    picker = { items, sel: 0 };
  }

  function pick(i: number) {
    const p = picker?.items[i];
    picker = null;
    if (p) newTab(p.id === "shell" ? undefined : p.id, at.ws, p.account);
  }

  /// FR-61: Login button = a new pane with that account + a hint card; Gembala types nothing into the terminal
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

  /// Actions from the menu (native key equivalents, see lib.rs). `⌘` belongs to the app, the rest to the terminal (FR-39).
  let last = { id: "", at: 0 };
  function run(id: string) {
    if (id === "app.quit_all") return askQuitAll();
    if (id === "app.settings") return void (settingsOpen = !settingsOpen);
    if (settingsOpen || picker || editing || rich || pal || lempar || newTask) return;
    if (confirm || quitAsk || linkAsk || wsAsk) return;
    // one key can arrive through the menu and keydown at once; don't run twice
    if (id === last.id && performance.now() - last.at < 150) return;
    last = { id, at: performance.now() };
    const [, group, arg] = id.split(".");
    const cur = order.findIndex((o) => o.wi === at.ws && o.ti === at.tab);
    if (id === "msg.drawer") return void ((drawer = !drawer), drawer && ((flow = false), refreshMsgs()));
    if (id === "msg.stop") return void call("msg.pause", { on: !msgData?.paused }).then(refreshMsgs);
    if (id === "msg.compose") {
      compose = { to: targets[0]?.pane ?? null, body: "", force: false, error: "" };
      return;
    }
    // actions without a tab: a workspace may be empty (FR-44)
    if (id === "tab.new") return void openPicker();
    if (id === "task.new") return void (newTask = true);
    if (id === "tab.sidebar") return void (sidebar = !sidebar);
    if (id === "tab.search") return void openPalette();
    if (id === "tab.prev") return goto((cur + order.length - 1) % order.length);
    if (id === "tab.next") return goto((cur + 1) % order.length);
    if (id === "tab.helper.owner") return void goOwner();
    if (id === "tab.helper.next") return void nextHelper(1);
    if (id === "tab.helper.prev") return void nextHelper(-1);
    if (id === "tab.helper.compare") return void toggleCompare();
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
      // FR-37: the focused pane swaps places with its neighbor; focus follows the moved pane
      const other = neighbor(tab.tree, tab.focus, arg as Arrow);
      if (other !== null) tab.tree = swap(tab.tree, tab.focus, other);
    }
    else if (id === "pane.zoom.toggle") zoomed = !zoomed;
    else if (id === "pane.move.newtab") moveToNewTab();
    else if (id === "pane.input.rich") openRich(tab.focus);
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
    // zoom without a sign = current size unknown; numbers match Settings > Appearance > Font size
    if (id.startsWith("term.zoom")) {
      const base = cfg?.values.ui.font_size ?? 13;
      const pz = id.startsWith("term.zoom.pane") ? (meta[tab.focus]?.zoom ?? 0) : 0;
      say(tt(pz ? "main.zoomPane" : "main.zoom", { n: Math.max(6, base + zoom + pz), base }));
    }
  }

  /// FR-39: palette = tabs, profiles, and every action with its shortcut (doubles as the shortcut list).
  /// Gembala.pen 03: titled groups, agent icons, right-aligned shortcuts, hint footer.
  type PalItem = { label: string; hint: string; kind: "tab" | "profile" | "action"; agent?: string; go: () => void };
  type PalGroup = { title: string; items: PalItem[] };
  async function openPalette() {
    // ⌘P again = close
    if (pal) return void (pal = null);
    const [keys] = await invoke<[[string, string, string][], string[]]>("shortcuts").catch(() => [[]] as any);
    pal = { q: "", sel: 0, keys };
    // autofocus can lose to a pane editor grabbing focus; without focus the palette can't be used/closed
    await tick();
    document.querySelector<HTMLInputElement>(".palette input")?.focus();
    setTimeout(() => pal && document.querySelector<HTMLInputElement>(".palette input")?.focus(), 50);
  }
  const palGroups = $derived.by((): PalGroup[] => {
    if (!pal) return [];
    // tab row icon and status use the same rules as the sidebar (FR-4/52), not a new copy
    const tabs: PalItem[] = order.map((o, i) => {
      const st = tabStatus(o.t);
      const agent = panes(o.t.tree).map(agentOf).find(Boolean) ?? "";
      // one workspace = shortcuts only (Gembala.pen 03); more than one = workspace name too, so twin tabs can be told apart
      const key = i < 9 ? `⌘${i + 1}` : "";
      return {
        label: [meta[o.t.focus]?.name ?? "", title(o.t), stText(st)].filter(Boolean).join(" · "),
        hint: wss.length > 1 ? [wss[o.wi].name, key].filter(Boolean).join(" · ") : key,
        kind: "tab",
        agent,
        go: () => goto(i),
      };
    });
    const profiles: PalItem[] = (cfg?.values.profiles ?? []).map((p) => ({
      label: p.name,
      hint: p.command || "$SHELL",
      kind: "profile",
      agent: p.agent === "shell" ? "" : p.agent,
      go: () => newTab(p.id === "shell" ? undefined : p.id),
    }));
    const actions: PalItem[] = [
      ...pal.keys.filter(([id]) => !id.startsWith("tab.goto.") && id !== "tab.search").map(([id, label, key]) => ({ label, hint: pretty(key), kind: "action" as const, go: () => run(id) })),
      { label: tt("act.worktree"), hint: tt("act.worktreeHint"), kind: "action", go: () => openWorktree() },
      { label: tt("act.template"), hint: tt("act.templateHint"), kind: "action", go: () => openTemplate() },
      ...pickable.map((p) => ({ label: tt("act.pickSessionOf", { name: p.label }), hint: tt("act.pickSessionHint"), kind: "action" as const, go: () => newTab(p.id, at.ws, p.account, { pick: true }) })),
      { label: tt("hn.newTask"), hint: "", kind: "action", go: () => (newTask = true) },
      { label: tt("rep.title"), hint: "", kind: "action", go: () => (reportDraft = { title: "", body: "" }) },
      { label: tt("act.newWs"), hint: "", kind: "action", go: addWorkspace },
      { label: editorOn ? tt("act.editorOff") : tt("act.editorOn"), hint: "", kind: "action", go: () => call("config.set", { section: "ui", key: "input_editor", value: !editorOn }).catch(() => {}) },
      { label: tt("act.renameTab"), hint: "", kind: "action", go: () => tab && (editing = { kind: "tab", id: tab.id, value: title(tab) }) },
      { label: tt("act.renameWs"), hint: "", kind: "action", go: () => ws && (editing = { kind: "ws", id: ws.id, value: ws.name }) },
      { label: tt("set.title"), hint: "⌘,", kind: "action", go: () => (settingsOpen = true) },
      { label: tt("set.diag.whatsNew"), hint: "", kind: "action", go: openWhatsNew },
      // FR-77: Atur pakai AI from the palette (recipe, Gembala settings or agent instructions)
      { label: `${tt("hn.set.aiTitle")}…`, hint: "", kind: "action", go: () => ((aiOpen = ""), (settingsSec = "harness"), (settingsOpen = true)) },
      { label: tt("act.quitAll"), hint: "", kind: "action", go: askQuitAll },
    ];
    const words = pal.q.toLowerCase().split(/\s+/).filter(Boolean);
    const hay = (it: PalItem) => `${it.label} ${it.hint}`.toLowerCase();
    // order: exact matches first, then recently used; stable sort = original order for the rest
    const rank = (it: PalItem) => {
      const r = recent.indexOf(it.label);
      return (words.every((w) => hay(it).includes(w)) ? 0 : 1000) + (r < 0 ? 100 : r);
    };
    const matched = [...tabs, ...profiles, ...actions].filter((it) => words.every((w) => fuzzy(hay(it), w))).sort((a, b) => rank(a) - rank(b));
    // a row shows once: top results first, then recently used, the rest per kind
    const top = words.length ? matched.slice(0, 3) : [];
    const rest = matched.slice(top.length);
    const recycled = rest.filter((it) => recent.includes(it.label)).sort((a, b) => recent.indexOf(a.label) - recent.indexOf(b.label));
    const left = rest.filter((it) => !recent.includes(it.label));
    const group = (title: string, items: PalItem[]) => ({ title, items });
    return [
      group(tt("pal.match"), top),
      group(tt("pal.recent"), recycled),
      group(tt("pal.tabs"), left.filter((it) => it.kind === "tab")),
      group(tt("pal.profiles"), left.filter((it) => it.kind === "profile")),
      group(tt("pal.actions"), left.filter((it) => it.kind === "action")),
    ].filter((g) => g.items.length > 0);
  });
  const palItems = $derived(palGroups.flatMap((g) => g.items));
  /// what's drawn: group title, then its rows; the selection index follows this order so ↑↓ doesn't stop at titles
  const palRows = $derived.by(() => {
    const rows: ({ kind: "head"; h: string } | { kind: "item"; it: PalItem; i: number })[] = [];
    let i = 0;
    for (const g of palGroups) {
      rows.push({ kind: "head", h: g.title });
      for (const it of g.items) rows.push({ kind: "item", it, i: i++ });
    }
    return rows;
  });
  /// word letters searched in order (gaps allowed): "spl ri" matches "split right"
  const fuzzy = (hay: string, w: string) => {
    let i = 0;
    for (const c of hay) if (c === w[i]) i++;
    return i === w.length;
  };
  // recently used palette actions (per-user convenience; storage may fail = ignored)
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
    if (editing) sidebar = true; // rename happens in the sidebar
  }

  /// Right-click menu (FR-37, FR-44, FR-52), HTML so the color submenu can be a swatch panel (Gembala.pen 08).
  /// Opened by keyboard (no pointer position) = below the element that was activated.
  let menu = $state<MenuAt | null>(null);
  function popup(e: MouseEvent, items: MItem[]) {
    e.preventDefault();
    e.stopPropagation();
    const r = (e.target as Element | null)?.getBoundingClientRect();
    menu = e.clientX || e.clientY || !r ? { x: e.clientX, y: e.clientY, items } : { x: r.left, y: r.bottom, items };
  }
  const copy = (text: string | null | undefined) =>
    text && invoke("copy_text", { text }).then(() => say(tt("common.copied")), () => say(tt("chg.copyFailed")));

  // short feedback for actions with no visible result (BRD section 11); role=status = read out by VoiceOver
  let toast = $state("");
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  function say(msg: string) {
    toast = msg;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = ""), 1800);
  }

  /// Gembala.pen 08 order. No rename shortcut exists, so none is shown; the close-pane key closes this tab
  /// only when it is the active tab with a single pane, so only then is it shown.
  async function tabMenu(e: MouseEvent, wi: number, t: Tab) {
    e.preventDefault();
    e.stopPropagation();
    const ti = wss[wi].tabs.indexOf(t);
    const others = wss.map((w, j) => [w.name, () => moveTab(wi, wss[wi].tabs.indexOf(t), j, w.tabs.length)] as MItem).filter((_, j) => j !== wi);
    const [keys] = await invoke<[[string, string, string][], string[]]>("shortcuts").catch(() => [[], []] as [[string, string, string][], string[]]);
    const closeKey = t === tab && panes(t.tree).length === 1 ? keys.find(([id]) => id === "pane.proc.close")?.[2] : "";
    popup(e, [
      [tt("menu.rename"), () => (editing = { kind: "tab", id: t.id, value: title(t) }), true, undefined, { icon: "rename" }],
      [tt("menu.color"), colorMenu(t.color, (c) => (t.color = c)), true, undefined, { icon: "color", title: tt("menu.tabColor"), hint: tt("menu.tabColorHint") }],
      [tt("menu.closeTab"), () => closeTabs(wi, [t]), true, undefined, { icon: "close", key: closeKey ? pretty(closeKey) : "" }],
      [tt("menu.closeOthers"), () => closeTabs(wi, wss[wi].tabs.filter((x) => x !== t)), wss[wi].tabs.length > 1],
      [tt("menu.closeBelow"), () => closeTabs(wi, wss[wi].tabs.slice(ti + 1)), ti < wss[wi].tabs.length - 1],
      "-",
      [tt("menu.moveToWs"), others.length ? others : () => {}, others.length > 0, undefined, { icon: "move" }],
      [tt("menu.copyFolder"), () => copy(meta[t.focus]?.cwd), true, undefined, { icon: "copy" }],
      [tt("act.worktree"), () => ((at = { ws: wi, tab: ti }), openWorktree(wi))],
      ...(meta[t.focus]?.git?.branch?.startsWith("gembala/") ? [[tt("menu.closeWorktree"), () => removeWorktree(wi, t), true, undefined, { danger: true }] as MItem] : []),
    ]);
  }

  /// FR-52: new tab from a profile in a given workspace (the + button in the workspace head)
  async function profileMenu(e: MouseEvent, wi: number) {
    e.preventDefault();
    e.stopPropagation();
    const agents: Agent[] = await call("agents.detect", {}).catch(() => []);
    const missing = new Set(agents.filter((a) => !a.installed).map((a) => a.agent));
    popup(e, [
      ...(cfg?.values.profiles ?? []).map((p) => [`${p.name}${missing.has(p.agent) ? ` (${tt("main.notInstalled")})` : ""}`, () => newTab(p.id === "shell" ? undefined : p.id, wi)] as MItem),
      "-",
      [tt("act.worktree"), () => openWorktree(wi)],
      [tt("act.template"), () => openTemplate(wi)],
      pickable.length
        ? [tt("act.pickSession"), pickable.map((p) => [p.label, () => newTab(p.id, wi, p.account, { pick: true })] as MItem)]
        : [tt("act.pickSession"), () => {}, false],
    ]);
  }

  /// Gembala.pen 13 order
  function wsMenu(e: MouseEvent, wi: number) {
    const w = wss[wi];
    // FR-61: default Claude account for new panes in this workspace (the human's choice, C-9)
    const accts = [{ id: "utama", name: tt("acc.main") }, ...(cfg?.values.accounts ?? []).filter((a) => a.agent === "claude")];
    const cur = accts.find((a) => a.id === (w.accounts?.claude ?? "utama"));
    popup(e, [
      [tt("menu.rename"), () => (editing = { kind: "ws", id: w.id, value: w.name }), true, undefined, { icon: "rename" }],
      [tt("menu.changeFolder"), () => changeRoot(w), true, undefined, { icon: "folder" }],
      [tt("menu.color"), colorMenu(w.color, (c) => (w.color = c ?? null)), true, undefined, { icon: "color", title: tt("menu.wsColor"), hint: tt("menu.wsColorHint") }],
      ...(accts.length > 1
        ? [[tt("menu.defaultAccount", { name: cur?.name ?? tt("acc.main") }), accts.map((a): MItem => [a.name, () => void (w.accounts = { ...w.accounts, claude: a.id }), true, undefined, { on: a === cur }]), true, undefined, { icon: "account" }] as MItem]
        : []),
      [w.collapsed ? tt("menu.expand") : tt("menu.collapse"), () => (w.collapsed = !w.collapsed), true, undefined, { icon: "collapse" }],
      [tt("menu.deleteWs"), () => askRemoveWorkspace(wss.indexOf(w)), wss.length > 1, undefined, { icon: "delete", danger: true }],
    ]);
  }

  /// FR-37: klik kanan terminal
  function paneMenu(e: MouseEvent, pane: number) {
    const tile = tiles[pane];
    const blk = tile?.blockAt(e.clientY); // FR-58: command block under the cursor
    popup(e, [
      [tt("tile.copy"), () => copy(tile?.selection()), !!tile?.selection()],
      [tt("menu.paste"), async () => tile?.paste(await invoke<string>("paste_text").catch(() => ""))],
      "-",
      [tt("menu.copyBlockCmd"), () => copy(blk?.cmd), !!blk?.cmd],
      [tt("menu.copyBlockOut"), () => copy(blk?.output), !!blk?.output],
      // FR-58: the block's command goes into the editor to edit and run again (like Warp)
      [tt("menu.reuse"), () => blk && ((draft[pane] = blk.cmd), document.querySelector<HTMLTextAreaElement>(`[data-editor="${pane}"]`)?.focus()), !!blk?.cmd && editorShown(pane)],
      "-",
      [tt("menu.splitRight"), () => splitFocused("row")],
      [tt("menu.splitDown"), () => splitFocused("col")],
      [zoomed ? tt("menu.unzoom") : tt("menu.zoom"), () => (zoomed = !zoomed), zoomed || (!!tab && panes(tab.tree).length > 1)],
      [tt("menu.clear"), () => tile?.clearScreen()],
      "-",
      [tt("menu.closePane"), () => askClose(pane)],
    ]);
  }

  /// FR-44: a new root folder applies to new tabs; existing tabs stay in their folders
  async function changeRoot(w: Ws) {
    const path = await invoke<string | null>("pick_folder");
    if (path) w.root = home(path);
  }

  /// `at` keeps pointing at the same tab after the order changes
  function keepAt(change: () => void) {
    const tid = tab?.id;
    const wid = ws?.id;
    change();
    const wi = wss.findIndex((w) => w.tabs.some((t) => t.id === tid));
    at = wi >= 0 ? { ws: wi, tab: wss[wi].tabs.findIndex((t) => t.id === tid) } : { ws: Math.max(0, wss.findIndex((w) => w.id === wid)), tab: 0 };
  }

  /// FR-52: tab moves position or workspace; the daemon is told its panes' new workspace (access rules SEC-4/21)
  function moveTab(wi: number, ti: number, to: number, index: number) {
    keepAt(() => {
      const [t] = wss[wi].tabs.splice(ti, 1);
      const children = t.parent ? [] : wss[wi].tabs.filter((x) => x.parent === t.id);
      for (const child of children) wss[wi].tabs.splice(wss[wi].tabs.indexOf(child), 1);
      if (t.parent && wi !== to) t.parent = undefined;
      wss[to].tabs.splice(Math.min(index, wss[to].tabs.length), 0, t);
      wss[to].tabs.splice(wss[to].tabs.indexOf(t) + 1, 0, ...children);
      if (wi !== to) for (const moved of [t, ...children]) for (const p of panes(moved.tree)) if (!dead[p]) call("pane.workspace", { pane: p, workspace: wss[to].id }).catch(() => {});
    });
  }

  /// FR-44/52: drag tabs and workspaces in the sidebar (pointer, not HTML drag: WKWebView + Tauri file drop).
  /// Pen screen 09: the source row stays in place (faded), its copy follows the cursor, a blue insertion line where it drops;
  /// other rows don't shift. Target = nearest row by Y (gaps between rows count). Esc = cancel.
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
        // the row copy is wrapped in an empty copy of its .wsg, so the block color and accent line still apply
        const g = row.parentElement!.getBoundingClientRect();
        ghost = document.createElement("div");
        ghost.className = "dragghost"; // not "ghost": that global class overrides editor suggestions and block buttons
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
        // workspace block = .wsg card; insert before/after the nearest card
        const gs = [...side.querySelectorAll<HTMLElement>(".wsg")].map((el) => el.getBoundingClientRect());
        const j = nearest(gs, y);
        const after = y > (gs[j].top + gs[j].bottom) / 2;
        over = { wi: j + +after, ti: -1 };
        top = after ? gs[j].bottom + 1 : gs[j].top - 4;
        box = gs[j];
      } else {
        // every sidebar row in DOM order: workspace head, tabs, "+ Tab" of an empty workspace
        const els = [...side.querySelectorAll<HTMLElement>("[data-k]")];
        const rs = els.map((el) => el.getBoundingClientRect());
        const k = nearest(rs, y);
        const el = els[k];
        const head = el.dataset.ti === undefined;
        // workspace head (also collapsed, or "+ Tab" of an empty workspace) = drops at that workspace's first position
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
      window.addEventListener("pointerup", up); // the click ending a cancelled drag is still swallowed
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
      // the click ending a drag doesn't also move the tab (click comes right after pointerup, if at all)
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
    const move = (m: PointerEvent) => (sideW = Math.min(480, Math.max(160, m.clientX - 44)));
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    e.preventDefault();
  }

  /// FR-56: Resume button. Agent profile pane: rerun its profile with the session id (hook flags stay);
  /// agent typed by hand: the resume command is typed into the idle shell.
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

  /// Confirm cards, the profile picker, and ⌘⇧↩ capture keys; the rest belongs to the terminal.
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
      // Esc in a Settings field = cancel the field, not close Settings
      const typing = document.activeElement instanceof HTMLInputElement || document.activeElement instanceof HTMLTextAreaElement;
      if (e.key === "Escape" && !typing) {
        settingsOpen = false;
        e.preventDefault();
      }
      return; // controls in Settings take keys as usual
    }
    if (rich || pal || lempar) return; // the Rich Input textarea, palette, and hand-off bar handle their own keys
    if (compose) {
      if (e.key === "Escape") (compose = null), stop(e);
      return;
    }
    if (drawer && e.key === "Escape") {
      drawer = false;
      return stop(e);
    }
    if (bell && e.key === "Escape") {
      bell = false;
      return stop(e);
    }
    if (changes && e.key === "Escape") {
      if (changes.act) changes.act = null;
      else changes = null;
      return stop(e);
    }
    if (editing) {
      if (e.key === "Escape") {
        editing = null;
        stop(e);
      }
      return; // input boxes in the sidebar take keys as usual
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
    // FR-97: Esc = Full mode, never asked again (user decision 10 Nov 2026); Enter does nothing, a choice is a click
    if (askMode) {
      if (e.key === "Escape") setMode("full");
      return stop(e);
    }
    if (welcome && e.key === "Escape") {
      welcome = null;
      return stop(e);
    }
    // FR-103: Esc = Oke (mockup pen: one main button, Esc and a click outside do the same)
    if (whatsNew?.length && !whatsNewBlocked && e.key === "Escape") {
      dismissWhatsNew();
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
    // WKWebView eats ⌘⇧↩ before the menu gets it
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
      // FR-3: unique names checked by the daemon; failure = old name stays
      const pane = +editing.id;
      editing = null;
      call("pane.rename", { pane, name: v.toLowerCase() })
        .then((r) => renamed(pane, r))
        .catch((e) => (renameError = e === "name_taken" ? tt("main.nameTaken", { name: v }) : tt("main.nameBad")));
      return;
    }
    if (editing.kind === "tab") {
      const t = wss.flatMap((w) => w.tabs).find((t) => t.id === editing!.id);
      if (t) t.title = v; // empty = back to the folder name
    } else {
      const w = wss.find((w) => w.id === editing!.id);
      if (w && v) w.name = v;
    }
    editing = null;
  }

  /// FR-37: the focused pane moves to a new tab in the same workspace
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

  /// FR-37: drag a pane head (like Warp). Over another pane: left/right/top/bottom zone = split on that side, middle =
  /// swap places; over another tab in the sidebar = move to that tab. Esc = cancel. Same pointer pattern as `startDrag`.
  function startPaneDrag(e: PointerEvent, from: Tab, pane: number) {
    if (e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
    e.preventDefault(); // without this the drag also selects head and toolbar text
    const [x0, y0] = [e.clientX, e.clientY];
    let ghost: HTMLElement | null = null;
    let esc = false;
    let drop: (() => void) | null = null;
    const move = (m: PointerEvent) => {
      if (esc || (!paneDrag && Math.hypot(m.clientX - x0, m.clientY - y0) < 5)) return;
      if (!ghost) {
        ghost = document.createElement("div");
        ghost.className = "dragghost panedrag";
        ghost.textContent = meta[pane]?.name ?? "pane";
        document.body.append(ghost);
      }
      Object.assign(ghost.style, { left: `${m.clientX + 12}px`, top: `${m.clientY + 12}px` });
      drop = null;
      let zone: NonNullable<typeof paneDrag>["zone"] = null;
      const row = document.elementFromPoint(m.clientX, m.clientY)?.closest<HTMLElement>(".tab[data-ti]");
      const g = box?.getBoundingClientRect();
      if (row) {
        const [wi, ti] = [+row.dataset.wi!, +row.dataset.ti!];
        const to = wss[wi]?.tabs[ti];
        if (to && to !== from) {
          const r = row.getBoundingClientRect();
          zone = { left: r.left, top: r.top, width: r.width, height: r.height, label: tt("drag.toTab") };
          drop = () => movePaneToTab(from, pane, to);
        }
      } else if (g) {
        // target pane = the box in this tab holding the cursor (grid fractions)
        const [fx, fy] = [(m.clientX - g.left) / g.width, (m.clientY - g.top) / g.height];
        const hit = [...rects(from.tree)].find(([, r]) => fx >= r.x && fx < r.x + r.w && fy >= r.y && fy < r.y + r.h);
        if (hit && hit[0] !== pane) {
          const [target, r] = hit;
          const [L, T, W, H] = [g.left + r.x * g.width, g.top + r.y * g.height, r.w * g.width, r.h * g.height];
          const [u, v] = [(m.clientX - L) / W, (m.clientY - T) / H];
          // nearest side when within 25% of the edge; otherwise the middle
          const side = (
            [["left", u], ["right", 1 - u], ["up", v], ["down", 1 - v]] as [Arrow, number][]
          ).reduce((a, b) => (b[1] < a[1] ? b : a));
          if (side[1] < 0.25) {
            const s = side[0];
            const half = s === "left" || s === "right" ? { width: W / 2, height: H } : { width: W, height: H / 2 };
            zone = { left: s === "right" ? L + W / 2 : L, top: s === "down" ? T + H / 2 : T, ...half,
              label: tt(`drag.${s}`) };
            drop = () => (from.tree = place(from.tree, pane, target, s));
          } else {
            zone = { left: L, top: T, width: W, height: H, label: tt("drag.swap") };
            drop = () => (from.tree = swap(from.tree, pane, target));
          }
        }
      }
      paneDrag = { zone };
    };
    const key = (k: KeyboardEvent) => {
      if (k.key !== "Escape" || !paneDrag) return;
      k.preventDefault();
      k.stopPropagation();
      esc = true;
      stop();
      window.addEventListener("pointerup", up);
    };
    const stop = () => {
      window.removeEventListener("pointermove", move, true);
      window.removeEventListener("pointerup", up, true);
      window.removeEventListener("pointercancel", stop, true);
      window.removeEventListener("keydown", key, true);
      ghost?.remove();
      const d = paneDrag;
      paneDrag = null;
      return d;
    };
    const up = () => {
      const d = stop();
      window.removeEventListener("pointerup", up);
      if (!d && !esc) return;
      const eat = (c: MouseEvent) => c.stopPropagation();
      window.addEventListener("click", eat, true);
      setTimeout(() => window.removeEventListener("click", eat, true), 0);
      if (d && drop) {
        drop();
        if (from.tree && panes(from.tree).includes(pane)) from.focus = pane;
      }
    };
    window.addEventListener("pointermove", move, true);
    window.addEventListener("pointerup", up, true);
    window.addEventListener("pointercancel", stop, true);
    window.addEventListener("keydown", key, true);
  }

  /// FR-37: pane moves to another tab (split right of that tab's focused pane); the empty source tab closes.
  /// Different workspace = the daemon is told (access rules SEC-4/21, same as `moveTab`).
  function movePaneToTab(from: Tab, pane: number, to: Tab) {
    const rest = remove(from.tree, pane);
    const src = wss.findIndex((w) => w.tabs.includes(from));
    if (rest) {
      from.tree = rest;
      if (from.focus === pane) from.focus = panes(rest)[0];
    } else wss[src].tabs.splice(wss[src].tabs.indexOf(from), 1);
    to.tree = split(to.tree, to.focus, "row", pane);
    to.focus = pane;
    const wi = wss.findIndex((w) => w.tabs.includes(to));
    if (wi !== src && !dead[pane]) call("pane.workspace", { pane, workspace: wss[wi].id }).catch(() => {});
    at = { ws: wi, tab: wss[wi].tabs.indexOf(to) };
    zoomed = false;
  }

  /// FR-44: workspace = one project folder picked through a dialog; name = folder name (rename with a double click);
  /// its new tabs start in that folder
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

  /// layout.json content is untrusted: wrong shapes are dropped, the rest is used.
  function sanitize(saved: any): Ws[] {
    if (!Array.isArray(saved?.workspaces)) return [];
    const out: Ws[] = saved.workspaces
      .filter((w: any) => typeof w?.id === "string" && Array.isArray(w.tabs))
      .map((w: any) => ({
        id: w.id,
        name: String(w.name ?? "workspace").slice(0, 60),
        root: String(w.root ?? "~").slice(0, 500),
        collapsed: w.collapsed === true,
        // FR-44: block color; old workspaces without a color get one below (null = deliberately no color)
        color: COLORS.some((c) => c === w.color) ? w.color : w.color === null ? null : undefined,
        // FR-61: only valid account ids (checked again by the daemon when opening a pane)
        brief: typeof w.brief === "string" && w.brief.trim() ? w.brief.slice(0, 4000) : undefined,
        accounts: Object.fromEntries(Object.entries(w.accounts ?? {}).filter(([k, v]) => /^[a-z]+$/.test(k) && typeof v === "string" && /^[a-z0-9-]{1,32}$/.test(v))),
        tabs: w.tabs
          .filter((t: any) => typeof t?.id === "string" && valid(t.tree))
          .map((t: any) => ({
            id: t.id,
            title: String(t.title ?? "").slice(0, 60),
            tree: t.tree,
            focus: panes(t.tree).includes(t.focus) ? t.focus : panes(t.tree)[0],
            color: COLORS.some((c) => c === t.color) ? t.color : undefined,
            parent: typeof t.parent === "string" ? t.parent : undefined,
            helpersOpen: t.helpersOpen === true,
            doneOpen: t.doneOpen === true,
          })),
      }));
    for (const w of out) for (const t of w.tabs) if (t.parent && !w.tabs.some((x) => x.id === t.parent && !x.parent)) t.parent = undefined;
    for (const w of out) if (w.color === undefined) w.color = nextColor(out);
    return out;
  }

  /// FR-45 restore; FR-43 first open
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
            // ids from another run can clash with new ids: dead panes are always negative
            const id = next--;
            t.tree = replace(t.tree, p, id);
            if (t.focus === p) t.focus = id;
            // a pane still dead in the saved layout (negative id) never ran in saved.boot: its history stays where `from` points
            const from = p < 0 && oldMeta[p]?.from ? oldMeta[p].from : { boot: saved.boot, pane: p };
            meta[id] = { ...(oldMeta[p] ?? { profile: "shell", cwd: null, agent: "", name: "" }), from };
            dead[id] = true;
          }
      wss = restored;
      // layout.json is untrusted: the FR-56 card stays, its resume command only when it matches a daemon-made shape
      for (const m of Object.values(meta)) if (m.ended) m.ended = { resume: safeResume(m.ended.resume) };
      // FR-45: dead panes are replaced by new shells right away (negative id first so it doesn't clash with new run ids)
      for (const d of Object.keys(dead).map(Number)) await restart(d, true);
      // live panes not in the layout (e.g. the layout failed to save) still show
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
    freshInstall = true; // FR-103: this is the "new install" the welcome card below belongs to
    welcome = await call("agents.detect", {}).catch(() => []);
  }

  // FR-65: attention queue = every pane needing permission, longest waiting first
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
    return s < 60 ? tt("tile.secs", { n: s }) : s < 3600 ? tt("tile.mins", { n: Math.floor(s / 60) }) : tt("tile.hours", { n: Math.floor(s / 3600) });
  };
  /// FR-65: go to that tab and pane (click a queue row, Cmd+Shift+U)
  // FR-77 (H5, pen 20/21): Atur pakai AI. A visible agent tab gets the brief; the agent writes only the draft file,
  // checked here every 3 s until it exists; the review and Approve live in Settings › Harness
  let aiDrafts = $state<AiDraft[]>([]);
  let aiOpen = $state<string | null>(null); // palette / section header: Settings opens on Atur pakai AI
  async function startAi(r: AiAsk) {
    settingsOpen = false;
    aiOpen = null;
    // AGENTS.md / CLAUDE.md: the agent works in the workspace folder, where the file lives
    const pane = await newTab(r.profile, at.ws, undefined, r.target === "agent" ? { cwd: wss[at.ws]?.root } : {}).catch(() => 0);
    if (!pane) return;
    const t = wss[at.ws]?.tabs[at.tab];
    if (t) t.title = tt("hn.aiTab");
    if (r.target === "agent") {
      const ok = await call("agent_file.brief", { pane, file: r.file, ask: r.ask }).then((x) => !x?.error, () => false);
      if (ok) say(tt("hn.agentAsked", { file: r.file }));
      return;
    }
    const kind = r.target === "config" ? "config" : "recipe";
    const d = await call(`${kind === "config" ? "config" : "recipe"}.draft_new`, { pane, id: r.id, ask: r.ask, section: r.section }).catch(() => null);
    if (d?.draft) aiDrafts.push({ kind, draft: d.draft, id: r.id, ask: r.ask, pane, who: `${cfg?.values.profiles.find((p) => p.id === r.profile)?.name ?? r.profile} · ${meta[pane]?.name ?? ""}` });
  }
  $effect(() => {
    if (!aiDrafts.some((d) => !d.ready)) return;
    const timer = setInterval(async () => {
      for (const d of aiDrafts.filter((x) => !x.ready)) {
        const r = await call(d.kind === "config" ? "config.draft_get" : "recipe.draft_get", { draft: d.draft, id: d.id }).catch(() => null);
        if (r?.exists) (d.ready = true), say(tt("hn.draftReady", { id: d.id }));
      }
    }, 3000);
    return () => clearInterval(timer);
  });

  function jumpTo(p: number) {
    for (const [wi, w] of wss.entries())
      for (const [ti, t] of w.tabs.entries())
        if (panes(t.tree).includes(p)) {
          at = { ws: wi, tab: ti };
          if (compareId) compareId = t.parent ? t.id : "";
          t.focus = p;
          zoomed = false;
        }
  }
  const tabOf = (p: number) => wss.flatMap((w) => w.tabs).find((t) => panes(t.tree).includes(p));

  // FR-40, FR-87: the done mark goes away once that pane is seen (agent or shell) — every pane on screen, so a split
  // next to the focused pane doesn't keep the Dock badge up either
  $effect(() => {
    for (const p of tab ? panes(tab.tree) : []) if (onScreen(p) && meta[p]?.status === "done" && !meta[p]?.waitingShell && !ack[p]) ack[p] = true;
  });

  // FR-55: the toolbar of the newly focused pane is refreshed (no git polling)
  $effect(() => {
    if (tab?.focus) refreshGit(tab.focus);
  });

  // FR-40: Dock badge and Dock bounce only while the window isn't being looked at
  let seen = $state(document.hasFocus());
  // FR-55: content and order of toolbar chips from [toolbar]
  const toolbar = $derived(cfg?.values.toolbar ?? { left: ["attach", "diff", "rich_input"], right: ["cwd", "branch", "settings"] });
  const notify = $derived(cfg?.values.notify ?? { on_blocked: true, on_done: true, long_command_secs: 30, sound: true });

  /// FR-40 / FR-87 (TECH 27): every notification goes through here. The bell always records; the banner follows the rules
  /// in lib/notice.ts (one per pane, no repeat inside 60 s, several "done" merged, withdrawn once answered) and only shows
  /// while the window isn't being looked at. Content is pane name · agent, kind · Workspace › Tab, action: no task label,
  /// no screen content, no message (SEC-33). Clicking a banner = go to that pane (`notify-click` from the plugin's
  /// `on_action`, notification id = pane id); normal window focus (Cmd+Tab) doesn't move panes.
  /// `detail` = FR-84 reason shown only in the app; `force` skips the switches and `bell: false` skips the bell (harness
  /// banners keep their own switches, FR-74).
  const nst = newState();
  function notice(pane: number | undefined, kind: NoticeKind, o: { detail?: string; force?: boolean; bell?: boolean; run?: string } = {}) {
    const m = pane === undefined ? undefined : meta[pane];
    if (m && pane !== undefined && o.bell !== false) note(pane, kind, o.detail);
    if (seen || (!o.force && !enabled(kind, notify))) return;
    if (m && cfg?.values.profiles.find((p) => p.id === m.profile)?.notify === false && !o.force) return;
    const t = pane === undefined ? undefined : tabOf(pane);
    const w = t && wss.find((w) => w.tabs.includes(t));
    // SEC-33: the tab name only when a person gave it (`t.title`); `title()` falls back to the agent's task label (FR-69)
    const at = whereOf(w?.name ?? "", t?.title ?? "");
    // the agent running there, also one typed in a shell pane ("shell-2 · Claude Code", not "· Shell"); the pane's own
    // profile first when it is that agent
    const own = m && cfg?.values.profiles.find((p) => p.id === m.profile);
    const running = pane === undefined ? "" : agentOf(pane);
    const agent = m ? ((running && own?.agent !== running && cfg?.values.profiles.find((p) => p.agent === running)?.name) || own?.name || m.profile) : "";
    const title = m ? tt("notice.title", { name: m.name || m.profile, agent }) : (o.run ?? "Gembala");
    const what = tt(wordKey(kind));
    const e: NoticeEvent = {
      pane: pane ?? 0,
      kind,
      now: Date.now(),
      title,
      body: `${at ? tt("notice.body", { what, where: at }) : what}\n${tt(actKey(kind))}`,
      sound: soundOn(kind, notify),
      merged: (n) => ({ title: tt("notice.merged", { n }), body: `${at}${at ? "\n" : ""}${tt("notice.mergedAct")}` }),
    };
    const p = plan(nst, e);
    if (!p.show) return;
    if (p.show.bounce) invoke("attention").catch(() => {}); // the Dock bounces only when action is needed
    invoke("notify_pane", { title: p.show.title, body: p.show.body, sound: p.show.sound, pane: p.show.pane || undefined, replace: p.remove }).catch(() => {});
  }
  /// FR-87: a card waiting for your OK (kiriman antar agent, Buka dan kirim) or a message that was refused or expired.
  /// `msg.event` carries pane names, not ids (names are unique, FR-79); a name that matches no pane = no banner.
  function onMsgEvent(ev: { kind?: string; reason?: string | null; msg?: { from?: string; to?: string } } | undefined) {
    if (!ev) return;
    const named = (n?: string) => Object.entries(meta).find(([p, m]) => n && m.name === n && !dead[+p])?.[0];
    // an open request's card sits in the asking pane (`from`); a held message's card in the receiver (`to`)
    if (ev.kind === "confirm") return void ((p) => p !== undefined && notice(+p, "approve"))(named(ev.reason === "needs_open" ? ev.msg?.from : ev.msg?.to));
    if (ev.kind === "rejected" && ev.reason !== "rejected_by_user") ((p) => p !== undefined && notice(+p, "failed"))(named(ev.msg?.from));
  }
  const withdrawTitles = (titles: string[]) => titles.length && invoke("withdraw", { titles }).catch(() => {});
  /// a kind that waits on you is withdrawn once the pane isn't waiting anymore (answered, closed, card gone)
  function sweep() {
    for (const [p, s] of [...nst.by]) if (p && !stillWaits(p, s.kind)) withdrawTitles(settle(nst, p));
  }
  // FR-76: bell history; unlike the macOS banner it may carry the task label (FR-69), being in-app only (SEC-33).
  // Newest first, max 100.
  // ponytail: kept in UI memory, so it clears when the app restarts; move to the daemon if it must outlive the UI
  type Note = { id: number; pane: number; agent: string; name: string; task: string; tab: string; ws: string; kind: NoticeKind; detail: string; at: number; seen?: boolean };
  let notes = $state<Note[]>([]);
  let bell = $state(false);
  let unfold = $state<Record<number, boolean>>({});
  let noteId = 0;
  /// FR-87: the pane on screen in the front window is seen as it happens, so its note never adds to the bell count
  const onScreen = (pane: number) => seen && !settingsOpen && !!tab && panes(tab.tree).includes(pane);
  function note(pane: number, kind: NoticeKind, detail = "") {
    const t = tabOf(pane);
    const w = wss.find((w) => t && w.tabs.includes(t));
    const n = { id: ++noteId, pane, agent: agentOf(pane), name: meta[pane].name || meta[pane].profile, task: labels[pane] ?? "", tab: t ? title(t) : "", ws: w?.name ?? "", kind, detail, at: Date.now(), seen: onScreen(pane) || undefined };
    notes = [n, ...notes].slice(0, 100);
  }
  /// bell line: the kind's word, then the FR-84 reason when it says more ("butuh izin · minta percaya folder")
  const kindText = (kind: NoticeKind, detail = "") => {
    const word = tt(wordKey(kind));
    // the FR-84 reasons that only restate the kind ("minta izin", "nunggu jawaban") add nothing
    const more = detail && detail !== "reason.permission" && detail !== "reason.input" ? tOr(detail, detail) : "";
    return more && more !== word ? `${word} · ${more}` : word;
  };
  const noteText = (n: Note) => kindText(n.kind, n.detail);
  /// kind of a pane that is `blocked`: FR-84 reason when the agent gave one, else a password prompt in a plain shell
  const blockedKindOf = (p: number): NoticeKind => {
    const m = meta[p];
    if (m?.reason) return blockedKind(waitingReasonKey(m.reason));
    return m && !AGENTS.includes(m.agent) && m.agent !== "custom" && !AGENTS.includes(m.busy ?? "") ? "password" : "permission";
  };
  /// is that pane still waiting for what the note says (permission, answer, password, a card to approve)?
  function stillWaits(pane: number, kind: NoticeKind) {
    if (kind === "approve") return pendingFor(pane).length > 0 || opensFor(pane).length > 0 || Object.values(runs).some((r) => r.state === "gate" && stagePanes(r).some((x) => x.pane === pane && x.step === r.gate));
    return meta[pane]?.status === "blocked";
  }
  // one row per pane (newest note, older ones folded); a permission still waiting goes first, the rest stay newest first
  const groups = $derived.by(() => {
    const by = new Map<number, Note[]>();
    for (const n of notes) by.get(n.pane)?.push(n) ?? by.set(n.pane, [n]);
    return [...by.values()].sort((a, b) => +waiting.has(b[0].id) - +waiting.has(a[0].id));
  });
  const unseen = $derived(groups.filter((g) => g.some((n) => !n.seen)).length);
  const noteWhere = (n: Note) => [n.ws, n.tab].filter(Boolean).join(" › ");
  // a waiting kind (permission, answer, password, approve) is still waiting only while it is its pane's newest note and the pane still waits
  const waiting = $derived.by(() => {
    const open = new Set<number>(), saw = new Set<number>();
    for (const n of notes) {
      if (saw.has(n.pane)) continue;
      saw.add(n.pane);
      if (KINDS[n.kind].waits && stillWaits(n.pane, n.kind)) open.add(n.id);
    }
    return open;
  });
  const noteTime = (at: number) => (now - at < 3600_000 ? ago(now - at) : new Date(at).toLocaleTimeString(loc(), { hour: "2-digit", minute: "2-digit" }));
  $effect(() => {
    if (!updOpen) return;
    const off = (e: PointerEvent) => !(e.target as Element).closest?.(".updcard, .updbtn") && (updOpen = false);
    window.addEventListener("pointerdown", off);
    return () => window.removeEventListener("pointerdown", off);
  });
  $effect(() => {
    if (!bell) return;
    const off = (e: PointerEvent) => !(e.target as Element).closest?.(".notes, .bellbtn") && (bell = false);
    window.addEventListener("pointerdown", off);
    return () => window.removeEventListener("pointerdown", off);
  });
  const attention = $derived(
    Object.keys(meta).map(Number).filter((p) => (stat(p) === "blocked" && enabled(blockedKindOf(p), notify)) || (stat(p) === "done" && !meta[p]?.waitingShell && enabled("done", notify))).length +
      (enabled("approve", notify) ? (msgData?.pending?.length ?? 0) + (msgData?.opens?.length ?? 0) : 0), // FR-87: cards waiting for your OK count too
  );
  $effect(() => {
    invoke("badge", { count: seen ? 0 : attention }).catch(() => {});
  });
  function onStatus(pane: number, status: string, shell = false, reason = "", at?: number, last?: string | null) {
    if (!meta[pane]) return;
    // FR-87: the first read after the UI connects says what the pane already was, not something that just happened; a
    // pane that finished while the app was closed must not open a bell entry or a banner. The daemon's status time says
    // whether it began before we started reading — a status from our own second may still be newer. Whole seconds, so a
    // status one second or more old is stale; without a time (older daemon) the first read stays quiet.
    const firstRead = !readStatus.has(pane);
    readStatus.add(pane);
    const stale = firstRead && (!at || at * 1000 + 1000 <= observingSince);
    if (at) statusAt[pane] = at * 1000;
    if (last !== undefined) previews[pane] = last ?? "";
    if (at && status === "blocked") blockedAt[pane] = at * 1000;
    if (at && (status === "done" || status === "idle")) quietAt[pane] = at * 1000;
    const previous = meta[pane];
    const changed = previous.status !== status || previous.waitingShell !== shell || (previous.reason ?? "") !== reason;
    meta[pane].status = status;
    meta[pane].waitingShell = shell;
    meta[pane].reason = reason;
    if (!changed) return;
    if (!at) statusAt[pane] = Date.now();
    if (status === "blocked") blockedAt[pane] = statusAt[pane];
    if (status === "done" || status === "idle") quietAt[pane] = statusAt[pane];
    else delete quietAt[pane];
    delete ack[pane];
    if (status === "done" || status === "idle") {
      refreshGit(pane); // toolbar refreshed per turn (TECH 7a)
      if (changes?.pane === pane) loadChanges(pane); // FR-22: the panel is refreshed too
    }
    if (!stale && status === "blocked") {
      const key = waitingReasonKey(reason);
      notice(pane, blockedKind(key), { detail: reason ? (key ?? reason) : "" });
    }
    if (!stale && status === "done" && !shell) notice(pane, "done");
    sweep(); // FR-87: a banner whose pane isn't waiting anymore leaves Notification Center
  }

  // save the layout half a second after the last change (SEC-13: no screen content)
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const body = JSON.stringify({ version: 1, boot, at, side: sideW, panel: panelW, workspaces: wss, panes: Object.fromEntries(Object.entries(meta).map(([id, { waitingShell, reason, ...pane }]) => [id, pane])) });
    if (!wss.length || !boot) return;
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => call("layout.save", { layout: JSON.parse(body) }).catch(() => {}), 500);
  });

  // folder and process of each pane for the tab title, subtitle, and agent icon (FR-52)
  async function poll() {
    now = Date.now();
    const ids = Object.keys(meta).map(Number).filter((p) => !dead[p]);
    // sent all at once (the daemon client multiplexes by id), not waiting one by one (TECH 19 Responsive)
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
      // agent typed in a shell pane (`m.busy` = agent in the previous poll): an empty status from the daemon when the agent
      // exits is used too, so "done" doesn't get stuck after its agent died
      if (AGENTS.includes(m.agent) || m.agent === "custom" || i.status || AGENTS.includes(m.busy ?? "")) {
        // status from the daemon: official hooks, or screen patterns for custom profiles and agents typed by hand (FR-4)
        onStatus(p, i.status, i.waiting_shell, i.reason, i.status_at, i.last);
      } else {
        // FR-40 for shell panes: a command longer than 30 seconds that finishes = done; waiting for a password = blocked
        if (busy && !m.busy) m.since = Date.now();
        if (i.password && m.status !== "blocked") {
          m.status = "blocked";
          notice(p, "password");
        } else if (!i.password && m.status === "blocked") (m.status = ""), sweep();
        if (!busy && m.busy && Date.now() - (m.since ?? Date.now()) >= notify.long_command_secs * 1000) {
          delete ack[p];
          m.status = "done";
          notice(p, "done");
        }
      }
      if (busy && busy !== m.busy && m.ended) delete m.ended; // FR-56: the finished block goes away once a new process runs
      // FR-99 P1: so does the "send it to an agent?" card: Send would swap the pane and close what runs there now
      if (busy && !m.busy && offer[p]) delete offer[p];
      if (m.busy !== busy) m.busy = busy;
      if (m.shell !== i.shell) m.shell = i.shell;
      // FR-69: label only while an agent runs in this pane
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

  // Esc the page doesn't use is passed by WebKit to the window = macOS leaves full screen; full screen is just fn+F
  const keepFullscreen = (e: KeyboardEvent) => e.key === "Escape" && e.preventDefault();
  let fullscreen = $state(false); // full screen: macOS window buttons disappear, their space in the sidebar goes too

  onMount(() => {
    call("account.list", {}).then((r) => { openModels = r.models ?? []; openEfforts = r.efforts ?? []; }).catch(() => {});
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keydown", keepFullscreen);
    const win = getCurrentWindow();
    const onFull = () => win.isFullscreen().then((f) => (fullscreen = f)).catch(() => {});
    onFull();
    const unlistenFull = win.onResized(onFull);
    const unlisten = listen<string>("shortcut", ({ payload }) => run(payload));
    // FR-40: banner clicked = go to its pane; that pane's bell entries get marked seen too
    const unlistenClick = listen<number>("notify-click", ({ payload: p }) => {
      notes.forEach((n) => n.pane === p && (n.seen = true));
      if (meta[p] && !dead[p]) jumpTo(p);
    });
    const unlistenCfg = listen<any>("daemon", ({ payload }) => {
      if (payload.event === "config.changed") cfg = payload.params;
      if (payload.event === "pane.status") onStatus(payload.params.pane, payload.params.status, payload.params.shell, payload.params.reason, payload.params.status_at, payload.params.last);
      // a tab whose xterm is detached has no Tile listening for pane.exit
      if (payload.event === "pane.exit" && !tiles[payload.params.pane]) onexit(payload.params.pane);
      if (payload.event === "pane.replace" && meta[payload.params.pane]) {
        // agent exits = empty status (the daemon clears it without a pane.status event; without this it stays "working")
        previews[payload.params.pane] = "";
        Object.assign(meta[payload.params.pane], { output: true, status: "", ended: { resume: payload.params.resume ?? null } });
        // FR-87: the agent itself failed; 126/127 = didn't start or wasn't found (the pane already says so), >128 = a
        // signal (Ctrl+C, closing it): not news
        const code = payload.params.code;
        if (typeof code === "number" && code >= 1 && code <= 125) notice(payload.params.pane, "failed");
      }
      if (payload.event === "daemon.lost") onLost();
      if (payload.event === "msg.event") (onMsgEvent(payload.params), refreshMsgs().then(sweep));
      if (payload.event === "harness.state") onRun(payload.params);
      if (payload.event === "harness.notice") runNotice(payload.params);
      if (payload.event === "telegram.changed") tg = payload.params ?? { state: "off" };
      if (payload.event === "usage.changed") usageRows = Array.isArray(payload.params?.rows) ? payload.params.rows : [];
      if (payload.event === "update.changed") upd = payload.params;
    });
    // an older daemon has no telegram.* (unknown_method): stays off
    call("telegram.status").then((r) => (tg = r ?? { state: "off" })).catch(() => {});
    call("usage.list").then((r) => (usageRows = Array.isArray(r?.rows) ? r.rows : [])).catch(() => {});
    // an older daemon has no update.* either: no update badge
    call("update.status").then((r) => (upd = r)).catch(() => {});
    getVersion().then((v) => (appVer = v)).catch(() => {});
    const timer = setInterval(poll, 2000);
    // FR-15: dragging a file onto the window = quoted path pasted into the focused pane (bracketed paste, no Enter)
    const unlistenDrop = getCurrentWebview().onDragDropEvent(({ payload }) => {
      if (payload.type !== "drop" || !tab || dead[tab.focus] || !payload.paths.length) return;
      const pane = tab.focus;
      if (rich?.pane === pane) return void invoke<string[]>("resolve_paths", { paths: payload.paths }).then(addRichFiles).catch(() => {});
      // FR-59: editor shown = the path goes into the editor, not straight to the shell
      if (editorShown(pane)) invoke<string[]>("resolve_paths", { paths: payload.paths }).then((p) => insertPaths(pane, p)).catch(() => {});
      else attach(pane, payload.paths);
    });
    const onFocus = () => ((seen = true), withdrawTitles(clear(nst))); // FR-87: looked at = the banners are done with
    const onBlur = () => (seen = false);
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);
    (async () => {
      try {
        cfg = await call("config.get");
        // FR-59: bahan autocomplete
        homePath = (await homeDir().catch(() => "")).replace(/\/$/, "");
        call("agents.detect", {}).then((a: Agent[]) => (installed = a.filter((x) => x.installed).map((x) => x.agent))).catch(() => {});
        invoke<string[]>("shell_history").then((h) => (zhist = h)).catch(() => {});
        await restore();
        restored = true;
        // panes alive from the previous run: the daemon is told their workspace (access rules SEC-4/21)
        for (const p of Object.keys(meta).map(Number).filter((p) => !dead[p])) call("pane.workspace", { pane: p, workspace: wsIdOf(p) }).catch(() => {});
        refreshMsgs();
        for (const w of wss) call("harness.state", { workspace: w.id }).then((r) => r?.run && onRun(r)).catch(() => {});
        observingSince = Date.now(); // FR-87: everything the daemon reports from here on can be an event
        poll();
        // FR-21: new app + old daemon = ask first; restarting the daemon stops every pane
        const [d, app] = await Promise.all([call("daemon.info"), getVersion()]);
        if (d.daemon !== app) upgrade = { daemon: d.daemon, app, panes: (await call("pane.list")).length };
      } catch (e) {
        error = tt("main.connectFailed", { why: errText(e) });
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
      unlistenClick.then((f) => f());
      unlistenCfg.then((f) => f());
      unlistenDrop.then((f) => f());
    };
  });
  $effect(() => {
    if (!restored || !appVer) return;
    // FR-103: a new install shows the welcome card (FR-43) instead; nothing was seen, so remember it now
    if (!seenVersion && freshInstall) return void saveSeenVersion(appVer);
    // the card that opened itself follows the language; one opened by hand (palette, Settings) stays as it is
    whatsNewAuto = untrack(() => whatsNewAuto || whatsNew === null);
    if (whatsNewAuto) whatsNew = upgradeNotes(changelog, seenVersion, appVer, i18n.lang);
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
    {:else if name === "sliders"}<path d="M2 5h7M12 5h2M2 11h2M7 11h7" /><circle cx="10.5" cy="5" r="1.5" /><circle cx="5.5" cy="11" r="1.5" />
    {:else if name === "gear"}<circle cx="8" cy="8" r="2" /><path d="M12.7 6.2L14.5 6.7L14.5 9.3L12.7 9.8L12.6 10.1L13.5 11.6L11.6 13.5L10.1 12.6L9.8 12.7L9.3 14.5L6.7 14.5L6.2 12.7L5.9 12.6L4.4 13.5L2.5 11.6L3.4 10.1L3.3 9.8L1.5 9.3L1.5 6.7L3.3 6.2L3.4 5.9L2.5 4.4L4.4 2.5L5.9 3.4L6.2 3.3L6.7 1.5L9.3 1.5L9.8 3.3L10.1 3.4L11.6 2.5L13.5 4.4L12.6 5.9z" />
    {:else if name === "bell"}<path d="M4 11.5V7a4 4 0 0 1 8 0v4.5l1 1H3zM6.5 14h3" />
    {:else if name === "phone"}<rect x="4.5" y="1.5" width="7" height="13" rx="1.5" /><path d="M7 12h2" />{/if}
  </svg>
{/snippet}

{#snippet chip(c: string, pane: number, g: any)}
  {#if c === "attach"}
    <button class="chip box c-attach" title={tt("chip.attachTip")} aria-label={tt("chip.attach")} onclick={async () => attach(pane, await invoke<string[]>("pick_files"), true)}>{@render icon("plus")}</button>
  {:else if c === "diff" && g?.repo && g.files}
    <button class="chip box c-diff" title={tt("chip.diffTip")} onclick={() => openChanges(pane)}>{@render icon("file")}{g.files} • <span class="plus">+{g.add}</span> <span class="minus">−{g.del}</span></button>
  {:else if c === "rich_input"}
    <button class="chip box c-rich" onclick={() => openRich(pane)}>{@render icon("rich")}Rich Input <span class="n">⌘G</span></button>
  {:else if c === "cwd"}
    <span class="chip box quiet c-cwd" title={meta[pane]?.cwd ?? ""}>{@render icon("folder")}{short(meta[pane]?.cwd) || "~"}</span>
  {:else if c === "branch" && g?.repo && g.branch}
    <!-- the branch isn't cut; the folder gives way (TECH 19 #9) -->
    <span class="chip box c-branch">{@render icon("branch")}{g.branch}{g.ahead ? ` ↑${g.ahead}` : ""}{g.behind ? ` ↓${g.behind}` : ""}</span>
  {:else if c === "settings"}
    <button class="chip box c-set" title={tt("set.title")} aria-label={tt("chip.settingsAria")} onclick={() => (settingsOpen = true)}>{@render icon("sliders")}</button>
  {/if}
{/snippet}

<Rail rows={usageRows} {installed} accounts={cfg?.values.accounts ?? []} active={usageActive} {simple} full={fullscreen} {tg} {unseen} {bell}
  showHint={!confirm && !picker && !askMode && !linkAsk && !settingsOpen && meta[tab?.focus ?? 0]?.status !== "blocked"} resetHints={hintReset}
  onsearch={openPalette} ontask={() => (newTask = true)} onbell={() => (bell = !bell)}
  ontelegram={() => ((settingsSec = "telegram"), (settingsOpen = true))} onsettings={() => (settingsOpen = true)} />
{#if sidebar}
  <aside class="side" aria-label={tt("menu.tab")} style:width="{sideW}px" style:left="44px" class:dragging={!!dragging} class:full={fullscreen}>
    <!-- transparent title bar: window button area + window drag -->
    <div class="drag" data-tauri-drag-region></div>
    <button class="add find" onclick={openPalette}>
      <span class="fl"><svg class="pfind" viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.4" fill="none" stroke="currentColor" stroke-width="1.4" /><path d="M10.4 10.4 14 14" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" /></svg>{tt("side.find")}</span>
      <span class="n">⌘P</span>
    </button>
    <!-- FR-99 P1: the main door, framed and named after what happens (not a "New task" label that reads like a to-do) -->
    <button class="add ask" onclick={() => (newTask = true)}><span>+ {tt("hn.newTask")}</span><span class="n">⌘N</span></button>
    {#if ws && runs[ws.id]}
      <button class="add" onclick={() => ((drawer = false), (flow = !flow))}>
        {runLive(runs[ws.id]) ? "● " : ""}{runs[ws.id].name} · {tOr(`hn.state.${runs[ws.id].state}`, runs[ws.id].state)}
      </button>
    {/if}
    <!-- FR-44: below the search box, always visible even with a long tab list -->
    {#if queue.length}
      <!-- FR-65: attention queue; hidden when nothing is waiting -->
      <div class="queue" role="region" aria-label={tt("status.blocked")}>
        <div class="qhead">{tt("side.needsYou")} <span>{queue.length}</span></div>
        {#each queue as q (q.p)}
          {@const ag = agentOf(q.p) || meta[q.p]?.agent || ""}
          <button class="tab" onclick={() => jumpTo(q.p)}>
            <span class="tt"><span class="dot blocked" aria-hidden="true"></span>{#if GLYPH[ag]}<span class="aico {ag}">{GLYPH[ag]}</span>{/if}{meta[q.p]?.name}{labels[q.p] || GLYPH[ag] ? ` · ${labels[q.p] || ag}` : ""}</span>
            <span class="kind">{tt(wordKey(blockedKindOf(q.p)))}</span><span class="k">{ago(now - (blockedAt[q.p] ?? now))}</span>
            <span class="sub">{[title(q.t) !== labels[q.p] ? title(q.t) : "", wss.length > 1 ? q.w.name : ""].filter(Boolean).join(" · ")}</span>
          </button>
        {/each}
        <div class="qhint">{tt("side.nextHint", { key: pretty("Cmd+Shift+U") })}</div>
      </div>
    {/if}
    {#each wss as w, wi (w.id)}
      <!-- FR-44: click the title = expand/collapse; rename through right click or the palette -->
      {@const liftWs = dragging?.kind === "ws" && dragging.wi === wi}
      <!-- FR-44: colored workspace block (mockup-warna-tab variant D) -->
      <div class="wsg {w.color ?? ''}">
      <div class="ws" data-wi={wi} data-k="{w.id}:h" class:lift={liftWs} class:target={dragging?.kind === "tab" && dragging.over.wi === wi && dragging.wi !== wi}
        role="button" tabindex="0" aria-expanded={!w.collapsed} title={w.root}
        onclick={() => editing?.id !== w.id && (w.collapsed = !w.collapsed)}
        onkeydown={(e) => e.key === "Enter" && e.target === e.currentTarget && (w.collapsed = !w.collapsed)}
        onpointerdown={(e) => startDrag(e, "ws", wi)} oncontextmenu={(e) => wsMenu(e, wi)}>
        {#if editing?.kind === "ws" && editing.id === w.id}
          <!-- svelte-ignore a11y_autofocus -->
          <input class="edit" bind:value={editing.value} onkeydown={(e) => e.key === "Enter" && saveEdit()} onblur={saveEdit} autofocus aria-label={tt("side.wsName")} />
        {:else}
          <span class="chev" aria-hidden="true">{w.collapsed ? "▸" : "▾"}</span>
          <span class="wsname" title={w.name}>{w.name}</span>
          {#if w.collapsed}
            <!-- FR-44: collapsed still shows the tab count and the most severe status -->
            {@const st = worst(w.tabs.flatMap((t) => panes(t.tree)))}
            <span class="wsum">{#if stText(st)}<span class="dot {st}" aria-hidden="true"></span><span class="st {st}">{stText(st)}</span>{" · "}{/if}{ttn("side.tabs", w.tabs.length)}</span>
          {/if}
          <!-- FR-52: new tab in THIS workspace; right click/Option = pick a profile -->
          <button class="wsadd" title={tt("side.newTabTip", { name: w.name })} aria-label={tt("side.newTabAria", { name: w.name })}
            onpointerdown={(e) => e.stopPropagation()}
            onclick={(e) => (e.stopPropagation(), e.altKey ? profileMenu(e, wi) : newTab(undefined, wi))}
            oncontextmenu={(e) => profileMenu(e, wi)}>+</button>
        {/if}
      </div>
      {#if !w.collapsed && !w.tabs.length}
        <button class="add empty-ws" data-wi={wi} data-k="{w.id}:e" class:lift={liftWs}
          onclick={() => newTab(undefined, wi)}>+ Tab</button>
      {/if}
      {#each w.collapsed ? [] : helperRows(w) as row (`${row.kind}:${row.tab.id}`)}
        {@const t = row.tab}
        {@const ti = w.tabs.indexOf(t)}
        {#if row.kind === "summary"}
          {@const children = w.tabs.filter((x) => x.parent === t.id)}
          {@const waiting = children.filter((x) => meta[x.focus]?.status === "blocked").length}
          <button class="helper-summary" aria-expanded={!!t.helpersOpen || waiting > 0} onclick={() => (t.helpersOpen = !t.helpersOpen)}>
            <span>{t.helpersOpen || waiting ? "▾" : "▸"}</span>
            {tt("helper.count", { n: children.length })}{waiting ? `, ${tt("helper.waiting", { n: waiting })}` : ""}
          </button>
          {#if latestHelper?.parent === t.id && !confirm && !picker && !askMode && !linkAsk}
            <div class="helper-tip" role="status">
              {helperTip ? tt("helper.tip", { name: title(t), key: "⌘⌥J" }) : tt("helper.ready")}
              <span class="helper-tip-actions"><button class="mini" onclick={() => { const child = w.tabs.find((x) => x.id === latestHelper?.id); if (child) openTab(wi, child); }}>{tt("helper.view")}</button>{#if helperTip}<button class="mini" onclick={dismissHelperTip}>{tt("helper.gotIt")}</button>{/if}</span>
            </div>
          {/if}
        {:else if row.kind === "fold"}
          <button class="helper-fold" aria-expanded={!!t.doneOpen} onclick={() => (t.doneOpen = !t.doneOpen)}>{t.doneOpen ? "▾" : "▸"} {tt(simple ? "helper.doneSimple" : "helper.done", { n: row.count ?? 0 })}</button>
        {:else}
        {@const n = order.findIndex((o) => o.t === t)}
        {@const agents = panes(t.tree).map((p) => meta[p]?.busy).filter(Boolean)}
        {@const st = row.child ? (meta[t.focus]?.status ?? "") : tabStatus(t)}
        <button class="tab {t.color ?? ''}" class:helper={!!row.child} class:on={at.ws === wi && at.tab === ti} class:blocked={st === "blocked"} data-wi={wi} data-ti={ti}
          class:lift={(dragging?.kind === "tab" && dragging.wi === wi && dragging.ti === ti) || (dragging?.kind === "ws" && dragging.wi === wi)}
          data-k={t.id} title={row.child ? tt("helper.shortcut") : undefined}
          onpointerdown={(e) => !row.child && startDrag(e, "tab", wi, ti)} oncontextmenu={(e) => tabMenu(e, wi, t)}
          onclick={() => openTab(wi, t)}
          ondblclick={() => (editing = { kind: "tab", id: t.id, value: t.title || title(t) })}>
          {#if editing?.kind === "tab" && editing.id === t.id}
            <!-- svelte-ignore a11y_autofocus -->
            <input class="edit" bind:value={editing.value} onkeydown={(e) => e.key === "Enter" && saveEdit()} onblur={saveEdit} autofocus aria-label={tt("side.tabName")} />
          {:else}
            {@const ag = panes(t.tree).map(agentOf).find(Boolean) ?? ""}
            <span class="tt">{#if row.child || ag || stText(st)}<span class="dot {st || 'idle'}" aria-hidden="true"></span>{/if}{#if ag}<span class="aico {ag}" title={ag}>{GLYPH[ag]}</span>{/if}{row.child && simple ? `${tt("helper.simple", { name: title(w.tabs.find((x) => x.id === t.parent) ?? t) })} · ` : ag && meta[t.focus]?.name ? `${meta[t.focus].name} · ` : ""}{title(t)}</span>
            <span class="n"><span class="k">{!row.child && n >= 0 && n < 9 ? `⌘${n + 1}` : ""}</span><span class="x" role="button" tabindex="-1" aria-label={tt("side.closeTab", { name: title(t) })}
                onclick={(e) => (e.stopPropagation(), closeTabs(wi, [t]))} onkeydown={(e) => e.key === "Enter" && closeTabs(wi, [t])}>×</span></span>
            {@const g = meta[t.focus]?.git}
            <!-- FR-61: accounts other than Utama show in the tab caption -->
            <!-- agents already show as an icon; other processes (npm, vim) are still named -->
            {@const idle = meta[t.focus]?.agent === "shell" && !meta[t.focus]?.busy ? meta[t.focus]?.shell : ""}
            {@const rest = [g?.repo ? (simple ? tt("helper.branch", { name: g.branch }) : g.branch) : "", where(meta[t.focus]?.cwd, w.root), ...agents.filter((a) => !AGENTS.includes(a!)), acctOf(t.focus), idle].filter((x) => x && x !== title(t)).join(" · ")}
            {#if row.child || ag || stText(st) || rest}
              <span class="sub" title={home(meta[t.focus]?.cwd)}>{#if panes(t.tree).length > 1}<!-- satu titik per pane: pane selesai tidak menutupi pane lain yang masih bekerja -->{#each panes(t.tree) as p (p)}<span class="dot pdot {stat(p) || 'idle'}" title="{meta[p]?.name} {helperStatus(p)}"></span>{/each}{/if}{#if row.child || ag || stText(st)}<span class="st {st}">{helperStatus(t.focus)}</span>{/if}{st === "blocked" ? ` · ${tt("helper.open")}` : ""}{#if rest}{row.child || ag || stText(st) || panes(t.tree).length > 1 ? " · " : ""}{rest}{/if}</span>
            {/if}
            {#if previews[t.focus] && st !== "blocked"}<span class="sub">{previews[t.focus]}</span>{/if}
          {/if}
        </button>
        {/if}
      {/each}
      </div>
    {/each}
    <!-- FR-99 P1: used rarely, so it sits quietly under the list -->
    <button class="add wsnew" onclick={addWorkspace}>+ {simple ? tt("side.addProject") : "Workspace"}</button>
    {#if dragging}<div class="dropline" style:top="{dragging.line.top}px" style:left="{dragging.line.left}px" style:width="{dragging.line.width}px"></div>{/if}
    <!-- FR-82: update notice stays in the sidebar; daily controls live in the rail. -->
    {#if updNew}<button class="updbtn" aria-expanded={updOpen} onclick={() => (updOpen = !updOpen)}><i aria-hidden="true"></i>{tt("upd.pill", { v: upd?.latest ?? "" })}</button>{/if}
  </aside>
  {#if updOpen && updNew && upd}
    {@const lines = noteLines(upd.notes)}
    <div class="updcard" role="dialog" aria-label={tt("upd.title", { v: upd.latest ?? "" })}>
      <b>{tt("upd.title", { v: upd.latest ?? "" })}</b>
      <small>{tt("upd.sub", { cur: appVer })}{upd.published ? ` · ${new Date(upd.published).toLocaleDateString(loc(), { day: "numeric", month: "short" })}` : ""}</small>
      <!-- release notes come from the network: text only, never HTML -->
      {#if lines.length}<ul>{#each lines as l, i (i)}<li>{l}</li>{/each}</ul>{/if}
      <code class="updcmd">{upgradeCmd(upd)}</code>
      <small>{tt("upd.before")}</small>
      <span class="acts">
        <button class="mini" onclick={skipUpdate}>{tt("upd.skip")}</button>
        <button class="mini" onclick={() => invoke("open_url", { url: upd?.url ?? "https://github.com/AHanif52/gembala/releases" }).catch(() => {})}>{tt("upd.notes")}</button>
        <button class="mini pri" onclick={() => copy(upgradeCmd(upd))}>{tt("upd.copy")}</button>
      </span>
    </div>
  {/if}
  <div class="sidegrip" style:left="{sideW + 41}px" onpointerdown={dragSide} role="separator" aria-orientation="vertical" aria-label={tt("side.width")}></div>
{/if}
{#if bell}
  <!-- FR-76: bell stays available even when the sidebar is hidden. -->
  <div class="notes" role="dialog" aria-label={tt("bell.title")}>
      <div class="nhead"><b>{tt("bell.title")}</b>{#if unseen}<button class="nlink" onclick={() => notes.forEach((n) => (n.seen = true))}>{tt("bell.markAll")}</button>{/if}</div>
      <div class="nlist">
        {#each groups as g, i (g[0].pane)}
          {@const n = g[0]}
          {@const past = KINDS[n.kind].waits && !waiting.has(n.id)}
          {#if noteWhere(n) && (i === 0 || noteWhere(groups[i - 1][0]) !== noteWhere(n))}<div class="nwhere">{noteWhere(n)}</div>{/if}
          <button class="nrow" class:seen={g.every((o) => o.seen)} class:past onclick={() => (g.forEach((o) => (o.seen = true)), (bell = false), meta[n.pane] && !dead[n.pane] && jumpTo(n.pane))}>
            <span class="dot {n.kind}" aria-hidden="true"></span>
            <span class="nmain">
              <span class="ntitle">{#if GLYPH[n.agent]}<span class="aico {n.agent}">{GLYPH[n.agent]}</span>{/if}{n.name}{n.task && n.task !== n.tab ? ` · ${n.task}` : ""}</span>
              <span class="nwhat">{noteText(n)}{#if past}{` · ${meta[n.pane] && !dead[n.pane] ? tt("bell.answered") : tt("bell.closed")}`}{/if}</span>
            </span>
            <span class="k">{noteTime(n.at)}</span>
          </button>
          {#if g.length > 1}
            <button class="nmore" aria-expanded={!!unfold[n.pane]} onclick={() => (unfold[n.pane] = !unfold[n.pane])}>{unfold[n.pane] ? tt("bell.hide") : tt("bell.earlier", { n: g.length - 1 })}</button>
            {#if unfold[n.pane]}
              {#each g.slice(1) as o (o.id)}
                <div class="nold"><span>{noteText(o)}{KINDS[o.kind].waits ? ` · ${tt("bell.answered")}` : ""}{o.task && o.task !== n.task ? ` · ${o.task}` : ""}</span><span class="k">{noteTime(o.at)}</span></div>
              {/each}
            {/if}
          {/if}
        {:else}
          <div class="nempty">{tt("bell.empty")}</div>
        {/each}
      </div>
      <div class="nfoot">{tt("bell.foot")}</div>
  </div>
{/if}

{#if !sidebar}<div class="drag top" style:left="44px" data-tauri-drag-region></div>{/if}

{#if paneDrag?.zone}
  {@const z = paneDrag.zone}
  <div class="dropzone" style:left="{z.left}px" style:top="{z.top}px" style:width="{z.width}px" style:height="{z.height}px"><span>{z.label}</span></div>
{/if}
{#if (owner || compareHelper) && ws}
  {@const parent = compareOwner ?? owner}
  {@const helper = compareHelper ?? tab}
  {#if parent && helper}
    <div class="helper-head" style:left={sidebar ? `${sideW + 44}px` : "44px"} style:top={sidebar ? "0" : "28px"} style:right={changes ? `${panelW}px` : "0"}>
      <button class="mini" onclick={() => openTab(at.ws, parent)}>← {title(parent)}</button>
      <span>› {simple ? tt("helper.simple", { name: title(parent) }) : title(helper)}</span>
      <button class="mini" onclick={toggleCompare}>{compareId ? tt("helper.unsplit") : simple ? tt("helper.compareSimple", { name: title(parent) }) : tt("helper.compare")}</button>
    </div>
  {/if}
{/if}
<div class="grid" style:left={sidebar ? `${sideW + 44}px` : "44px"} style:top={sidebar ? (owner || compareHelper ? "32px" : "0") : (owner || compareHelper ? "60px" : "28px")} style:right={changes ? `${panelW}px` : "0"} bind:this={box}>
  {#if ws && !tab}
    <div class="empty">
      <span>{tt("main.wsEmpty", { name: ws.name })}</span>
      <button class="mini" onclick={openPicker}>+ Tab <span class="keys">⌘T</span></button>
    </div>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
  {#each wss as w, wi (w.id)}
    {#each w.tabs as t, ti (t.id)}
      {@const compared = !!compareId && at.ws === wi && (t.id === compareId || t.id === compareOwner?.id)}
      {@const active = (at.ws === wi && at.tab === ti) || compared}
      {#each [...rects(t.tree)] as [pane, r] (pane)}
        {@const hidden = !active || (compared && pane !== t.focus) || (zoomed && pane !== t.focus)}
        {@const rr = compared ? { x: t.id === compareOwner?.id ? 0 : 0.5, y: 0, w: 0.5, h: 1 } : zoomed ? { x: 0, y: 0, w: 1, h: 1 } : r}
        <!-- FR-55: toolbar for agent panes from a profile, and agents typed by hand while running -->
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
          onpointerdown={() => { t.focus = pane; if (compared) at = { ws: wi, tab: ti }; }}
          role="presentation"
        >
          {#if dead[pane]}
            <div class="dead">
              <span>{tt("pane.dead")}</span>
              <span class="keys">{meta[pane]?.name || meta[pane]?.profile} · {home(meta[pane]?.cwd) || "~"}</span>
              <button class="mini" onclick={() => restart(pane)}>{tt("pane.restart", { name: meta[pane]?.profile ?? "shell" })}</button>
            </div>
          {:else}
            {#if head}
              <!-- FR-37: thin head, also when the tab has only one pane (Gembala.pen 02) -->
              {@const hs = stat(pane)}
              <div class="head" class:on={split && pane === t.focus} onpointerdown={(e) => startPaneDrag(e, t, pane)} role="presentation">
                <span class="hname">{#if stText(hs)}<span class="dot {hs}" aria-hidden="true"></span>{/if}{meta[pane]?.name}{labels[pane] || meta[pane]?.busy ? ` · ${labels[pane] || meta[pane]?.busy}` : ""}</span>
                {#if stText(hs)}<span class="st {hs}">{paneStatusText(pane)}</span>{/if}
                <!-- FR-27: token saver on for this pane (read, test log, artifacts are reduced) -->
                {#if meta[pane]?.saver}<span class="hsaver" title={tt("pane.saverTip")}>{tt("pane.saver")}</span>{/if}
                <!-- FR-78: add-ons of the running agent in this workspace, names in the tooltip -->
                {#if addonsOf(pane).length}<span class="hadd" title="Add-on: {addonsOf(pane).join(', ')}">{ttn("pane.addons", addonsOf(pane).length)}</span>{/if}
                <!-- the folder once is enough: the agent toolbar already shows it -->
                {#if !(bar && [...toolbar.left, ...toolbar.right].includes("cwd"))}<span class="hcwd" title={home(meta[pane]?.cwd)}>{short(meta[pane]?.cwd)}</span>{/if}
                <span class="grow"></span>
                {#if !compared}
                  <button class="hb" title="{tt('drag.right')} ⌘D" aria-label={tt("drag.right")} onclick={() => splitFocused("row")}>◫</button>
                  <button class="hb" title="{tt('drag.down')} ⌘⇧D" aria-label={tt("drag.down")} onclick={() => splitFocused("col")}>⊟</button>
                {/if}
                <button class="hb" title="{tt('menu.closePane')} ⌘W" aria-label={tt("menu.closePane")} onclick={() => askClose(pane)}>×</button>
              </div>
            {/if}
            {@const ed = editorShown(pane)}
            <div class="body" class:withbar={bar} class:withhead={head} class:withedit={ed} oncontextmenu={(e) => paneMenu(e, pane)} role="presentation">
              {#if t.parent ? active : liveTabs.includes(t.id) || compared}
              <Tile bind:this={tiles[pane]} {pane} codexPane={agentOf(pane) === "codex"} focused={at.ws === wi && at.tab === ti && pane === t.focus && lempar?.pane !== pane && rich?.pane !== pane && !ed} {ui} zoom={zoom + (meta[pane]?.zoom ?? 0)} {onexit} {onlink}
                onpasted={(path: string) => meta[pane] && (meta[pane].pastes = [...(meta[pane].pastes ?? []), path])}
                online={(line: string) => onLine(pane, line)} onkey={(key: string) => onMentionKey(pane, key)}
                onprompt={(on: boolean) => ((atPrompt[pane] = on), on || (ran[pane] = true))}
                onused={() => (ran[pane] = true)}
                ondone={(cmd: string, code: number) => (offer[pane] = askOffer(cmd, code) ? cmd.trim() : "")}
                onblock={(kind: string, text: string) => {
                  if (kind === "output") return void (outputOf = pane);
                  // last session of an agent pane from a profile: rerun its pane (official hooks), not typed into the shell
                  if (kind === "run" && meta[pane]?.ended?.resume === text && AGENTS.includes(meta[pane].agent)) return void resumeEnded(pane);
                  // FR-56 timeline: Resume/Rerun = run the command in this pane (through the editor so it lands in history)
                  if (kind === "run" && editorShown(pane)) return void ((draft[pane] = text), runDraft(pane));
                  if (kind !== "reuse") return void copy(text);
                  if (!editorShown(pane)) return void copy(text); // editor not shown (e.g. a TUI running): just copy
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
              {/if}
            </div>
            {#if ed && !ran[pane] && !draft[pane]}
              <!-- FR-59: Start panel: gone once a command runs or typing starts -->
              <div class="start" class:withhead={head}>
                {#if welcome && !askMode && pane === startHere}{@render welcomeCard("wcard")}{/if}
                <!-- FR-99 P1: the welcome card already holds the agent buttons and the folder: no second set under it -->
                <div class="scol" class:gone={welcome && !askMode && pane === startHere}>
                  <span class="wm">{tt("start.title")}</span>
                  {#if startProfiles.length}
                    <div class="starts">
                      {#each startProfiles as p (p.key)}
                        <button class="sbtn" onclick={() => swapToProfile(pane, p.id, p.account)}><span class="aico {p.agent}">{GLYPH[p.agent] ?? "›"}</span>{p.label}</button>
                      {/each}
                    </div>
                  {/if}
                  <div class="starts">
                    <button class="sbtn" onclick={() => startInFolder(pane)}>{@render icon("folder")} {tt("start.cd")}</button>
                    {#if pickable.length}
                      <button class="sbtn" onclick={(e) => popup(e, pickable.map((p) => [p.label, () => swapToProfile(pane, p.id, p.account, true)] as MItem))}
                        >{@render icon("history")} {tt("act.pickSession")}</button
                      >
                    {/if}
                  </div>
                </div>
              </div>
            {/if}
            {#if ed}
              <!-- FR-59: input editor; cursor and selection with the mouse, Enter runs, Tab/Esc to the terminal -->
              <div class="editor" class:withbar={bar}>
                <!-- Warp-style context: folder and branch, replacing the shell prompt -->
                <div class="ctx">
                  <button class="cchip" title={tt("ed.attachTip")} aria-label={tt("chip.attach")}
                    onclick={async () => insertPaths(pane, await invoke<string[]>("pick_files").catch(() => [] as string[]))}>{@render icon("plus")}</button>
                  <!-- FR-59: klik = pemilih folder ala Warp -->
                  <button class="cchip dir" title="{tt('ed.cdTip')} · {meta[pane]?.cwd ?? ''}" onclick={() => openDirs(pane)}>
                    <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M1.5 3.5h4l1.5 1.5h7.5v8h-13z" fill="none" stroke="currentColor" /></svg>
                    {short(meta[pane]?.cwd) || "~"}
                  </button>
                  {#if meta[pane]?.git?.repo && meta[pane].git.branch}
                    <span class="cchip">⎇ {meta[pane].git.branch}{meta[pane].git.files ? ` · ${ttn("chg.files", meta[pane].git.files)}` : ""}</span>
                  {/if}
                  <span class="ehint" class:off={!active || pane !== t.focus}>{tt("ed.hint")}</span>
                </div>
                {#if dirs?.pane === pane}
                  <div class="mention complist acct dirs" role="listbox" aria-label={tt("ed.pickFolder")}>
                    <!-- svelte-ignore a11y_autofocus -->
                    <input class="dq" placeholder={tt("ed.findFolder")} bind:value={dirs.q} autofocus spellcheck="false" autocapitalize="off"
                      onkeydown={dirsKey} oninput={dirsInput} onblur={() => (dirs = null)} />
                    {#each dirItems as n, i (n)}
                      <button class="opt" class:on={i === dirs.sel} role="option" aria-selected={i === dirs.sel}
                        onpointerdown={(e) => e.preventDefault()} onclick={() => cdTo(n)}>
                        <span class="ico" aria-hidden="true">{#if n === ".."}↑{:else}{@render icon("folder")}{/if}</span>
                        <span>{n === ".." ? `.. (${tt("ed.parent")})` : n}</span><span></span>
                      </button>
                    {/each}
                    {#if !dirItems.length}<span class="keys">{tt("ed.noFolder")}</span>{/if}
                  </div>
                {/if}
                {#if comp?.pane === pane}
                  <!-- FR-59: daftar Tab ala Warp -->
                  <div class="mention complist" class:acct={comp.auto} role="listbox" aria-label={tt("ed.completions")}>
                    {#each comp.items as it, i (it.label)}
                      <button class="opt" class:on={i === comp.sel} role="option" aria-selected={i === comp.sel}
                        onpointerdown={(e) => e.preventDefault()} onclick={() => ((draft[pane] = it.insert), (comp = null))}>
                        {#if comp.auto}
                          <!-- FR-61 variant C (mockup-pilih-akun): name on top, small email/command below -->
                          <span class="ico claude" aria-hidden="true">{GLYPH.claude}</span>
                          <span class="two"><span>{it.label}</span><small>{it.kind}</small></span>
                          <span class="keys">{i === comp.sel ? "Tab" : ""}</span>
                        {:else}
                          <span class="ico" aria-hidden="true">{({ Folder: "▸", Perintah: "›", Branch: "⎇", Script: "▶" } as Record<string, string>)[it.kind] ?? "·"}</span>
                          <span>{it.label}</span>
                          <span class="keys">{tOr(`comp.${it.kind}`, it.kind)}</span>
                        {/if}
                      </button>
                    {/each}
                  </div>
                {/if}
                <!-- a clear input box: border + ❯, blue border on focus -->
                <div class="ebox">
                <span class="ps" aria-hidden="true">❯</span>
                <div class="ghostwrap">
                  <!-- gray suggestion from history; → accepts -->
                  <div class="ghost" aria-hidden="true"><span class="typed">{draft[pane] ?? ""}</span>{ghost(pane)}</div>
                <textarea bind:value={draft[pane]} rows={Math.min(6, (draft[pane] ?? "").split("\n").length)} data-editor={pane}
                  aria-label={tt("ed.aria", { name: meta[pane]?.name ?? "" })} spellcheck="false" autocapitalize="off" autocomplete="off"
                  placeholder={tt("ed.ph")}
                  onkeydown={(e) => editorKey(pane, e)} oninput={() => onEditorInput(pane)} onfocus={() => (t.focus = pane)}
                  onpaste={(e) => pasteToEditor(pane, e)}
                  oncopy={(e) => {
                    // ⌘C without a selection in the editor = copy the terminal selection (focus is always in the editor while it's shown)
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
              <!-- FR-57: @ list; Tab picks, ↑↓ moves, Esc closes, keep typing = the @ belongs to the agent -->
              <div class="mention" class:withbar={bar} class:withedit={mention.editor} role="listbox" aria-label={tt("men.aria")}>
                {#if !mentionItems.length}
                  <!-- shown even when empty: without this @@ looks broken -->
                  <span class="keys">{targets.length ? tt("men.noMatch", { q: mention.q }) : tt("men.none")}</span>
                {/if}
                {#each mentionItems as r, i (r.pane)}
                  <button class="opt" class:on={i === mention.sel} role="option" aria-selected={i === mention.sel}
                    onpointerdown={(e) => e.preventDefault()} onclick={() => chooseMention(i)}>
                    <span class="dot {r.status}"></span>
                    <span>{r.name}</span>
                    <span class="keys">{r.agent === "shell" ? (meta[r.pane]?.busy ?? "shell") : r.agent} · {stText(r.status) || r.status || "?"}</span>
                  </button>
                {/each}
                {#if mentionItems.length}<span class="keys"
                    >{mention.picked ? tt("men.hintMore") : mention.inline ? tt("men.hintInline") : tt("men.hint")}</span
                  >{/if}
              </div>
            {/if}
            {#if lempar?.pane === pane}
              <div class="lempar" class:withbar={bar}>
                <span class="to">→ {lempar.name}</span>
                <!-- svelte-ignore a11y_autofocus -->
                <textarea bind:value={lempar.body} rows="2" autofocus aria-label={tt("men.msgAria", { name: lempar.name })} placeholder={tt("men.msgPh")}
                  spellcheck="false" autocapitalize="off"
                  onkeydown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) (e.preventDefault(), sendLempar());
                    if (e.key === "Escape") (e.preventDefault(), (lempar = null), tiles[pane]?.focus());
                  }}></textarea>
                {#if lempar.error}
                  <span class="err">{errText(lempar.error)}</span>
                  {#if lempar.error === "target_busy"}<button class="mini" onclick={() => sendLempar(true)}>{tt("men.force")}</button>{/if}
                {/if}
              </div>
            {/if}
            {#if meta[pane]?.login}
              <!-- FR-61: Gembala types nothing; the user logs in to Claude Code in this pane -->
              <div class="ended" class:withhead={head} role="status">
                <span class="etitle">{tt("pane.login", { name: meta[pane].login })} <code>/login</code></span>
                <span class="grow"></span>
                <button class="hb" aria-label={tt("common.close")} onclick={() => delete meta[pane].login}>×</button>
              </div>
            {/if}
            <!-- FR-56: finished agent session buttons live in the timeline block (pen screen 02), no banner above the pane -->
            {#if bar}
              <!-- FR-55: agent toolbar; chips without data (not a repo) are hidden -->
              <!-- unfocused pane in a split tab: toolbar content hidden (height stays, the terminal doesn't reflow), shown on hover -->
              <div class="bar" class:rest={split && !zoomed && pane !== t.focus}>
                {#if editing?.kind === "pane" && editing.id === String(pane)}
                  <!-- svelte-ignore a11y_autofocus -->
                  <input class="edit pname" bind:value={editing.value} onkeydown={(e) => e.key === "Enter" && saveEdit()} onblur={saveEdit} autofocus aria-label={tt("pane.name")} />
                {:else}
                  <button class="chip pname" title={tt("pane.renameTip")} ondblclick={() => (editing = { kind: "pane", id: String(pane), value: meta[pane]?.name ?? "" })}>{#if agentOf(pane)}<span class="aico {agentOf(pane)}">{GLYPH[agentOf(pane)]}</span>{/if}{meta[pane]?.name}</button>
                {/if}
                {#if acctOf(pane)}<span class="chip acc c-acc" title={tt("pane.accountTip")}>{acctOf(pane)}</span>{/if}
                {#if meta[pane]?.owner && agentOf(pane) === "claude"}<span class="chip rwarn" title={tt("pane.ownerTip", { name: meta[pane].owner.name })}>{tt("pane.owner", { name: meta[pane].owner.name })}</span>{/if}
                {#if meta[pane]?.roleWarn}<span class="chip rwarn" title={tt("pane.roleWarnTip")}>{tt("pane.roleWarn")}</span>{/if}
                {#each toolbar.left as c}{@render chip(c, pane, g)}{/each}
                {#if meta[pane]?.output}<button class="chip" onclick={() => (outputOf = pane)}>{tt("pane.output")}</button>{/if}
                {#if meta[pane]?.prev}<button class="chip" onclick={() => resumePrev(pane)}>{tt("pane.resumePrev", { agent: meta[pane].prev.agent })}</button>{/if}
                {#if meta[pane]?.sig && meta[pane].sig !== sig(meta[pane].profile)}
                  <button class="chip changed" onclick={() => relaunch(pane)}>{tt("pane.changed")}</button>
                {/if}
                <span class="grow"></span>
                {#each toolbar.right as c}{@render chip(c, pane, g)}{/each}
                <!-- FR-101 (5): narrow pane = chips give way (CSS container query on .bar); everything stays reachable here -->
                <button class="chip box c-more" aria-label={tt("chip.more")} onclick={(e) => popup(e, barMenu(pane, g))}>⋯</button>
              </div>
            {/if}
            {#if outputOf === pane}
              <OutputView {pane} onclose={() => (outputOf = null)} />
            {/if}
            {#if rich?.pane === pane}
              <!-- pen 04: titled card; attachments as chips, sent as quoted paths after the text -->
              {@const agName = cfg?.values.profiles.find((p) => p.agent === agentOf(pane))?.name}
              <div class="rich" role="dialog" aria-label="Rich Input" tabindex="-1" onkeydown={(e) => e.key === "Escape" && (e.preventDefault(), (rich = null))}>
                <div class="rhead"><b>Rich Input</b><span class="rto">{tt("rich.to", { name: meta[pane]?.name ?? "" })}{agName ? ` · ${agName}` : ""}</span><span class="n">⌘G</span></div>
                <!-- svelte-ignore a11y_autofocus -->
                <textarea
                  bind:value={rich.text}
                  autofocus
                  rows="4"
                  aria-label="Rich Input"
                  placeholder={tt("rich.ph")}
                  spellcheck="false"
                  autocapitalize="off"
                  onpaste={(e) => pasteToEditor(pane, e, (p) => addRichFiles([p]))}
                  onkeydown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) (e.preventDefault(), sendRich());
                  }}
                ></textarea>
                {#if rich.files?.length}
                  <div class="rfiles">
                    {#each rich.files as f (f)}
                      <span class="rfile" title={f}>{@render icon("file")}{f.split("/").pop()}<button aria-label={tt("set.tb.remove", { chip: f.split("/").pop() ?? "" })}
                          onclick={() => rich && (rich.files = rich.files?.filter((x) => x !== f))}>×</button></span>
                    {/each}
                  </div>
                {/if}
                {#if rich.error}<span class="rerr">{errText(rich.error, rich.args)}</span>{/if}
                <div class="rfoot">
                  <button class="mini" onclick={async () => addRichFiles(await invoke<string[]>("pick_files").catch(() => []))}>{tt("rich.attach")}</button>
                  <span class="rhint">{tt("rich.ctxHint")}</span>
                  <span class="rhint">{tt("rich.hint")}</span>
                  <button class="mini pri" onclick={sendRich}>{tt("common.send")} <span class="keys">Enter</span></button>
                </div>
              </div>
            {/if}
          {/if}
          {#if pendingFor(pane).length || opensFor(pane).length || closesFor(pane).length || offer[pane] || reportsFor(pane).length}
          <!-- FR-98 bug: the cards stack in one column instead of sitting on top of each other -->
          <div class="cards">
          {#each reportsFor(pane) as r (r.id)}
            <!-- FR-100: an agent drafted a GitHub issue; nothing is sent until the user opens it and presses Submit on GitHub -->
            <div class="card offer" role="alert">
              <span>{tt("rep.card", { from: r.from })} <b>{r.title}</b></span>
              <span class="sub">{tt("rep.cardSub")}</span>
              <span class="acts">
                <button class="mini" onclick={() => call("report.discard", { id: r.id }).then(refreshMsgs)}>{tt("rep.discard")}</button>
                <button class="mini pri" onclick={() => (reportDraft = { title: r.title, body: r.body, from: r.from, id: r.id })}>{tt("rep.review")}</button>
              </span>
            </div>
          {/each}
          {#if offer[pane]}
            <!-- FR-99 P1: the sentence went to zsh; offer it to an agent instead -->
            <div class="card offer" role="alert">
              <span>{offerAgent ? tt("offer.ask", { name: offerAgent.name }) : tt("offer.noAgent")}</span>
              <span class="acts">
                <button class="mini" onclick={() => delete offer[pane]}>{offerAgent ? tt("offer.no") : tt("common.close")}</button>
                <button class="mini pri" onclick={() => sendOffer(pane)}>{offerAgent ? tt("offer.send") : tt("offer.install")}</button>
              </span>
            </div>
          {/if}
          {#each pendingFor(pane) as m (m.id)}
            <!-- FR-12: card near the target pane; click only (Enter doesn't approve by accident) -->
            <div class="card" role="alertdialog" aria-label={tt("msg.pending")}>
              <span><b>{m.from}</b> {tt("card.wants")} <span class="keys">{m.id}</span></span>
              <pre>{m.body}</pre>
              <span class="acts">
                <button class="mini" onclick={() => call("msg.reject", { id: m.id }).then(refreshMsgs)}>{tt("msg.reject")}</button>
                <button class="mini" onclick={() => call("msg.approve", { id: m.id }).then(refreshMsgs)}>{tt("msg.approve")}</button>
              </span>
            </div>
          {/each}
          {#each opensFor(pane).slice(0, 1) as o (o.id)}
            <!-- FR-75 (pen screen 15): one card at a time, the rest counted; click only, like FR-12 -->
            {@const n = opensFor(pane).length}
            {@const openAgent = agentForOpen(o)}
            <div class="card" role="alertdialog" aria-label={tt("open.title", { who: openWho(o) })}>
              <b>{tt("open.title", { who: openWho(o) })}{n > 1 ? ` · ${tt("open.more", { n: n - 1 })}` : ""}</b>
              <span>{tt("open.ask", { from: o.from, who: openWho(o) })}</span>
              <pre>{o.body}</pre>
              {#if ["claude", "codex", "omp", "opencode", "gemini"].includes(openAgent ?? "")}
                <div class="sub"><span>{tt("open.modelEffort")}</span>
                  <select aria-label={tt("open.modelEffort")} value={openModels.includes(o.model) ? o.model : ""} onchange={(e) => (o.model = e.currentTarget.value)}>
                    <option value="">{tt("set.acc.default.opt")}</option>{#each openModels as model (model)}<option value={model}>{model}</option>{/each}
                  </select>
                  <input aria-label={tt("set.acc.modelAria", { name: openWho(o) })} value={o.model} placeholder={tt("open.customModel")} oninput={(e) => (o.model = e.currentTarget.value)} />
                  {#if ["claude", "codex", "omp"].includes(openAgent ?? "")}
                    <select aria-label={tt("open.effort")} bind:value={o.effort}>
                      <option value="">{tt("set.acc.default.opt")}</option>{#each openEfforts as effort (effort)}<option value={effort}>{effort}</option>{/each}
                    </select>
                  {:else}
                    <span>{tt("open.effortUnsupported", { agent: openAgent ?? o.profile })}</span>
                  {/if}
                </div>
              {:else}
                <span class="sub">{tt("open.modelUnsupported", { agent: openAgent ?? o.profile })}</span>
              {/if}
              <span class="sub" title={o.cwd}>{tt("open.where", { cwd: short(o.cwd) })}</span>
              <span class="acts">
                <button class="mini" onclick={() => call("open.reject", { id: o.id }).then(refreshMsgs)}>{tt("msg.reject")}</button>
                <button class="mini" onclick={() => approveOpen(o)}>{tt("open.go")}</button>
              </span>
            </div>
          {/each}
          {#each closesFor(pane) as c (c.id)}
            <!-- FR-86: an agent asked to close another pane. Only this card closes it (4 Nov: it used to type "/exit" into it) -->
            <div class="card" role="alertdialog" aria-label={tt("close.reqTitle", { from: c.from, name: c.target })}>
              <b>{tt("close.reqTitle", { from: c.from, name: c.target })}</b>
              <span class="sub">{tt("close.reqAsk")}</span>
              {#if c.status === "working" || c.status === "blocked"}
                <span class="sub">{tt(c.status === "working" ? "close.reqBusy" : "close.reqBlocked")}</span>
              {/if}
              <span class="acts">
                <button class="mini" onclick={() => call("close.reject", { id: c.id }).then(refreshMsgs)}>{tt("close.reqStay")}</button>
                <button class="mini danger" onclick={() => call("close.approve", { id: c.id }).then(refreshMsgs)}>{tt("close.reqGo")}</button>
              </span>
            </div>
          {/each}
          </div>
          {/if}
          {#if confirm?.pane === pane}
            <div class="confirm ask" role="alertdialog" aria-label={tt("close.aria")}>
              <b class="ctitle">{tt("close.title", { name: confirm.what === "pane" ? meta[pane]?.name || "pane" : confirm.what })}</b>
              <span>{tt("close.running1")} <b>{confirm?.name}</b>{tt("close.running2")}</span>
              <span class="acts">
                <button class="mini" onclick={() => (confirm = null)}>{tt("common.cancel")} <span class="keys">Esc</span></button>
                <button class="mini danger" onclick={() => { confirm?.close.forEach((p) => call("pane.close", { pane: p }).catch(() => {})); confirm = null; }}>{tt("common.close")} <span class="keys">Enter</span></button>
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
      title={tt("main.dividerTip")}
      role="separator"
      aria-orientation={d.dir === "row" ? "vertical" : "horizontal"}
    ></div>
  {/each}
  {#if upgrade}
    <div class="confirm ask" role="alertdialog" aria-label={tt("up.aria")}>
      <b class="ctitle">{tt("up.title")}</b>
      <span>{tt("up.versions", { daemon: upgrade.daemon, app: upgrade.app })}</span>
      <span>{ttn("up.panes", upgrade.panes)}</span>
      <span class="acts">
        <button class="mini" onclick={() => (upgrade = null)}>{tt("up.later")}</button>
        <button class="mini pri" onclick={() => ((upgrade = null), call("daemon.stop").catch(() => {}))}>{tt("up.restart")}</button>
      </span>
    </div>
  {/if}
  {#if msgData?.paused}
    <div class="paused" role="status">{tt("main.paused")}</div>
  {/if}
  {#if flow && ws && runs[ws.id]}
    <HarnessFlow snap={runs[ws.id]} has={(p: number) => !!meta[p]} onchange={(s: Snap) => onRun(s)} onjump={jumpTo} onchanges={(p: number) => ((flow = false), showChanges(p))} onclose={() => (flow = false)} />
  {/if}
  {#if newTask && ws}
    <NewTask
      ws={{ id: ws.id, name: ws.name }}
      cwd={absPath(meta[tab?.focus ?? -1]?.cwd ?? ws.root)}
      brief={ws.brief}
      agents={installed}
      {simple}
      names={Object.fromEntries((cfg?.values.profiles ?? []).filter((p) => p.agent !== "shell").map((p) => [p.agent, `${GLYPH[p.agent] ?? ""} ${p.name}`.trim()]))}
      ontemplate={() => ((newTask = false), openTemplate())}
      onclose={() => (newTask = false)}
      onstarted={(s: Snap) => {
        if (ws.id === s.workspace && (owner ?? tab)) runOwner[s.run] = (owner ?? tab)!.id;
        newTask = false;
        drawer = false;
        onRun(s);
        flow = true;
      }}
      onmanual={startManual}
    />
  {/if}
  {#if reportDraft}
    {@const d = reportDraft}
    <Report draft={d} version={appVer} mode={simple ? "Simple" : "Full"} home={homePath} onclose={() => (reportDraft = null)}
      onsent={() => ((reportDraft = null), d.id && call("report.discard", { id: d.id }).then(refreshMsgs))} />
  {/if}
  {#if drawer}
    <Messages data={msgData} {rows} models={openModels} efforts={openEfforts} agentOf={agentForOpen} onchange={refreshMsgs} onclose={() => (drawer = false)} onopen={approveOpen} who={openWho} />
  {/if}
  {#if compose}
    <div class="confirm compose" role="dialog" aria-label={tt("cmp.title")}>
      <b>{tt("cmp.title")}</b>
      <div class="targets" role="listbox" aria-label={tt("cmp.targets")}>
        {#each targets as r (r.pane)}
          <button class="opt" class:on={compose.to === r.pane} role="option" aria-selected={compose.to === r.pane} onclick={() => compose && (compose.to = r.pane)}>
            <span class="dot {r.status}"></span>
            <span>{r.name}</span>
            <span class="keys">{r.agent === "shell" ? (meta[r.pane]?.busy ?? "shell") : r.agent} · {stText(r.status) || r.status || "?"}</span>
          </button>
        {:else}
          <span class="keys">{tt("cmp.none")}</span>
        {/each}
      </div>
      <!-- svelte-ignore a11y_autofocus -->
      <textarea bind:value={compose.body} rows="5" autofocus aria-label={tt("cmp.body")} placeholder={tt("cmp.bodyPh")} spellcheck="false" autocapitalize="off"
        onkeydown={(e) => e.key === "Enter" && e.metaKey && sendCompose()}></textarea>
      <label class="inline"><input type="checkbox" bind:checked={compose.force} /> {tt("cmp.force")}</label>
      {#if compose.error}<span class="err">{errText(compose.error)}</span>{/if}
      <span class="acts"><button class="mini" onclick={() => (compose = null)}>{tt("common.cancel")}</button> <button class="mini" onclick={sendCompose}>{tt("common.send")} ⌘↩</button></span>
    </div>
  {/if}
  {#if changes}
    <Changes status={meta[changes.pane]?.git} diff={changes.diff} width={panelW} onwidth={(w: number) => (panelW = w)}
      pname={meta[changes.pane]?.name ?? ""} turns={changes.turns ?? []} turn={changes.turn ?? null} ask={changes.ask ?? null}
      onturn={pickTurn} onrestore={askRestore} onconfirm={doRestore} oncancel={() => changes && (changes.ask = null)}
      act={changes.act ?? null} onact={askFiles} onactdo={doFiles} onactcancel={() => changes && (changes.act = null)}
      onreload={() => changes && loadChanges(changes.pane)} onclose={() => (changes = null)} />
  {/if}
  {#if cfg?.errors.length && cfgSeen !== JSON.stringify(cfg.errors)}
    <!-- FR-42 / pen 13: a broken config is visible without opening Settings; dismissed until the errors change -->
    <div class="confirm ask warn" role="alert">
      <b class="ctitle">{tt("main.cfgErrors")}</b>
      {#each cfg.errors as e}<code class="url">{errText(e)}</code>{/each}
      <span class="acts">
        <button class="mini" onclick={() => cfg && (cfgSeen = JSON.stringify(cfg.errors))}>{tt("common.close")}</button>
        <button class="mini" onclick={() => cfg && ((cfgSeen = JSON.stringify(cfg.errors)), (settingsOpen = true))}>{tt("main.openSettings")}</button>
      </span>
    </div>
  {/if}
  {#if renameError}
    <div class="confirm" role="alert">
      <span>{renameError}</span>
      <button class="mini" onclick={() => (renameError = "")}>{tt("common.ok")}</button>
    </div>
  {/if}
  {#if wsAsk}
    <div class="confirm ask" role="alertdialog" aria-label={tt("wsdel.aria")}>
      <b class="ctitle">{tt("wsdel.title", { name: wss[wsAsk.wi]?.name ?? "" })}</b>
      <span>{ttn("wsdel.busy", wsAsk.busy, { name: wss[wsAsk.wi]?.name ?? "" })}</span>
      <span class="acts">
        <button class="mini" onclick={() => (wsAsk = null)}>{tt("common.cancel")} <span class="keys">Esc</span></button>
        <button class="mini danger" onclick={() => wsAsk && removeWorkspace(wsAsk.wi)}>{tt("common.delete")} <span class="keys">Enter</span></button>
      </span>
    </div>
  {/if}
  {#snippet welcomeCard(cls: string)}
    <!-- FR-43 / FR-99 P1: every agent found is a button that opens it; none found = how to install each (no fake "Start") -->
    {@const found = (welcome ?? []).filter((a) => a.installed)}
    <div class="{cls} welcome" role="dialog" aria-label={tt("wel.aria")}>
      <span class="whead">
        <b class="wtitle">{found.length ? (simple ? tt("wel.ask") : tt("wel.found")) : tt("wel.noneTitle")}</b>
        <button class="mini wx" aria-label={tt("common.close")} onclick={() => (welcome = null)}>×</button>
      </span>
      {#if found.length}
        {#if simple}<span class="wm">{tt("wel.askSub")}</span>{/if}
        {#each found as a, i (a.agent)}
          <button class="wopen" class:pri={i === 0} onclick={() => openAgent(a.agent)}
            ><span class="aico {a.agent}">{GLYPH[a.agent] ?? "›"}</span>{tt("wel.open", { name: a.name })}{#if !simple}<span class="wver" class:untested={!a.tested}
                >{a.version ?? ""}{a.tested ? "" : ` · ${tt("set.agent.notTried")}`}</span
              >{/if}</button
          >
        {/each}
        {#if startHere !== null}
          <!-- FR-99 P1: where the agent will work, instead of the Start panel's folder chip -->
          {@const dir = meta[startHere]?.cwd ?? ""}
          <span class="wrow wdir" title={dir}>{tt("wel.folder", { name: dir.split("/").filter(Boolean).pop() ?? "~" })}<button class="mini" onclick={() => startHere !== null && startInFolder(startHere)}>{tt("hn.pick")}</button></span>
        {/if}
        {#if !simple}
          <span class="wrow"><kbd class="wkey">⌘P</kbd>{tt("wel.p")}</span>
          <span class="wrow"><kbd class="wkey">{tt("wel.boxKey")}</kbd>{tt("wel.box")}</span>
          <span class="wrow"><kbd class="wkey">@@</kbd>{tt("wel.at")}</span>
        {/if}
      {:else}
        <span class="wm">{tt("wel.noneSub")}</span>
        {#each welcome ?? [] as a, i (a.agent)}
          {@render installRow(a, i === 0)}
        {/each}
        <span class="acts"><button class="mini" onclick={redetect}>{installing ? tt("wel.checking") : tt("wel.recheck")}</button></span>
      {/if}
    </div>
  {/snippet}
  <!-- FR-99 P1: one agent that isn't installed: type its install command into a new terminal tab (the user presses Enter), copy it, or read the official guide -->
  {#snippet installRow(a: Agent, first: boolean)}
    <span class="wrow inst">
      <span class="aico {a.agent}">{GLYPH[a.agent] ?? "›"}</span><span class="grow">{a.name}{#if !simple}<code class="wcmd">{a.install}</code>{/if}</span>
      <button class="mini" class:pri={first} disabled={!a.install} onclick={() => installInTerminal(a)}>{tt("wel.install")}</button>
      <button class="mini" disabled={!a.install} onclick={() => navigator.clipboard?.writeText(a.install).catch(() => {})}>{tt("wel.copy")}</button>
      {#if GUIDE[a.agent]}<button class="mini" onclick={() => (linkAsk = GUIDE[a.agent])}>{tt("wel.guide")} ↗</button>{/if}
    </span>
  {/snippet}
  <!-- FR-97: asked once while ui.mode is "ask"; a click picks, Esc = Full and it isn't asked again -->
  {#if askMode}
    <div class="confirm ask modeq" role="dialog" aria-label={tt("mode.ask")}>
      <b class="wtitle">{tt("mode.ask")}</b>
      <span class="wm">{tt("mode.askSub")}</span>
      <button class="wopen" onclick={() => setMode("full")}><b>{tt("mode.full")}</b><small>{tt("mode.fullSub")}</small></button>
      <button class="wopen" onclick={() => setMode("simple")}><b>{tt("mode.simple")}</b><small>{tt("mode.simpleSub")}</small></button>
    </div>
  {/if}
  <!-- with the Start panel showing, the card sits above it inside the pane (pen 01) -->
  {#if welcome && !askMode && startHere === null}{@render welcomeCard("confirm")}{/if}
  {#if linkAsk}
    <div class="confirm ask" role="alertdialog" aria-label={tt("link.aria")}>
      <b class="ctitle">{tt("link.title")}</b>
      <code class="url">{linkAsk}</code>
      <span class="acts">
        <button class="mini" onclick={() => (linkAsk = "")}>{tt("common.cancel")} <span class="keys">Esc</span></button>
        <button class="mini" onclick={openLink}>{tt("common.open")} <span class="keys">Enter</span></button>
      </span>
    </div>
  {/if}
  {#if tpl}
    <!-- FR-13: role template dialog (pen screen 17) -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div class="confirm tpldlg" role="dialog" aria-label={tt("tpl.title")} tabindex="-1" onkeydown={(e) => e.key === "Escape" && (tpl = null)}>
      <span class="thd"><b>{tt("tpl.title")}</b><button class="mini" onclick={() => (tpl = null)} aria-label={tt("common.close")}>×</button></span>
      <div class="tpls">
        {#each TEMPLATES as t, i (t.key)}
          <button class="tplc" class:on={tpl.t === i} onclick={() => pickTemplate(i)}><span>{tt(`tpl.${t.key}` as Key)}</span><span class="m">{tt(`tpl.${t.key}Desc` as Key)}</span></button>
        {/each}
      </div>
      <div class="roles">
        <div class="rrow rh"><span>{tt("tpl.role")}</span><span>Agent</span><span>{tt("tpl.limit")}</span><span>{tt("pane.name")}</span></div>
        {#each tpl.rows as r, i (i)}
          {@const lim = limitOf(r.role, r.prof)}
          <div class="rrow">
            <span>{roleLabel(r.label)}</span>
            <select class="edit" bind:value={r.prof}>
              {#each startProfiles as p, j (p.key)}<option value={j}>{GLYPH[p.agent] ?? "›"} {p.label}</option>{/each}
            </select>
            <span class="lim {lim}">{tt(`tpl.lim.${lim}`)}</span>
            <input class="edit" bind:value={r.name} spellcheck="false" autocapitalize="off" />
          </div>
        {/each}
      </div>
      <div class="brief">
        <b>{tt("tpl.brief", { name: wss[tpl.wi]?.name ?? "" })}</b>
        <span class="m">{tt("tpl.briefSub")}</span>
        <textarea class="edit" bind:value={tpl.brief} rows="5" maxlength="4000" spellcheck="false" placeholder={tt("tpl.briefPh")}></textarea>
      </div>
      <span class="acts">
        <span class="m">{ttn("tpl.foot", tpl.rows.length)}</span>
        <button class="mini" onclick={() => (tpl = null)}>{tt("common.cancel")}</button>
        <button class="mini pri" onclick={runTemplate}>{ttn("tpl.run", tpl.rows.length)}</button>
      </span>
    </div>
  {/if}
  {#if wt}
    <!-- FR-71: worktree dialog (pen screen 07) -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div class="confirm wtdlg" role="dialog" aria-label={tt("wt.title")} tabindex="-1"
      onkeydown={(e) => (e.key === "Escape" ? (wt = null) : e.key === "Enter" && !e.isComposing && (e.preventDefault(), createWorktree()))}>
      <b>{tt("wt.title")}</b>
      <span class="m">{tt("wt.from", { dir: home(wt.root) })}</span>
      <label>{tt("set.prof.name")} <input class="edit" bind:value={wt.name} spellcheck="false" autocapitalize="off"
        oninput={() => wt && !wt.dirSet && (wt.dir = `${wt.root}.${wt.name.trim()}`)} /></label>
      <label>{tt("set.prof.folder")} <input class="edit" bind:value={wt.dir} spellcheck="false" autocapitalize="off" oninput={() => wt && (wt.dirSet = true)} /></label>
      <label>{tt("wt.branch")} <input class="edit" value={`gembala/${wt.name.trim()}`} disabled /></label>
      <label>{tt("wt.profile")}
        <select class="edit" bind:value={wt.prof}>
          {#each startProfiles as p, i (p.key)}<option value={i}>{p.label}</option>{/each}
        </select>
      </label>
      <span class="m note">{tt("wt.note")}</span>
      {#if wt.err}<span class="err">{wt.err}</span>{/if}
      <span class="acts">
        <button class="mini" onclick={() => (wt = null)}>{tt("common.cancel")} <span class="keys">Esc</span></button>
        <button class="mini pri" onclick={createWorktree} disabled={!startProfiles.length}>{tt("wt.create")} <span class="keys">Enter</span></button>
      </span>
    </div>
  {/if}
  {#if wtDel}
    <div class="confirm wtdlg" role="alertdialog" aria-label={tt("wtdel.aria")}>
      <b>{tt("wtdel.title", { dir: home(wtDel.dir) })}</b>
      <span class="m">{tt("wtdel.sub")}</span>
      <pre class="files">{wtDel.files.slice(0, 30).join("\n")}{wtDel.files.length > 30 ? `\n${ttn("chg.more", wtDel.files.length - 30)}` : ""}</pre>
      <span class="acts">
        <button class="mini" onclick={() => (wtDel = null)}>{tt("common.cancel")}</button>
        <button class="mini danger" onclick={() => ((wtDel = null), wtForce())}>{tt("wtdel.go")}</button>
      </span>
    </div>
  {/if}
  {#if pal}
    <!-- FR-39: command palette (Gembala.pen 03) -->
    <div class="confirm picker palette" role="dialog" aria-label={tt("pal.aria")}>
      <div class="psearch">
        <svg class="pfind" viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.4" fill="none" stroke="currentColor" stroke-width="1.4" /><path d="M10.4 10.4 14 14" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" /></svg>
        <!-- svelte-ignore a11y_autofocus -->
        <input class="edit" bind:value={pal.q} autofocus placeholder={tt("side.find")} aria-label={tt("side.find")} spellcheck="false" autocapitalize="off"
          oninput={() => pal && (pal.sel = 0)}
          onkeydown={(e) => {
            const n = palItems.length;
            if (e.key === "ArrowDown" && n) (e.preventDefault(), pal && (pal.sel = (pal.sel + 1) % n));
            if (e.key === "ArrowUp" && n) (e.preventDefault(), pal && (pal.sel = (pal.sel + n - 1) % n));
            if (e.key === "Enter") (e.preventDefault(), palRun(pal?.sel ?? 0));
            if (e.key === "Escape") (e.preventDefault(), (pal = null));
          }} onblur={() => setTimeout(() => (pal = null), 150)} />
        <button class="esc" aria-label={tt("pal.close")} onclick={() => (pal = null)}>Esc</button>
      </div>
      <div class="pallist" role="listbox" aria-label={tt("pal.results")}>
        {#each palRows as row}
          {#if row.kind === "head"}
            <span class="ph" role="presentation">{row.h}</span>
          {:else}
            <button class="opt" class:on={row.i === pal.sel} role="option" aria-selected={row.i === pal.sel} onpointerdown={(e) => e.preventDefault()} onclick={() => palRun(row.i)}>
              <span class="pico">{#if row.it.agent}<span class="aico {row.it.agent}">{GLYPH[row.it.agent] ?? "›"}</span>{:else}→{/if}</span>
              <span class="plbl">{row.it.label}</span>
              <span class="keys">{row.it.hint}</span>
            </button>
          {/if}
        {:else}
          <span class="pempty">{tt("pal.none")}</span>
        {/each}
      </div>
      <span class="pfoot">{tt("pal.foot")}</span>
    </div>
  {/if}
  {#if picker}
    <div class="confirm picker" role="listbox" aria-label={tt("pick.aria")}>
      {#each picker.items as p, i (p.key)}
        <button class="opt" class:on={i === picker.sel} role="option" aria-selected={i === picker.sel} onclick={() => pick(i)}>
          <span class="num">{i < 9 ? i + 1 : ""}</span>
          <span>{p.label}</span>
          <span class="keys">{p.missing ? tt("main.notInstalled") : p.command || "$SHELL"}</span>
        </button>
      {/each}
      <span class="keys">{tt("pick.hint")}</span>
    </div>
  {/if}
  {#if zoomed}<span class="zoom">{tt("main.zoomed")}</span>{/if}
  <ContextMenu bind:menu />
  <div class="toast" class:show={!!toast} role="status" aria-live="polite">{toast}</div>
</div>
<!-- outside .grid: a fixed .grid is its own stacking layer, so inside it Settings (z-index 10) lost to the sidebar grip (4) and the rail (6) -->
{#if settingsOpen && cfg}
  <Settings {cfg} {meta} {tg} {upd} {aiOpen} drafts={aiDrafts} onai={startAi} ondraft={(d: string) => (aiDrafts = aiDrafts.filter((x) => x.draft !== d))}
    onpane={(p: number) => ((settingsOpen = false), meta[p] && !dead[p] && jumpTo(p))} start={settingsSec} workspaces={wss.map((w) => ({ id: w.id, name: w.name }))} onclose={() => ((settingsOpen = false), (settingsSec = "umum"), (aiOpen = null))} onquitall={askQuitAll} onlogin={loginAccount} onreport={() => ((settingsOpen = false), (reportDraft = { title: "", body: "" }))}
    oninstall={(a: Pick<Agent, "name" | "install">) => ((settingsOpen = false), installInTerminal(a))} onguide={(agent: string) => (GUIDE[agent] ? () => (linkAsk = GUIDE[agent]) : undefined)}
    onresetHints={() => { helperTip = true; hintReset += 1; try { localStorage.removeItem(TIP_KEY); } catch {} }} onwhatsnew={() => (settingsOpen = false, openWhatsNew())} />
{/if}
{#if quitAsk}
  <!-- also opened from Settings > General (z-index 10): sits above it (9 Nov: covered, Esc first to see it) -->
  <div class="confirm ask over" role="alertdialog" aria-label={tt("quit.aria")}>
    <b class="ctitle">{tt("quit.title")}</b>
    <span>{ttn("quit.busy", quitAsk)}</span>
    <span class="acts">
      <button class="mini" onclick={() => (quitAsk = 0)}>{tt("common.cancel")} <span class="keys">Esc</span></button>
      <button class="mini danger" onclick={quitAll}>{tt("set.general.quit")} <span class="keys">Enter</span></button>
    </span>
  </div>
{/if}
{#if whatsNew && whatsNew.length && !whatsNewBlocked}
  <div class="confirm whatsnew" role="dialog" aria-label={tt("set.diag.whatsNew")}>
    <span class="wnhead">
      <b class="ctitle">{tt("whatsnew.title", { version: whatsNew[0]?.version ?? appVer })}</b>
      {#if whatsNewAuto && whatsNew.length > 1}<span class="wnskip">{ttn("whatsnew.skipped", whatsNew.length - 1)}</span>{/if}
    </span>
    <div class="wnbody">
      {#each whatsNew as release (release.version)}
        {#if release !== whatsNew[0]}<h3>{release.version}</h3>{/if}
        <ul>{#each release.bullets as bullet (bullet)}<li>{#each inline(bullet) as part}<span class:bold={part.kind === "bold"} class:code={part.kind === "code"}>{part.text}</span>{/each}</li>{/each}</ul>
      {/each}
    </div>
    <span class="acts"><button class="mini pri" onclick={dismissWhatsNew}>{tt("common.ok")}</button></span>
  </div>
{/if}

<style>
  /* FR-34: fallback icon font (Nerd Fonts v3.4.0, MIT); loaded only when there are PUA glyphs missing from the main font */
  @font-face {
    font-family: "Symbols Nerd Font Mono";
    src: url("/fonts/SymbolsNerdFontMono-Regular.woff2") format("woff2");
    font-display: block;
  }
  /* Hack = Warp's default font (MIT + Bitstream Vera, static/fonts/Hack-LICENSE.md) */
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
  /* keyboard focus always visible; inputs with their own marker (classed outline: none) aren't affected */
  :global(:focus-visible) {
    outline: 1px solid #3b82f6;
    outline-offset: 1px;
  }
  /* press feedback: any button dims briefly when clicked (TECH 19 Responsive) */
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
    --dim: rgba(0, 0, 0, 0.25); /* dim text of an unfocused pane stays readable; the blue line on the pane head already marks focus */
  }
  /* FR-34 light theme = Latte palette (TECH, Gembala.pen T01–T38): not plain white, sidebar darker than the panes,
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
  /* FR-52: vertical tabs per workspace; calm, only what helps the work (memory: clean Warp-style UI) */
  .side {
    position: fixed;
    inset: 0 auto 0 0;
    width: 220px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 40px 8px 10px; /* top: macOS window buttons (transparent title bar) */
    border-right: 1px solid var(--line);
    background: var(--side); /* dark: slightly lighter than the terminal (like Warp); light: darker than the panes */
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
  /* the line under the window-button strip, continuing the rail's (Rail.svelte) */
  .side:not(.full) {
    background: linear-gradient(transparent 34px, var(--line) 34px 35px, transparent 35px) no-repeat, var(--side);
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
  /* terminal: font weight like Warp; antialiased makes text dim/thin on dark screens */
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
  /* pen 02: summary sits right after the name; the + button keeps the right edge */
  .wsum {
    flex-shrink: 0; /* long names get cut, the summary stays on one line */
    white-space: nowrap;
  }
  .x {
    border: 0;
    background: none;
    color: var(--muted);
    visibility: hidden;
    cursor: pointer;
  }
  /* tab/workspace drag (pen screen 09): source faded in place, copy follows the cursor, blue insertion line; no animation */
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
  /* FR-37 pane drag: blue drop zone with the target label, a copy of the pane name follows the cursor */
  .dropzone {
    position: fixed;
    z-index: 40;
    box-sizing: border-box;
    display: grid;
    place-items: center;
    border: 2px solid #3b82f6;
    border-radius: 6px;
    background: color-mix(in srgb, #3b82f6 22%, transparent);
    pointer-events: none;
  }
  .dropzone span {
    padding: 3px 8px;
    border-radius: 4px;
    background: #3b82f6;
    color: #fff;
    font: 12px system-ui, -apple-system, sans-serif;
  }
  :global(.dragghost.panedrag) {
    width: auto;
    padding: 3px 8px;
    font-size: 12px;
  }
  :global(.dragghost) {
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
  :global(.dragghost > *) {
    margin: 0 !important;
    padding: 0 !important;
  }
  :global(.dragghost > * > *) {
    margin-top: 0 !important;
  }
  /* the hand cursor stays visible over any row while dragging */
  .side.dragging,
  .side.dragging * {
    cursor: grabbing !important;
  }
  /* sidebar = app chrome: dragging tabs/workspaces doesn't select text; inputs can still be selected */
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
  .tab.helper {
    margin-left: 16px;
    border-left: 1px solid var(--line);
  }
  .helper-summary, .helper-fold {
    border: 0;
    background: transparent;
    color: var(--muted);
    font: inherit;
    text-align: left;
    padding: 2px 8px 4px 24px;
    cursor: pointer;
    font-size: 11px;
  }
  .helper-fold {
    padding-left: 34px;
  }
  .helper-tip {
    margin: 3px 8px 5px 24px;
    padding: 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    font-size: 11px;
  }
  .helper-tip-actions {
    display: flex;
    gap: 6px;
    margin-top: 6px;
  }
  .helper-head {
    position: absolute;
    height: 32px;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 8px;
    border-bottom: 1px solid var(--line);
    background: var(--bg);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .helper-head > span {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
  /* FR-87: the age sits next to the name and the kind word gets its own row, so a long kind
     word no longer squeezes the name into "s…" or pushes the age onto another line */
  .queue .tab .k {
    grid-area: 1 / 2;
    white-space: nowrap;
  }
  .queue .tab .kind {
    grid-column: 1 / -1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* FR-82: update notice remains visible below the sidebar's workspace list. */
  .updbtn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    align-self: flex-end;
    margin-top: 4px;
    padding: 2px 8px;
    border: 1px solid #14532d;
    border-radius: 999px;
    background: #052e16;
    color: #86efac;
    font: 11px system-ui, -apple-system, sans-serif;
    white-space: nowrap;
  }
  .updbtn i {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #22c55e;
  }
  .updcard {
    position: fixed;
    left: 52px;
    bottom: 40px;
    z-index: 7;
    display: flex;
    flex-direction: column;
    gap: 4px;
    width: 340px;
    max-width: calc(100vw - 16px);
    padding: 12px 14px;
    border: 1px solid #14532d;
    border-radius: 8px;
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
    box-shadow: 0 8px 24px #0008;
  }
  .updcard small {
    color: var(--muted);
    font-size: 11px;
  }
  .updcard ul {
    margin: 4px 0;
    padding-left: 16px;
    font-size: 12px;
  }
  .updcmd {
    margin: 4px 0;
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
    font: 12px "SF Mono", Menlo, monospace;
  }
  /* FR-76: bell panel stays beside the rail when the sidebar is hidden. */
  .notes {
    position: fixed;
    left: 52px;
    bottom: 40px;
    z-index: 7;
    display: flex;
    flex-direction: column;
    width: 380px;
    max-width: calc(100vw - 60px);
    max-height: min(560px, calc(100vh - 80px));
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.35);
  }
  .nhead {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    padding: 10px 12px;
    border-bottom: 1px solid var(--line);
  }
  .nlink {
    padding: 0;
    border: 0;
    background: none;
    color: var(--tx-blue, #60a5fa);
    font: inherit;
    font-size: 12px;
  }
  .nlink:hover {
    text-decoration: underline;
  }
  .nlist {
    overflow-y: auto;
  }
  .nrow {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    width: 100%;
    padding: 10px 12px;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
  }
  .nrow:hover {
    background: color-mix(in srgb, var(--line) 55%, transparent);
  }
  .nrow .dot {
    flex: none;
    margin: 5px 0 0;
  }
  /* FR-76: unseen = bold title, the dot only tells the kind */
  .nrow.seen .ntitle {
    font-weight: 400;
  }
  .notes .nwhere {
    padding: 10px 12px 0;
    font-weight: 600;
  }
  /* folded history, indented under the title (row padding + dot + gap) */
  .nmore,
  .nold {
    display: flex;
    justify-content: space-between;
    box-sizing: border-box;
    width: 100%;
    padding: 0 12px 8px 25px;
    border: 0;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: 12px;
    text-align: left;
  }
  .nmore {
    margin-top: -6px;
  }
  .nmore:hover {
    color: var(--fg);
  }
  /* FR-76: a permission that is no longer waiting fades into history */
  .nrow.past {
    opacity: 0.55;
  }
  .nmain {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }
  .ntitle {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
  .nwhat {
    color: var(--muted);
  }
  .nwhere,
  .nrow .k,
  .nfoot,
  .nempty {
    color: var(--muted);
    font-size: 11px;
  }
  .nempty {
    padding: 14px 12px;
  }
  .nfoot {
    padding: 8px 12px;
    border-top: 1px solid var(--line);
  }
  /* FR-52: needs permission stands out most */
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
  /* FR-44 (mockup-warna-tab variant D, Gembala.pen 02): thin card tinted with the workspace color, no left line;
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
    /* dark: based on the pane color, not the sidebar, so the block is darker than the sidebar (user feedback 4 Nov);
       terang: tint di atas sidebar (--ws-base transparent), karena sidebar terang sudah lebih gelap dari pane */
    background: color-mix(in srgb, var(--c) 5%, var(--ws-base, var(--bg)));
  }
  .wsg:is(.red, .orange, .yellow, .green, .blue, .purple, .pink, .cyan, .brown) > .ws {
    margin-top: 4px;
    color: color-mix(in srgb, var(--c) 70%, var(--ws-mix, #fff));
  }
  /* Gembala.pen 02: workspace name bold in the workspace color */
  .wsg:is(.red, .orange, .yellow, .green, .blue, .purple, .pink, .cyan, .brown) > .ws .wsname {
    color: inherit;
  }
  /* active/hover tab in a colored block: tint of the block color, not a gray box */
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
  .dot.blocked,
  .dot.permission,
  .dot.answer,
  .dot.password {
    background: #d97706;
  }
  /* FR-87 bell kinds: waiting for your OK, failed / hit a limit */
  .dot.approve {
    background: #a855f7;
  }
  .dot.failed,
  .dot.limit {
    background: #dc2626;
  }
  .dot.done {
    background: #16a34a;
  }
  .dot.working {
    background: #3b82f6;
    animation: pulse 1.4s ease-in-out infinite; /* FR-52: working = pulsing */
  }
  /* FR-52: tab with several panes = one small dot per pane; idle pane = ring */
  /* not .mini: that name is already used by small buttons (padding + border) */
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
  /* FR-52: agent mark in tabs and toolbar */
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
  /* door to the ⌘P palette, shaped like a search box so it's easy to find; Gembala.pen 02: "+ Workspace" the same box */
  .add.find,
  .add.ask {
    border: 1px solid var(--line);
  }
  .add.ask {
    display: flex;
    justify-content: space-between;
    color: var(--fg);
  }
  .add.ask .n {
    color: var(--muted);
  }
  /* a search icon + border lighting up on hover, so the box reads as a button that opens the palette */
  .add.find .fl {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .add.find:hover,
  .add.ask:hover {
    border-color: var(--muted);
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
  /* FR-59: input editor docked at the bottom of the pane; extra lines grow upward over the terminal */
  .body.withedit {
    bottom: 86px;
  }
  .body.withedit.withbar {
    bottom: 112px;
  }
  /* FR-59: a clear input box (not plain text that looks like output) */
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
  /* narrow pane: the hint gives way first (hidden), then the folder gets cut; the branch stays whole (TECH 19 #9) */
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
  /* the key hint only in the focused pane; still takes space so the chip row doesn't shift */
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
  /* gray suggestion behind the text: the typed part transparent, the rest faint */
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
  /* Tab list: above the editor box */
  /* FR-59: Start panel in the middle of a new shell pane; clicks outside the buttons go through to the terminal */
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
  /* pen 01: label + agent row + action row, left-aligned in the card's column */
  .scol {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: min(520px, 90%);
  }
  .starts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    pointer-events: auto;
  }
  .sbtn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 4px 10px;
    border: 1px solid var(--line);
    border-radius: 4px;
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
  /* above the editor box (folder/branch row), not covering the typed text; `.mention` (bottom: 8px) loses */
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
  /* FR-61 variant C: account suggestions like mockup-pilih-akun (system font, two lines, orange icon) */
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
  /* FR-37: pane head; buttons show when the cursor is over the pane */
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
  /* focused pane in a split tab: thin blue bottom line */
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
  .hadd {
    flex: none;
    color: var(--muted);
  }
  .hsaver {
    flex: none;
    padding: 0 4px;
    border: 1px solid var(--line);
    border-radius: 3px;
    color: var(--accent, #3b82f6);
    font-size: 10px;
    line-height: 2;
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
  /* pane name and account (FR-61) always shown: the neighboring pane stays clear on who it is and which account it uses */
  .bar.rest > .pname,
  .bar.rest > .acc {
    visibility: visible;
  }
  /* FR-56: finished agent block */
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
    /* FR-101 (5): chips give way by the bar's own width, folder first; the pane name stays and is cut last */
    container-type: inline-size;
    overflow: hidden;
  }
  .bar .pname {
    flex-shrink: 0;
    max-width: 45%;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .bar .chip.box.c-more {
    display: none;
  }
  @container (max-width: 760px) {
    .bar .chip.box.c-cwd {
      display: none;
    }
    .bar .chip.box.c-more {
      display: inline-flex;
    }
  }
  @container (max-width: 620px) {
    .bar .chip.box.c-branch {
      display: none;
    }
  }
  @container (max-width: 520px) {
    .bar .chip.box.c-rich,
    .bar .chip.box.c-set {
      display: none;
    }
  }
  @container (max-width: 420px) {
    .bar .chip.box.c-diff {
      display: none;
    }
  }
  @container (max-width: 330px) {
    .bar .chip.acc.c-acc,
    .bar .chip.box.c-attach {
      display: none;
    }
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
  /* FR-61: pane account name */
  .chip.acc {
    border: 1px solid rgba(59, 130, 246, 0.5);
    color: var(--tx-blue, #93c5fd);
  }
  /* outlined chips + icons like Warp's agent CLI toolbar */
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
    inset: auto 12px 40px 12px;
    z-index: 3;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 10px 12px 12px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.3);
  }
  .rhead {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .rto {
    color: var(--muted);
    font-size: 12px;
  }
  .rhead .n {
    margin-left: auto;
    font-family: "SF Mono", Menlo, monospace;
  }
  .rich textarea {
    width: 100%;
    box-sizing: border-box;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
    font: 13px/1.4 "SF Mono", Menlo, monospace;
    resize: vertical;
    outline: none;
  }
  .rich textarea:focus {
    border-color: var(--tx-blue, #3b82f6);
  }
  .rfiles {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .rfile {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    max-width: 240px;
    padding: 2px 4px 2px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .rfile button {
    padding: 0 3px;
    border: 0;
    background: none;
    color: var(--muted);
  }
  .rfile button:hover {
    color: var(--fg);
  }
  .rfoot {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .rerr {
    padding: 0 12px 6px;
    color: var(--tx-red, #f87171);
    font-size: 12px;
  }
  .rhint {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: 12px;
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
  .cards {
    position: absolute;
    inset: auto 12px 34px 12px;
    z-index: 3;
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-height: calc(100% - 46px);
    overflow: auto;
  }
  .card {
    flex: none;
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
  .card.offer {
    border-color: var(--blue, #3b82f6);
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
  /* FR-57: @ list and hand-off bar, docked at the bottom of the pane (above the agent toolbar) */
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
  /* @ list from the input editor: above the editor box */
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
  /* Gembala.pen 03: 560 px box at the top center, outlined search row, titled groups, full blue selected row */
  .palette {
    top: 96px;
    width: min(560px, 86%);
    padding: 0;
    gap: 0;
    border-radius: 10px;
    overflow: hidden;
  }
  .psearch {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--line);
    color: var(--muted);
  }
  .pfind {
    flex: none;
    width: 13px;
    height: 13px;
  }
  .psearch .edit {
    flex: 1;
    min-width: 0;
    padding: 0;
    border: 0;
    background: none;
    color: var(--fg);
    font: inherit;
    outline: none;
  }
  .psearch .esc {
    flex: none;
    padding: 1px 5px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: none;
    color: var(--muted);
    font: 11px "SF Mono", Menlo, monospace;
    cursor: pointer;
  }
  .pallist {
    display: flex;
    flex-direction: column;
    gap: 0;
    max-height: 50vh;
    padding: 2px 0 6px;
    overflow-y: auto;
  }
  .ph {
    padding: 8px 12px 4px;
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .palette .opt {
    grid-template-columns: 16px 1fr auto;
    padding: 5px 12px;
    border-radius: 0;
  }
  .palette .opt.on {
    background: var(--pri-bg, #2563eb);
    color: #ffffff;
  }
  .palette .opt.on .keys,
  .palette .opt.on .pico {
    color: rgba(255, 255, 255, 0.85);
  }
  .palette .opt.on .aico {
    color: #ffffff;
  }
  .pico {
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .plbl {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pempty {
    padding: 10px 12px;
    color: var(--muted);
    font-size: 12px;
  }
  .pfoot {
    padding: 7px 12px;
    border-top: 1px solid var(--line);
    color: var(--muted);
    font-size: 11px;
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
  /* pen 01: welcome card, also used inside the Start panel (.wcard) */
  .welcome {
    width: min(520px, 90%);
    box-sizing: border-box;
    gap: 8px;
    padding: 16px 18px;
  }
  .wcard {
    display: flex;
    flex-direction: column;
    margin-bottom: 16px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    color: var(--fg);
    pointer-events: auto;
  }
  .wtitle {
    font-size: 14px;
  }
  .wm {
    margin-top: 2px;
    color: var(--muted);
  }
  .wrow {
    display: flex;
    align-items: baseline;
    gap: 4px;
  }
  .wver {
    margin-left: auto;
    color: var(--muted);
    font: 11px "SF Mono", Menlo, monospace;
  }
  .wver.untested {
    color: var(--tx-amber, #d97706);
  }
  /* FR-99 P1: title + close, agent buttons (first one lit), install rows */
  .whead {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .wopen {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: transparent;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .wopen:hover,
  .wopen.pri {
    border-color: var(--blue, #3b82f6);
  }
  .wopen small {
    display: block;
    color: var(--muted);
  }
  .modeq .wopen {
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
  }
  .scol.gone {
    display: none;
  }
  .wdir {
    align-items: center;
    gap: 8px;
    color: var(--muted);
  }
  .wrow.inst {
    align-items: center;
    gap: 6px;
  }
  .wrow.inst .grow {
    flex: 1;
    min-width: 0;
  }
  .wcmd {
    display: block;
    color: var(--muted);
    font: 11px "SF Mono", Menlo, monospace;
  }
  .wkey {
    flex: none;
    width: 80px;
    margin-right: 6px;
    padding: 1px 6px;
    border: 1px solid var(--line);
    border-radius: 3px;
    font: 11px "SF Mono", Menlo, monospace;
  }
  .tile {
    position: absolute;
    box-sizing: border-box;
  }
  /* dim with a thin overlay, not opacity: opacity forces a GPU layer the size of the pane */
  .tile.dim::after {
    content: "";
    position: absolute;
    inset: 0;
    background: var(--dim);
    pointer-events: none;
  }
  /* above the editor, cards, and bar (z-index 3): the divider runs all the way down, input doesn't join across panes */
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
  /* the divider lights up on touch: clearly draggable */
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
    z-index: 8; /* dialogs and palette above the pane dividers (4) and side panels (5) */
  }
  .confirm.over {
    z-index: 11;
  }
  /* pen 13: confirmation card = bold title, body, Batal · Esc / main action */
  .confirm.ask {
    width: min(440px, 90%);
    box-sizing: border-box;
    gap: 10px;
    padding: 14px 16px;
  }
  .confirm.whatsnew {
    width: min(620px, 90vw);
    max-height: 70vh;
    box-sizing: border-box;
    padding: 16px 20px;
    gap: 12px;
  }
  .wnhead {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .wnhead .wnskip {
    color: var(--muted);
    font-size: 12px;
  }
  .wnbody {
    overflow: auto;
    min-height: 0;
    line-height: 1.5;
  }
  .wnbody ul {
    margin: 8px 0 16px;
    padding-left: 22px;
  }
  .wnbody li + li { margin-top: 6px; }
  .wnbody h3 { margin: 14px 0 0; font-size: 13px; }
  .wnbody .bold { font-weight: 700; }
  .wnbody .code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  .ctitle {
    font-size: 14px;
  }
  /* not modal: sits near the top so a real confirmation in the middle stays visible */
  .confirm.warn {
    top: 48px;
    transform: translateX(-50%);
    border-color: var(--tx-amber, #d97706);
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
