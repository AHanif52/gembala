<script lang="ts">
  // FR-92 task panel: who works on what, and who waits for whom. Shares the right column with the message box (two tabs).
  // Sederhana shows people's words only: no task or message ids, no shell-N, no REQ/NOTE/RESULT (BRD section 11).
  import { call } from "./daemon";
  import { errText, t, tOr } from "./i18n/state.svelte";
  import { PILL, split, tally, whyKey, type Kid, type Task } from "./tasks";

  let {
    tasks,
    msgCount = 0,
    simple = false,
    width = 360,
    right = 0,
    person = (n: string) => n,
    ago,
    clock,
    onchange,
    onclose,
    onmsgs,
    onjump,
    onflow,
  }: {
    tasks: Task[];
    /// cards waiting in the other tab, for its badge
    msgCount?: number;
    simple?: boolean;
    width?: number;
    right?: number;
    person?: (name: string) => string;
    /// "12 min ago" for a unix time
    ago: (at: number) => string;
    /// "14.30" for a unix time
    clock: (at: number) => string;
    onchange: () => void;
    onclose: () => void;
    onmsgs: () => void;
    onjump: (pane: number) => void;
    onflow: () => void;
  } = $props();

  const GLYPH: Record<string, string> = { claude: "✳", omp: "π", codex: "◎", opencode: "◇", gemini: "✦", commandcode: "⌘" };
  const groups = $derived(split(tasks));
  let foldOpen = $state(false);
  let menu = $state("");
  let failed = $state("");

  const act = (method: string, params: object) =>
    call(method, params).then(
      () => ((failed = ""), onchange()),
      (e) => ((failed = errText(e)), onchange()),
    );
  const name = (n: string) => (simple ? person(n) : n);
  const why = (k: Kid) => {
    const key = whyKey(k);
    return key ? tOr(key, t("task.why.other")) : "";
  };
  /// the line under a kid's name
  const line = (k: Kid) =>
    k.state === "limit"
      ? (k.reset_at ? t("task.limitAt", { at: clock(k.reset_at) }) : t("task.limit")) + (simple ? "" : ` · ${k.title}`)
      : k.state === "held"
        ? t("task.heldWhy")
        : why(k) || (simple ? k.title : `${k.state} · ${k.title} · ${k.id}`);
  const banner = (task: Task) => {
    const c = tally(task);
    if (!c.waiting) return "";
    const bad = [c.failed && t("task.n.failed", { n: c.failed }), c.held && t("task.n.held", { n: c.held }), c.limit && t("task.n.limit", { n: c.limit })].filter(Boolean);
    const who = c.names.map(name).join(", ");
    return t("task.waitingOn", { who: name(task.owner.name), n: c.waiting, names: who }) + (bad.length ? `; ${bad.join(", ")}` : "");
  };
  const needsLook = (task: Task) => task.kids.some((k) => k.state === "failed" || k.state === "held" || k.state === "limit");
</script>

