<script lang="ts">
  import { onMount } from "svelte";
  import { buildRings, orderRings, relativeTime, remaining, toneOf, validWindows, type UsageRing, type UsageRow, type UsageWindow } from "./usage";
  import { i18n, loc, t } from "./i18n/state.svelte";
  import { combo, keyText } from "./keys.svelte";

  let {
    rows = [], installed = [], accounts = [], active = [], simple = false, full = false, tg = null,
    unseen = 0, bell = false, showHint = true, resetHints = 0,
    onsearch, ontask, onbell, ontelegram, onsettings,
  }: {
    rows?: UsageRow[];
    installed?: string[];
    accounts?: { id: string; agent: string; name: string }[];
    active?: { agent: string; login: string }[];
    simple?: boolean;
    full?: boolean;
    tg?: any;
    unseen?: number;
    bell?: boolean;
    showHint?: boolean;
    resetHints?: number;
    onsearch: () => void;
    ontask: () => void;
    onbell: () => void;
    ontelegram: () => void;
    onsettings: () => void;
  } = $props();

  const HINT_KEY = "gembala.hint.usage";
  let hint = $state((() => { try { return localStorage.getItem(HINT_KEY) !== "done"; } catch { return true; } })());
  let lastReset = 0;
  $effect(() => {
    if (resetHints > lastReset) {
      lastReset = resetHints;
      hint = true;
      try { localStorage.removeItem(HINT_KEY); } catch {}
    }
  });
  const dismissHint = () => { hint = false; try { localStorage.setItem(HINT_KEY, "done"); } catch {} };
  let now = $state(Math.floor(Date.now() / 1000));
  let height = $state(560);
  let railEl: HTMLElement;
  let hovered = $state("");
  let pinned = $state("");
  let leaveTimer: ReturnType<typeof setTimeout> | undefined;
  let cardY = $state(0);
  let previous: string[] = [];
  const rings = $derived.by(() => {
    const ordered = orderRings(buildRings(installed, accounts, active, rows, now), previous);
    previous = ordered.map((r) => r.key);
    return ordered;
  });
  const capacity = $derived(Math.min(6, Math.max(1, Math.floor((height - (full ? 190 : 220)) / 34))));
  const shown = $derived(rings.length > capacity ? rings.slice(0, Math.max(0, capacity - 1)) : rings);
  const hidden = $derived(rings.slice(shown.length));
  const selected = $derived(hovered || pinned);
  const selectedRing = $derived(rings.find((r) => r.key === selected));
  const firstData = $derived(rings.some((r) => r.row && !r.stale && r.window));
  const shade = (tone: UsageRing["tone"]) => tone === "red" ? "#ef4444" : tone === "yellow" ? "#eab308" : tone === "green" ? "#22c55e" : "#737373";
  // FR-104: the agent keeps its own glyph color; the ring still describes its allowance.
  const iconShade = (agent: string, fallback: string) => agent === "commandcode" ? "var(--tx-commandcode, #ca8a04)" : fallback;
  const icon = (agent: string) => ({ claude: "✳", codex: "◎", omp: "π", opencode: "◇", gemini: "✦", commandcode: "⌘" })[agent as UsageRow["agent"]] ?? "•";
  const name = (agent: string) => ({ claude: "Claude Code", codex: "Codex", omp: "Oh My Pi", opencode: "OpenCode", gemini: "Gemini CLI", commandcode: "Command Code" })[agent as UsageRow["agent"]] ?? agent;
  const windowName = (kind: UsageWindow["kind"]) => kind === "5h" ? t("usage.fiveHours") : kind === "week" ? t("usage.week") : kind === "month" ? t("usage.month") : t("usage.day");
  const sourceName = (source: UsageRow["source"]) => source === "claude_statusline" ? t(simple ? "usage.sourceClaudeSimple" : "usage.sourceClaude") : source === "codex_footer" ? t("usage.sourceCodex") : source === "codex_app_server" ? t(simple ? "usage.sourceCodex" : "usage.sourceCodexServer") : source === "omp_cli" ? t("usage.sourceOmp") : t("usage.sourceGemini");
  const clock = (at: number) => new Date(at * 1000).toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit", hour12: false });
  const resetAt = (at: number | null) => {
    if (!at) return t("usage.resetUnknown");
    const date = new Date(at * 1000);
    const when = date.toDateString() === new Date(now * 1000).toDateString()
      ? t("usage.atTime", { time: clock(at) }) : `${date.toLocaleDateString(loc(), { weekday: "short" })} ${clock(at)}`;
    return `${when} (${relativeTime(at, now, i18n.lang)})`;
  };
  const readAgo = (at: number) => {
    const secs = Math.max(0, now - at);
    return secs < 60 ? t("usage.secondsAgo", { n: secs }) : secs < 3600 ? t("usage.minutesAgo", { n: Math.floor(secs / 60) }) : t("usage.hoursAgo", { n: Math.floor(secs / 3600) });
  };
  const fallback = (agent: string) => agent === "commandcode" ? t("usage.noCommandCode") : agent === "opencode" ? t("usage.noOpenCode") : agent === "gemini" ? t("usage.noGemini") : agent === "omp" ? t("usage.noOmp") : t("usage.noData");
  const badge = (ring: UsageRing) => rings.filter((r) => r.agent === ring.agent).length > 1 ? ring.loginName.charAt(0).toUpperCase() : "";
  const limitReset = (ring: UsageRing) => ring.window?.resets_at ? clock(ring.window.resets_at) : "";
  function show(key: string, el: HTMLElement) {
    clearTimeout(leaveTimer);
    hovered = key;
    cardY = Math.max(8, Math.min(el.getBoundingClientRect().top - 12, window.innerHeight - 260));
  }
  const leave = () => { leaveTimer = setTimeout(() => (hovered = ""), 160); };
  onMount(() => {
    const timer = setInterval(() => (now = Math.floor(Date.now() / 1000)), 30_000);
    const resize = new ResizeObserver(() => (height = railEl.clientHeight));
    resize.observe(railEl);
    const outside = (e: PointerEvent) => { if (!(e.target as Element).closest(".usage-rail, .usage-card")) { pinned = ""; hovered = ""; } };
    document.addEventListener("pointerdown", outside);
    return () => { clearInterval(timer); clearTimeout(leaveTimer); resize.disconnect(); document.removeEventListener("pointerdown", outside); };
  });
</script>

<nav class="usage-rail" class:full bind:this={railEl} aria-label={t("usage.rail")}>
  {#if !full}<div class="window-drag" data-tauri-drag-region></div>{/if}
  <div class="top-buttons">
    <button class="rail-button" title={`${t("side.find")} ${combo("tab.search")}`} aria-label={t("side.find")} onclick={onsearch}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.4" /><path d="M10.4 10.4 14 14" /></svg>
    </button>
    <button class="rail-button" title={`${t("hn.newTask")} ${combo("task.new")}`} aria-label={t("hn.newTask")} onclick={ontask}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 2.5v11M2.5 8h11" /></svg>
    </button>
  </div>
  <div class="bottom-buttons">
    {#each shown as ring (ring.key)}
      <div class="ring-slot">
        <button class="usage-ring" class:stale={ring.stale} aria-label={`${name(ring.agent)} ${ring.loginName}: ${ring.left === null ? t(ring.stale ? "usage.stale" : "usage.noDataShort") : t("usage.left", { n: Math.round(ring.left) })}`}
          aria-expanded={selected === ring.key} title={`${name(ring.agent)} ${ring.loginName}`}
          onpointerenter={(e) => show(ring.key, e.currentTarget)} onpointerleave={leave}
          onclick={(e) => { show(ring.key, e.currentTarget); pinned = pinned === ring.key ? "" : ring.key; }}>
          <svg viewBox="0 0 24 24" aria-hidden="true"><circle class="track" cx="12" cy="12" r="10" class:unknown={ring.tone === "unknown"} /><circle class="amount" cx="12" cy="12" r="10" stroke={shade(ring.tone)} stroke-dasharray={`${(ring.left ?? (ring.row?.limited && !ring.stale ? 5 : 0)) * 0.628} 62.8`} /></svg>
          <span class="agent" style:color={iconShade(ring.agent, ring.tone === "unknown" ? "var(--muted)" : shade(ring.tone))}>{icon(ring.agent)}</span>
          {#if badge(ring)}<span class="login-badge">{badge(ring)}</span>{/if}
        </button>
        {#if ring.row?.limited && !ring.stale && limitReset(ring)}<small class="limit-time">{limitReset(ring)}</small>{/if}
      </div>
    {/each}
    {#if hidden.length}
      <button class="more-ring" aria-label={t("usage.more", { n: hidden.length })} aria-expanded={selected === "more"}
        onpointerenter={(e) => show("more", e.currentTarget)} onpointerleave={leave}
        onclick={(e) => { show("more", e.currentTarget); pinned = pinned === "more" ? "" : "more"; }}>+{hidden.length}</button>
    {/if}
    <span class="divider" aria-hidden="true"></span>
    <button class="rail-button bellbtn" class:on={bell} title={t("bell.title")} aria-label={unseen ? t("bell.ariaUnseen", { n: unseen }) : t("bell.title")} aria-expanded={bell} onclick={onbell}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 11.5V7a4 4 0 0 1 8 0v4.5l1 1H3zM6.5 14h3" /></svg>{#if unseen}<span class="badge">{unseen}</span>{/if}
    </button>
    {#if tg?.name && tg.state !== "off"}
      <button class="rail-button telegram" class:warn={tg.state !== "on"} title={tg.state === "on" ? t("side.tgOn", { bot: tg.bot || "bot" }) : t("side.tgOff")} aria-label={t("set.nav.telegram")} onclick={ontelegram}>
        <svg viewBox="0 0 16 16" aria-hidden="true"><rect x="4.5" y="1.5" width="7" height="13" rx="1.5" /><path d="M7 12h2" /></svg>
      </button>
    {/if}
    <button class="rail-button" title={t("side.settingsTip", { key: keyText("Cmd+,") })} aria-label={t("set.title")} onclick={onsettings}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="2" /><path d="M12.7 6.2l1.8.5v2.6l-1.8.5-.1.3.9 1.5-1.9 1.9-1.5-.9-.3.1-.5 1.8H6.7l-.5-1.8-.3-.1-1.5.9-1.9-1.9.9-1.5-.1-.3-1.8-.5V6.7l1.8-.5.1-.3-.9-1.5 1.9-1.9 1.5.9.3-.1.5-1.8h2.6l.5 1.8.3.1 1.5-.9 1.9 1.9-.9 1.5z" /></svg>
    </button>
  </div>
</nav>

{#if hint && firstData && showHint && !selected}
  <div class="usage-card hint" style:top={`${Math.max(52, Math.min(height - 220, height - (shown.length + (hidden.length ? 1 : 0) + (tg?.name && tg.state !== "off" ? 3 : 2)) * 33 - 32))}px`} role="status">
    <b>{t("usage.hintTitle")}</b>
    <p>{t("usage.hintBody")}</p>
    <button onclick={dismissHint}>{t("helper.gotIt")}</button>
  </div>
{:else if selectedRing}
  <div class="usage-card" style:top={`${cardY}px`} onpointerenter={() => clearTimeout(leaveTimer)} onpointerleave={leave} role="dialog" tabindex="-1" aria-label={t("usage.cardTitle", { name: name(selectedRing.agent) })}>
    <b class="card-title"><span style:color={iconShade(selectedRing.agent, shade(selectedRing.tone))}>{icon(selectedRing.agent)}</span> {t("usage.cardTitle", { name: name(selectedRing.agent) })}{#if !simple || badge(selectedRing)}<small>{simple ? selectedRing.loginName : t("usage.login", { name: selectedRing.loginName })}</small>{/if}</b>
    {#if selectedRing.row?.limited && !selectedRing.stale}<p class="limit">{t("usage.limited", { time: selectedRing.window?.resets_at ? resetAt(selectedRing.window.resets_at) : t("usage.resetUnknown") })}</p>{/if}
    {#if selectedRing.stale}<p class="quiet">{t(simple ? "usage.staleBodySimple" : "usage.staleBody", { name: name(selectedRing.agent) })}</p>
    {:else if !selectedRing.window}<p class="quiet">{fallback(selectedRing.agent)}</p>
    {:else}
      {#each validWindows(selectedRing.row!, now) as w (w.kind)}
        {@const left = remaining(w)}
        <div class="window-row"><span><b>{windowName(w.kind)}</b><strong>{t("usage.left", { n: Math.round(left) })}</strong></span><div class="bar"><i style:width={`${left}%`} style:background={shade(toneOf(left, false))}></i></div><small>{w.resets_at ? t(simple ? "usage.fullAgain" : "usage.resets", { time: resetAt(w.resets_at) }) : t("usage.resetUnknown")}</small></div>
      {/each}
    {/if}
    <div class="source">{selectedRing.row ? `${simple ? t("usage.readFrom") : t("usage.source")}: ${sourceName(selectedRing.row.source)} · ${readAgo(selectedRing.row.read_at)}` : t("usage.sourceUnknown")}</div>
  </div>
{:else if selected === "more"}
  <div class="usage-card" style:top={`${cardY}px`} onpointerenter={() => clearTimeout(leaveTimer)} onpointerleave={leave} role="dialog" tabindex="-1" aria-label={t("usage.moreTitle")}>
    <b>{t("usage.moreTitle")}</b>
    {#each hidden as ring (ring.key)}<button class="more-row" onpointerenter={(e) => show(ring.key, e.currentTarget)} onclick={(e) => { show(ring.key, e.currentTarget); pinned = ring.key; }}><span style:color={iconShade(ring.agent, shade(ring.tone))}>{icon(ring.agent)}</span> {name(ring.agent)}{ring.login !== "utama" ? ` · ${ring.loginName}` : ""}<small>{ring.left === null ? t(ring.stale ? "usage.stale" : "usage.noDataShort") : t("usage.left", { n: Math.round(ring.left) })}</small></button>{/each}
  </div>
{/if}

<style>
  .usage-rail {
    position: fixed;
    inset: 0 auto 0 0;
    z-index: 6;
    box-sizing: border-box;
    width: 44px;
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 40px 0 10px;
    border-right: 1px solid var(--line);
    background: color-mix(in srgb, var(--bg) 97%, var(--fg));
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .usage-rail.full {
    padding-top: 10px;
  }
  /* the macOS window buttons (≈70 px) sit across the rail and the sidebar: the top strip is one color with the
     sidebar, the rail's own color and line start below it */
  .usage-rail:not(.full) {
    border-right: 0;
    background: linear-gradient(var(--side) 34px, var(--line) 34px 35px, color-mix(in srgb, var(--bg) 97%, var(--fg)) 35px);
  }
  .usage-rail:not(.full)::after {
    content: "";
    position: absolute;
    top: 34px;
    right: 0;
    bottom: 0;
    border-right: 1px solid var(--line);
  }
  .window-drag {
    position: absolute;
    inset: 0 0 auto;
    height: 34px;
  }
  .top-buttons, .bottom-buttons {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 5px;
  }
  .bottom-buttons {
    margin-top: auto;
  }
  .rail-button {
    position: relative;
    display: grid;
    place-items: center;
    width: 30px;
    height: 28px;
    padding: 0;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--muted);
    cursor: pointer;
  }
  .rail-button:hover, .rail-button.on {
    background: var(--line);
    color: var(--fg);
  }
  .rail-button svg {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .rail-button.telegram {
    color: var(--tx-green, #22c55e);
  }
  .rail-button.telegram.warn {
    color: var(--tx-amber, #eab308);
  }
  .badge {
    position: absolute;
    top: -3px;
    right: -4px;
    min-width: 13px;
    padding: 0 3px;
    border-radius: 8px;
    background: #d97706;
    color: white;
    font: 600 9px/14px system-ui, sans-serif;
  }
  .ring-slot {
    display: flex;
    flex-direction: column;
    align-items: center;
  }
  .usage-ring {
    position: relative;
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 0;
    border-radius: 50%;
    background: transparent;
    cursor: pointer;
  }
  .usage-ring:hover, .usage-ring[aria-expanded="true"] {
    outline: 2px solid var(--tx-blue, #3b82f6);
  }
  .usage-ring svg {
    position: absolute;
    width: 24px;
    height: 24px;
    transform: rotate(-90deg);
    fill: none;
    stroke-width: 2;
  }
  .usage-ring .track {
    stroke: var(--line);
  }
  .usage-ring .track.unknown {
    stroke: var(--muted);
    stroke-dasharray: 2 3;
  }
  .usage-ring .amount {
    stroke-linecap: round;
  }
  .agent {
    position: relative;
    font-size: 12px;
    line-height: 1;
  }
  .login-badge {
    position: absolute;
    right: -5px;
    bottom: -4px;
    min-width: 12px;
    border-radius: 7px;
    background: var(--card);
    color: var(--fg);
    font: 700 9px/13px system-ui, sans-serif;
  }
  .limit-time {
    color: #fca5a5;
    font: 9px system-ui, sans-serif;
  }
  .more-ring {
    width: 26px;
    height: 26px;
    padding: 0;
    border: 1px dashed var(--muted);
    border-radius: 50%;
    background: transparent;
    color: var(--fg);
    font: 600 10px system-ui, sans-serif;
    cursor: pointer;
  }
  .divider {
    width: 22px;
    height: 1px;
    margin: 2px 0;
    background: var(--line);
  }
  .usage-card {
    position: fixed;
    left: 50px;
    z-index: 9;
    box-sizing: border-box;
    width: 272px;
    max-width: calc(100vw - 56px);
    padding: 10px 12px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--card);
    color: var(--fg);
    box-shadow: 0 12px 30px rgba(0, 0, 0, .35);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .usage-card::before {
    content: "";
    position: absolute;
    left: -6px;
    top: 14px;
    width: 10px;
    height: 10px;
    border-left: 1px solid var(--line);
    border-bottom: 1px solid var(--line);
    background: var(--card);
    transform: rotate(45deg);
  }
  .card-title {
    display: flex;
    align-items: center;
    gap: 5px;
    margin-bottom: 9px;
  }
  .card-title small {
    margin-left: auto;
    color: var(--muted);
    font-weight: 400;
  }
  .window-row {
    margin-bottom: 8px;
  }
  .window-row > span {
    display: flex;
    justify-content: space-between;
    gap: 6px;
  }
  .window-row strong {
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }
  .window-row small {
    color: var(--muted);
    font-size: 10px;
  }
  .bar {
    height: 5px;
    margin: 4px 0 3px;
    overflow: hidden;
    border-radius: 3px;
    background: var(--line);
  }
  .bar i {
    display: block;
    height: 100%;
    border-radius: 3px;
  }
  .source {
    margin-top: 6px;
    padding-top: 7px;
    border-top: 1px solid var(--line);
    color: var(--muted);
    font-size: 10px;
  }
  .quiet {
    color: var(--muted);
    line-height: 1.4;
  }
  .limit {
    padding: 6px 8px;
    border-radius: 5px;
    background: rgba(239, 68, 68, .12);
    color: #fca5a5;
    line-height: 1.4;
  }
  .hint {
    border-color: var(--tx-blue, #3b82f6);
  }
  .hint p {
    margin: 6px 0 8px;
    color: var(--fg);
    line-height: 1.4;
  }
  .hint button {
    display: block;
    margin-left: auto;
    padding: 4px 9px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  .more-row {
    display: flex;
    gap: 6px;
    align-items: center;
    width: 100%;
    padding: 6px 0;
    border: 0;
    background: transparent;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .more-row small {
    margin-left: auto;
    color: var(--muted);
  }
  button:focus-visible {
    outline: 2px solid var(--tx-blue, #3b82f6);
    outline-offset: 2px;
  }
</style>