{#snippet kidRow(task: Task, k: Kid)}
  <div class="ch" class:go={k.to_pane > 0}>
    <span class="br" aria-hidden="true">└</span>
    <button class="main" disabled={k.to_pane <= 0} onclick={() => onjump(k.to_pane)}>
      <b>{#if GLYPH[k.agent]}<span class="aico {k.agent}">{GLYPH[k.agent]}</span>{/if}{task.kind === "flow" ? (k.stage ?? k.title) : name(k.to)}</b>
      {#if !simple && task.kind === "task"}<span class="kd">{k.result ? "RESULT" : "REQ"}</span>{#if k.from && k.from !== task.owner.name}<span class="kd">helper_of {k.from}</span>{/if}{/if}
      {#if task.kind === "flow" && k.to}<span class="m"> · {name(k.to)}</span>{/if}
      {#if task.kind === "task" || k.why}<span class="why">{line(k)}</span>{/if}
      {#if k.result}<span class="res">{k.result}</span>{/if}
    </button>
    <span class="pill {PILL[k.state]}">{simple ? t(`task.st.${k.state}`) : k.result !== null && k.outcome && k.outcome !== "unspecified" ? k.outcome : k.state}</span>
    {#if k.state === "held"}
      <span class="res acts">
        <button class="mini" onclick={() => act("msg.resend", { id: k.id })}>{t("task.resend")}</button>
        <button class="mini ghost" onclick={() => act("msg.discard", { id: k.id })}>{t("task.discard")}</button>
      </span>
    {/if}
  </div>
{/snippet}

<aside class="drawer" style:width="{width}px" style:right="{right}px" aria-label={t("task.aria")}>
  <header>
    <span class="tabs">
      <span class="tab on">{t("task.tab")}<i>{groups.open.length}</i></span>
      <button class="tab" onclick={onmsgs}>{t("task.tabMsgs")}{#if msgCount}<i>{msgCount}</i>{/if}</button>
    </span>
    <button class="mini" onclick={onclose} aria-label={t("msg.close")}>Esc</button>
  </header>
  {#if failed}<div class="failed" role="alert">{failed}</div>{/if}
  <div class="body">
    {#each groups.open as task (task.id)}
      <div class="tk" class:look={needsLook(task)}>
        <div class="th">
          <span class="dot" class:flow={task.kind === "flow"} aria-hidden="true"></span>
          <button class="head" onclick={() => (task.kind === "flow" ? onflow() : task.owner.pane > 0 && onjump(task.owner.pane))}>
            <b>{task.title || t("task.untitled")}</b>
            {#if task.kind === "flow"}<span class="m"> · {simple ? t("task.flow") : `harness · ${task.id}`}</span>{/if}
            {#if !simple}<span class="kd">task {task.id}</span>{#if task.kind === "task"}<span class="kd">{task.implicit ? "REQ · agent" : "REQ · user"}</span>{/if}{/if}
            <small>
              {#if task.kind === "flow"}{t("task.flowLine", { name: task.owner.name })}
              {:else}{#if GLYPH[task.owner.agent]}<span class="aico {task.owner.agent}">{GLYPH[task.owner.agent]}</span>{/if}{t("task.owner", { name: name(task.owner.name), ago: ago(task.at) })}{/if}
            </small>
          </button>
          {#if task.kind === "task"}
            <button class="more" aria-label={t("task.more")} aria-expanded={menu === task.id} onclick={() => (menu = menu === task.id ? "" : task.id)}>⋯</button>
          {/if}
        </div>
        {#if menu === task.id}
          <div class="menu" role="menu">
            {#if task.owner.pane > 0}<button role="menuitem" onclick={() => ((menu = ""), onjump(task.owner.pane))}>{t("task.goTo", { name: name(task.owner.name) })}</button>{/if}
            <button role="menuitem" onclick={() => ((menu = ""), onmsgs())}>{t("task.openResult")}</button>
            <div class="sep"></div>
            <button role="menuitem" onclick={() => ((menu = ""), act("task.done", { id: task.id }))}>{t("task.markDone")}</button>
          </div>
        {/if}
        {#if task.kind === "task" && banner(task)}<div class="wait" class:bad={needsLook(task)}>{banner(task)}</div>{/if}
        {#each task.kids as k (k.id)}{@render kidRow(task, k)}{/each}
      </div>
    {:else}
      <div class="empty">
        <b>{t("task.empty")}</b>
        <span>{t("task.emptyHint")}</span>
      </div>
    {/each}
    {#if groups.done.length}
      <button class="fold" aria-expanded={foldOpen} onclick={() => (foldOpen = !foldOpen)}>
        <span>{t("task.doneFold", { n: groups.done.length })}</span><span aria-hidden="true">{foldOpen ? "▾" : "▸"}</span>
      </button>
      {#if foldOpen}
        {#each groups.done as task (task.id)}
          <div class="tk finished">
            <div class="th">
              <span class="dot" aria-hidden="true"></span>
              <span class="head static">
                <b>{task.title || t("task.untitled")}</b>
                {#if !simple}<span class="kd">task {task.id}</span>{/if}
                <small>{t("task.owner", { name: name(task.owner.name), ago: ago(task.at) })}</small>
              </span>
            </div>
          </div>
        {/each}
      {/if}
    {/if}
  </div>
  <footer><small>{simple ? t("task.hint") : t("task.hintFull")}</small></footer>
</aside>

<style>
  .drawer {
    position: fixed;
    inset: 0 0 0 auto;
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
    align-items: flex-end;
    gap: 8px;
    padding: 8px 12px 0;
    border-bottom: 1px solid var(--line);
  }
  .tabs {
    display: flex;
    flex: 1;
    gap: 2px;
  }
  .tab {
    padding: 6px 12px;
    border: 0;
    border-bottom: 2px solid transparent;
    background: transparent;
    color: var(--muted);
    font: 600 13px system-ui, -apple-system, sans-serif;
    cursor: pointer;
  }
  .tab.on {
    border-color: var(--tx-blue, #3b82f6);
    color: var(--fg);
    cursor: default;
  }
  .tab i {
    margin-left: 5px;
    padding: 0 6px;
    border-radius: 999px;
    background: rgba(217, 119, 6, 0.2);
    color: var(--tx-amber, #d97706);
    font-size: 10.5px;
    font-style: normal;
  }
  header .mini {
    margin-bottom: 6px;
  }
  .mini {
    padding: 2px 8px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--card);
    color: var(--fg);
    font: 11px system-ui, -apple-system, sans-serif;
    cursor: pointer;
  }
  .mini.ghost {
    border-color: transparent;
    color: var(--muted);
  }
  .failed {
    margin: 8px 12px 0;
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    color: var(--tx-amber, #b26b00);
    font-size: 12px;
  }
  .body {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 10px;
    min-height: 0;
    padding: 10px 12px 28px;
    overflow: auto;
  }
  footer {
    padding: 8px 12px;
    border-top: 1px solid var(--line);
    color: var(--muted);
    font-size: 11.5px;
  }
  .tk {
    position: relative;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--bg);
  }
  .tk.look {
    border-color: var(--tx-red, #ef4444);
  }
  .th {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 9px 11px 6px;
  }
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    margin-top: 6px;
    border-radius: 50%;
    background: var(--tx-blue, #3b82f6);
  }
  .tk.look .dot {
    background: var(--tx-red, #ef4444);
  }
  .dot.flow {
    background: var(--tx-purple, #a855f7);
  }
  .head {
    flex: 1;
    min-width: 0;
    padding: 0;
    border: 0;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .head.static {
    cursor: default;
  }
  .head b {
    font-size: 13px;
  }
  .head small {
    display: block;
    margin-top: 1px;
    color: var(--muted);
    font-size: 11.5px;
  }
  .m {
    color: var(--muted);
  }
  .kd {
    margin-left: 6px;
    padding: 0 5px;
    border: 1px solid var(--line);
    border-radius: 4px;
    color: var(--muted);
    font: 10px ui-monospace, "JetBrains Mono", monospace;
  }
  .more {
    flex: none;
    padding: 0 6px;
    border: 1px solid transparent;
    border-radius: 5px;
    background: transparent;
    color: var(--muted);
    font-size: 14px;
    line-height: 1.3;
    cursor: pointer;
  }
  .more:hover,
  .more[aria-expanded="true"] {
    border-color: var(--line);
    color: var(--fg);
  }
  .menu {
    position: absolute;
    top: 34px;
    right: 8px;
    z-index: 2;
    display: flex;
    flex-direction: column;
    min-width: 180px;
    padding: 4px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--card);
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.3);
  }
  .menu button {
    padding: 6px 10px;
    border: 0;
    border-radius: 5px;
    background: transparent;
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
    text-align: left;
    cursor: pointer;
  }
  .menu button:hover {
    background: rgba(127, 127, 127, 0.15);
  }
  .sep {
    height: 1px;
    margin: 3px 0;
    background: var(--line);
  }
  .wait {
    margin: 0 11px 6px;
    padding: 4px 8px;
    border-radius: 6px;
    background: rgba(217, 119, 6, 0.12);
    color: var(--tx-amber, #d97706);
    font-size: 12px;
  }
  .wait.bad {
    background: rgba(239, 68, 68, 0.1);
    color: var(--tx-red, #ef4444);
  }
  .ch {
    display: grid;
    grid-template-columns: 16px 1fr auto;
    gap: 6px;
    align-items: start;
    padding: 6px 11px;
    border-top: 1px solid var(--line);
  }
  .br {
    padding-top: 2px;
    color: var(--muted);
    font: 11px ui-monospace, monospace;
  }
  .main {
    display: block;
    min-width: 0;
    padding: 0;
    border: 0;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
  }
  .ch.go .main {
    cursor: pointer;
  }
  .main b {
    font-size: 12.5px;
    font-weight: 600;
  }
  .why {
    display: block;
    margin-top: 1px;
    color: var(--muted);
    font-size: 11.5px;
  }
  .res {
    display: block;
    margin-top: 3px;
    padding: 3px 8px;
    border-left: 2px solid var(--line);
    font-size: 12px;
  }
  .acts {
    grid-column: 2 / 4;
    display: flex;
    gap: 6px;
    padding: 0;
    border: 0;
  }
  .aico {
    display: inline-block;
    width: 1.1em;
    margin-right: 3px;
    font-weight: 700;
    text-align: center;
  }
  .aico.claude { color: var(--tx-claude, #d97757); }
  .aico.omp { color: var(--tx-purple, #a855f7); }
  .aico.codex { color: var(--tx-codex, #10a37f); }
  .aico.opencode { color: var(--tx-blue, #3b82f6); }
  .aico.gemini { color: var(--tx-gemini, #4285f4); }
  .aico.commandcode { color: var(--tx-commandcode, #ca8a04); }
  .pill {
    padding: 1px 8px;
    border: 1px solid var(--line);
    border-radius: 999px;
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
    white-space: nowrap;
  }
  .pill.run { border-color: var(--tx-blue, #3b82f6); color: var(--tx-blue, #3b82f6); }
  .pill.wt, .pill.hold { border-color: var(--tx-amber, #d97706); color: var(--tx-amber, #d97706); }
  .pill.ok { border-color: var(--tx-green, #22c55e); color: var(--tx-green, #22c55e); }
  .pill.bad, .pill.lim { border-color: var(--tx-red, #ef4444); color: var(--tx-red, #ef4444); }
  .fold {
    display: flex;
    justify-content: space-between;
    padding: 8px 11px;
    border: 1px dashed var(--line);
    border-radius: 10px;
    background: transparent;
    color: var(--muted);
    font: inherit;
    cursor: pointer;
  }
  .finished {
    opacity: 0.75;
  }
  .finished .dot {
    background: var(--muted);
  }
  .empty {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 12px;
    border: 1px dashed var(--line);
    border-radius: 10px;
    color: var(--muted);
    line-height: 1.4;
  }
  .empty b {
    color: var(--fg);
  }
</style>
