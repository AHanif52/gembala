<script module lang="ts">
  import type { View } from "./agent-view";
  /// views outlive the component: a tab switch unmounts it, re-attaching replays from `seq` instead of a lossy snapshot
  const views = new Map<number, View>();
  export const forget = (pane: number) => views.delete(pane);
</script>

<script lang="ts">
  // PLAN-CC-PANE phase 3: structured Command Code pane (mockup requirements/mockup-cc-pane.html). Renders only; state comes
  // from agent-view.ts through the commandcode.ts adapter. Access is the daemon's policy: restricted by default, broad only
  // after the UI gate (phase 5); the terminal stays one click away for per-action approval.
  import { onMount, tick, untrack } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { call } from "./daemon";
  import { BUSY, apply, emptyView, num, type Block, type Tool } from "./agent-view";
  import { fromSnapshot, loadHistory, onEvent, proposal, type CcEvent } from "./commandcode";
  import { lineDiff } from "./diff";
  import { errText, i18n, loc, t, tOr } from "./i18n/state.svelte";

  let {
    pane,
    cwd = "",
    focused = false,
    efforts = [],
    model = $bindable(""),
    effort = $bindable(""),
    onterminal = () => {},
    resume = "",
    onresumed = () => {},
  }: {
    pane: number;
    cwd?: string;
    focused?: boolean;
    efforts?: string[];
    model?: string;
    effort?: string;
    onterminal?: () => void;
    resume?: string; // session of this pane before the daemon restarted (phase 4): reopened read-first
    onresumed?: () => void;
  } = $props();

  // +page keys this component by pane id: one instance per pane, so reading it once is right
  let v = $state<View>(untrack(() => views.get(pane) ?? emptyView()));
  untrack(() => views.set(pane, v));
  let restored = $state(false);
  let prompt = $state("");
  let note = $state("");
  let models = $state<{ id: string; label: string }[]>([]);
  let box = $state<HTMLElement>();
  let input = $state<HTMLTextAreaElement>();
  const busy = $derived(BUSY.includes(v.status));
  // phase 4: a reopened session shows its committed transcript; input waits for "Continue session" after a restart
  let banner = $state<{ ok: boolean; n?: number; cut?: boolean; partial?: boolean; error?: string } | null>(null);
  let locked = $state(false);
  let picking = $state<any[] | null>(null);
  // phase 5 gate: folder, session and policy revision come from the daemon (pane.info) and go back unchanged; a stale
  // revision is refused there. The checkbox is the human decision; gateId names this one dialog.
  let gate = $state<{ gateId: string; cwd: string; session: string | null; revision: number } | null>(null);
  let trust = $state(false);
  let gateErr = $state("");
  let dialog = $state<HTMLDialogElement>();
  const FINAL_OK = ["done", ...BUSY];

  // events that arrive while cc.attach is in flight wait here, then dedup by seq
  let attaching = false;
  let queue: CcEvent[] = [];
  async function attach(fresh = false): Promise<void> {
    attaching = true;
    queue = [];
    let again = false;
    try {
      const r = await call("cc.attach", fresh || !v.epoch ? { pane } : { pane, afterSeq: v.seq });
      if (!fresh && v.epoch === r.epoch && !r.gap) {
        for (const e of r.events ?? []) if (onEvent(v, e) === "resync") again = true;
      } else {
        const had = v.turns.length > 0;
        v = fromSnapshot(r.snapshot, r.epoch, r.throughSeq);
        views.set(pane, v);
        restored = had || v.turns.length > 0;
      }
      for (const e of queue) if (onEvent(v, e) === "resync") again = true;
      // stderr / diagnostic live only in the snapshot: shown under a run that didn't finish cleanly
      const last = v.turns.at(-1);
      if (last?.role === "agent" && !FINAL_OK.includes(last.state)) Object.assign(last, { stderr: r.snapshot?.stderr || "", diagnostic: r.snapshot?.diagnostic ?? null });
    } catch (e) {
      note = errText(e);
    } finally {
      attaching = false;
      queue = [];
    }
    if (again) return attach(true);
  }

  onMount(() => {
    const un = listen<any>("daemon", ({ payload }) => {
      if (payload?.event !== "cc.event" || payload.params?.pane !== pane) return;
      const e: CcEvent = payload.params;
      if (attaching) return void queue.push(e);
      const r = onEvent(v, e);
      if (r === "resync") void attach(true);
      else if (r === "ok" && e.kind === "state" && !FINAL_OK.includes(e.payload?.state) && e.payload?.state !== "working") void attach();
    });
    void attach().then(() => {
      if (resume && !v.turns.length) void openSession(resume, true).then(onresumed);
    });
    call("models.list", { agent: "commandcode" })
      .then((r) => (models = Array.isArray(r?.models) ? r.models : []))
      .catch(() => {});
    return () => {
      un.then((f) => f());
      call("cc.detach", { pane }).catch(() => {});
    };
  });

  // follow the stream only while the reader is at the bottom (scroll doesn't jump while reading history)
  let atBottom = true;
  $effect.pre(() => {
    void v.seq, v.turns.length;
    if (box) atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 80;
  });
  $effect(() => {
    void v.seq, v.turns.length;
    if (atBottom) tick().then(() => box && (box.scrollTop = box.scrollHeight));
  });
  $effect(() => {
    if (focused) input?.focus();
  });

  async function openSession(id: string, cut = false) {
    picking = null;
    try {
      const r = await call("cc.resume", { pane, sessionId: id });
      const messages = r?.history?.messages ?? [];
      loadHistory(v, r.sessionId ?? id, messages);
      restored = false;
      banner = { ok: true, n: messages.length, cut, partial: !!r?.history?.partial };
      locked = cut;
    } catch (e) {
      banner = { ok: false, error: errText(e) };
      locked = false;
    }
  }
  async function openGate() {
    gateErr = "";
    trust = false;
    try {
      const i = await call("pane.info", { pane });
      gate = { gateId: crypto.randomUUID(), cwd: i.cwd, session: i.session || null, revision: i.access_revision };
      dialog?.showModal();
    } catch (e) {
      note = errText(e);
    }
  }
  async function grant() {
    if (!gate || !trust) return;
    try {
      const r = await call("cc.access", { pane, mode: "yolo", gateId: gate.gateId, revision: gate.revision });
      if (r?.mode === "yolo") apply(v, { t: "access", mode: "yolo" });
      dialog?.close();
    } catch (e) {
      gateErr = errText(e); // a stale gate is spent: the dialog has to be opened again for fresh facts
      gate = null;
    }
  }
  async function revoke() {
    try {
      const i = await call("pane.info", { pane });
      await call("cc.access", { pane, mode: "restricted", revision: i.access_revision });
      apply(v, { t: "access", mode: "restricted" });
    } catch (e) {
      note = errText(e);
    }
  }

  async function pick() {
    if (picking) return void (picking = null);
    const r = await call("cc.sessions", { pane }).catch((e) => ((note = errText(e)), null));
    if (r) picking = Array.isArray(r.sessions) ? r.sessions : [];
  }

  async function send() {
    const text = prompt.trim();
    if (!text || busy || locked) return;
    banner = null;
    if (text.startsWith("/")) return void (note = t("cc.slash"));
    note = "";
    prompt = "";
    const turn = { role: "user" as const, text, at: Date.now() };
    apply(v, { t: "user", ...turn });
    try {
      await call("cc.run", { pane, requestId: crypto.randomUUID(), prompt: text, ...(model ? { model } : {}), ...(effort ? { effort } : {}) });
    } catch (e) {
      note = errText(e);
      prompt = text;
      v.turns = v.turns.filter((x) => !(x.role === "user" && x.at === turn.at && x.text === text));
    }
  }
  const stop = () => v.runId && call("cc.cancel", { pane, runId: v.runId }).catch((e) => (note = errText(e)));
  function onkey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      send();
    }
  }

  const time = (at: number) => new Date(at).toLocaleTimeString(loc(), { hour: "2-digit", minute: "2-digit" });
  const n = (x: number | null | undefined) => num(x, i18n.lang);
  const secs = (ms: number | null) => (typeof ms === "number" ? (ms / 1000).toLocaleString(loc(), { maximumFractionDigits: 1 }) : "—");
  const shown = (turn: { blocks: Block[]; draft: Block[] }) => [...turn.blocks, ...turn.draft];
  /// lineDiff lists + before − inside a change; a proposal reads old → new, so each run of changes shows − first
  const minusFirst = (before: string, after: string) => {
    const out = lineDiff(before, after);
    for (let i = 0; i < out.length; ) {
      let j = i;
      while (j < out.length && out[j].t !== " ") j++;
      out.splice(i, j - i, ...out.slice(i, j).sort((a, b) => (a.t === b.t ? 0 : a.t === "-" ? -1 : 1)));
      i = j + 1;
    }
    return out;
  };
  const inputPath =(x: any) => (x && typeof x === "object" ? String(x.file_path ?? x.path ?? x.pattern ?? x.command ?? "") : "");
</script>

{#snippet toolCard(tool: Tool)}
  {@const p = proposal(tool.name, tool.input)}
  <details class="tool" open={["error", "denied", "blocked"].includes(tool.status)}>
    <summary>
      <b>{tool.name || "tool"}</b>{#if inputPath(tool.input)}<code>{inputPath(tool.input)}</code>{/if}
      <span class="st {tool.status}">{tOr(`cc.t.${tool.status}`, tool.status)}</span>
    </summary>
    {#if tool.note}<small class="tnote">{tool.note}</small>{/if}
    {#if p}
      <div class="diff">
        <div class="dh"><code>{p.path}</code><small>{tool.status === "done" ? t("cc.proposalDone") : t("cc.proposal")}</small></div>
        <pre>{#each minusFirst(p.before, p.after) as l, i (i)}<span class={l.t === "+" ? "plus" : l.t === "-" ? "minus" : ""}>{l.t} {l.s}</span>{/each}</pre>
      </div>
    {:else if tool.input && Object.keys(tool.input).length}
      <pre>{JSON.stringify(tool.input, null, 2)}</pre>
    {/if}
    {#if tool.partial && (tool.status === "running" || tool.status === "queued")}<pre class="partial" aria-label={t("cc.partial")}>{tool.partial}</pre>{/if}
    {#if tool.output}<pre>{tool.output}</pre>{/if}
  </details>
{/snippet}

{#snippet block(b: Block)}
  {#if b.kind === "text"}
    <p class="pre">{b.text}</p>
  {:else if b.kind === "thinking"}
    <details class="thinking" open={b.live}>
      <summary>{#if b.live}<span class="pulse" aria-hidden="true"></span>{/if}{t("cc.thinking")}</summary>
      <p class="pre">{b.text}</p>
    </details>
  {:else if b.kind === "notice"}
    <p class="notice {b.level}">{tOr(b.key, b.key, b.params)}</p>
  {:else if v.tools[b.id]}
    {@render toolCard(v.tools[b.id])}
  {/if}
{/snippet}

<div class="cc" role="region" aria-label={t("cc.badge")}>
  <div class="access" class:wide={v.access === "yolo"}>
    {#if v.access === "yolo"}
      <p><b>{t("cc.wide")}</b> · {t("cc.wideNote")}</p>
      {#if busy}<button class="btn" onclick={stop}>{t("cc.stop")}</button>{/if}
      <button class="btn" onclick={revoke}>{t("cc.revoke")}</button>
    {:else}
      <p><b>{t("cc.access")}</b> · {t("cc.accessNote")}</p>
      <button class="btn" onclick={openGate} disabled={busy || locked}>{t("cc.changeAccess")}</button>
    {/if}
    <button class="btn" onclick={pick} disabled={busy} aria-expanded={!!picking}>{t("cc.history")}</button>
    <button class="btn" onclick={onterminal}>{t("cc.openTerminal")}</button>
  </div>
  {#if picking}
    <ul class="picker" aria-label={t("cc.history")}>
      {#each picking as s (s.id)}
        <li>
          <button class="btn row" disabled={s.unsupported} title={s.unsupported ? t("cc.historyUnsupported") : s.path} onclick={() => openSession(s.id)}>
            <span>{s.title || s.id}</span><small>{s.unsupported ? t("cc.historyUnsupported") : s.updatedAt ? new Date(s.updatedAt * 1000).toLocaleString(loc()) : ""}</small>
          </button>
        </li>
      {:else}
        <li class="meta">{t("cc.historyEmpty")}</li>
      {/each}
    </ul>
  {/if}
  {#if banner}
    <div class="resume" class:bad={!banner.ok} role="status">
      <p>
        {#if banner.ok}{t("cc.saved", { n: banner.n ?? 0 })}{#if banner.cut}<br /><small>{t("cc.cut")}</small>{/if}{#if banner.partial}<br /><small>{t("cc.partialHistory")}</small>{/if}
        {:else}{t("cc.broken", { error: banner.error ?? "" })}{/if}
      </p>
      {#if locked}<button class="btn pri" onclick={() => ((locked = false), input?.focus())}>{t("cc.continue")}</button>{/if}
    </div>
  {/if}
  <div class="msgs" bind:this={box}>
    {#if restored}<p class="meta">{t("cc.restored")}</p>{/if}
    {#if v.truncated}<p class="meta">{t("cc.truncated")}</p>{/if}
    {#if !v.turns.length}
      <section class="empty">
        <h2>{t("cc.emptyTitle")}</h2>
        <p>{t("cc.emptyBody")}</p>
        <small>{t("cc.emptySlash")}</small>
      </section>
    {/if}
    {#each v.turns as turn, i (i)}
      {#if turn.role === "user"}
        <article class="user"><div class="who"><b>{t("cc.you")}</b>{#if turn.at}<time>{time(turn.at)}</time>{/if}</div><p class="pre">{turn.text}</p></article>
      {:else}
        <article class="agent">
          <div class="who"><b>Command Code</b>{#if turn.at}<time>{time(turn.at)}</time>{/if}</div>
          {#each shown(turn) as b, j (j)}{@render block(b)}{/each}
          {#if BUSY.includes(turn.state) && turn.state === v.status && !shown(turn).length}<p class="meta"><span class="pulse" aria-hidden="true"></span>{t("cc.s.starting")}</p>{/if}
          {#if !FINAL_OK.includes(turn.state)}
            <p class="err" role="alert">{tOr(`cc.r.${turn.state}`, t("cc.r.error"))}</p>
            {#if turn.result?.error}<pre class="errtext">{turn.result.error}</pre>{/if}
            {#if turn.stderr || turn.diagnostic}
              <details class="tool"><summary>{t("cc.stderr")}</summary><pre>{[turn.diagnostic, turn.stderr].filter(Boolean).join("\n")}</pre></details>
            {/if}
          {/if}
          {#if turn.result}
            {@const u = turn.result.usage}
            <div class="foot">{t("cc.footer", { in: n(u.input), out: n(u.output), cache: n(u.cacheRead), secs: secs(turn.result.durationMs) })}</div>
          {/if}
        </article>
      {/if}
    {/each}
  </div>
  <form class="composer" onsubmit={(e) => (e.preventDefault(), send())}>
    <div class="frame">
      <label class="sr" for="cc-input-{pane}">{t("cc.inputLabel")}</label>
      <textarea id="cc-input-{pane}" rows="2" placeholder={t("cc.placeholder")} disabled={locked} bind:value={prompt} bind:this={input} onkeydown={onkey}></textarea>
      <div class="acts">
        <small>{locked ? t("cc.hintLocked") : busy ? t("cc.hintBusy") : t("cc.hint")}</small>
        <span class="grow"></span>
        {#if busy}<button type="button" class="btn" onclick={stop}>{t("cc.stop")}</button>{/if}
        <button type="submit" class="btn pri" disabled={busy || locked || !prompt.trim()}>{t("cc.send")}</button>
      </div>
    </div>
    {#if note}<p class="meta" role="status">{note}</p>{/if}
    <div class="status">
      <label>{t("cc.model")}
        <select bind:value={model}>
          <option value="">{t("cc.default")}</option>
          {#if model && !models.some((m) => m.id === model)}<option value={model}>{model}</option>{/if}
          {#each models as m (m.id)}<option value={m.id}>{m.label}</option>{/each}
        </select>
      </label>
      <label>{t("cc.effort")}
        <select bind:value={effort}>
          <option value="">{t("cc.default")}</option>
          {#each efforts as e (e)}<option value={e}>{e}</option>{/each}
        </select>
      </label>
      <span class="grp" title={t("cc.contextTip")} aria-label="{t('cc.context')}: {n(v.context)}. {t('cc.contextTip')}">{t("cc.context")} <b>{n(v.context)}</b></span>
      <span class="grp" title={t("cc.usageTip")} aria-label={t("cc.usageTip")}
        >in <b>{n(v.usage?.input)}</b> out <b>{n(v.usage?.output)}</b> cache <b>{n(v.usage?.cacheRead)}</b></span
      >
      <span class="grp" title={t("cc.limitsTip")} aria-label="{t('cc.limits')}: {t('cc.limitsTip')}">{t("cc.limits")} <b>—</b></span>
      <span class="state" role="status">{tOr(`cc.s.${v.status}`, v.status)}</span>
    </div>
  </form>
  <dialog bind:this={dialog} class="gate" aria-labelledby="cc-gate-{pane}" onclose={() => (gate = null)}>
    <h2 id="cc-gate-{pane}">{t("cc.gateTitle")}</h2>
    {#if gate}
      <p>{t("cc.gateProject")}: <code>{gate.cwd}</code><br />{t("cc.session")}: <code>{gate.session ?? t("cc.gateNew")}</code></p>
    {/if}
    <p>{t("cc.gateBody")}</p>
    <p class="meta">{t("cc.gateNote")}</p>
    <label class="check"><input type="checkbox" bind:checked={trust} disabled={!gate} /> <span>{t("cc.gateCheck")}</span></label>
    {#if gateErr}<p class="err" role="alert">{gateErr}</p>{/if}
    <div class="dacts">
      <button class="btn" onclick={() => dialog?.close()}>{t("cc.gateKeep")}</button>
      <button class="btn pri" disabled={!gate || !trust} onclick={grant}>{t("cc.gateGrant")}</button>
    </div>
  </dialog>
  {#if v.sessionId}
    <details class="session">
      <summary>{t("cc.session")} <code>{v.sessionId}</code></summary>
      <p><code>{cwd}</code><br />{t("cc.sessionNote")}</p>
    </details>
  {/if}
</div>

<style>
  .cc {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    min-width: 0;
    background: var(--bg);
    color: var(--fg);
    font: 13px/1.5 system-ui, -apple-system, sans-serif;
  }
  code,
  pre,
  .pre {
    font: 12px/1.6 "SF Mono", Menlo, monospace;
  }
  .pre {
    font-family: inherit;
    font-size: inherit;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    margin: 6px 0;
  }
  pre {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  code {
    overflow-wrap: anywhere;
  }
  .btn {
    padding: 4px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  .btn:hover {
    background: var(--line);
  }
  .btn.pri {
    border-color: #1d4ed8;
    background: rgba(59, 130, 246, 0.15);
  }
  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .btn:focus-visible,
  select:focus-visible,
  summary:focus-visible {
    outline: 2px solid var(--tx-blue, #93c5fd);
    outline-offset: 2px;
  }
  .access {
    margin: 10px 14px 0;
    padding: 8px 12px;
    display: flex;
    gap: 12px;
    align-items: center;
    border: 1px solid #2c405c;
    border-radius: 6px;
    color: var(--tx-blue, #b8d7fa);
  }
  .access.wide {
    border-color: #5b471a;
    background: rgba(252, 211, 77, 0.06);
    color: var(--tx-amber, #fcd34d);
  }
  .gate {
    max-width: 520px;
    width: calc(100% - 32px);
    padding: 22px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--card);
    color: var(--fg);
  }
  .gate::backdrop {
    background: rgba(0, 0, 0, 0.7);
  }
  .gate h2 {
    margin: 0 0 8px;
    font-size: 17px;
    font-weight: 600;
  }
  .gate p {
    margin: 10px 0;
  }
  .check {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    margin: 16px 0;
  }
  .check input {
    margin-top: 4px;
  }
  .dacts {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .access p {
    margin: 0;
    flex: 1;
    font-size: 12px;
  }
  .access .btn {
    flex: none;
    background: transparent;
    color: inherit;
  }
  .picker {
    list-style: none;
    margin: 6px 14px 0;
    padding: 4px;
    max-height: 220px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
  }
  .picker .row {
    width: 100%;
    display: flex;
    gap: 12px;
    justify-content: space-between;
    text-align: left;
    border: 0;
    background: transparent;
  }
  .picker .row:hover:not(:disabled) {
    background: var(--line);
  }
  .picker small {
    color: var(--muted);
  }
  .resume {
    margin: 8px 14px 0;
    padding: 8px 12px;
    display: flex;
    gap: 12px;
    align-items: center;
    border: 1px solid #335479;
    border-radius: 6px;
  }
  .resume.bad {
    border-color: rgba(220, 38, 38, 0.5);
  }
  .resume p {
    flex: 1;
    margin: 0;
    font-size: 12px;
  }
  .resume small {
    color: var(--muted);
  }
  .msgs {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 18px max(16px, calc((100% - 820px) / 2));
  }
  .meta {
    color: var(--muted);
    font-size: 12px;
  }
  .empty {
    min-height: 220px;
    display: flex;
    flex-direction: column;
    justify-content: center;
  }
  .empty h2 {
    font-size: 18px;
    font-weight: 500;
    margin: 0 0 6px;
  }
  .empty p,
  .empty small {
    color: var(--muted);
    max-width: 52ch;
    margin: 0 0 4px;
  }
  .who {
    display: flex;
    gap: 10px;
    align-items: center;
    font-size: 12px;
  }
  .who b {
    font-weight: 600;
  }
  .who time {
    font-size: 11px;
    color: var(--muted);
  }
  .user {
    padding: 10px 14px;
    background: var(--card);
    border-radius: 8px;
    margin-bottom: 20px;
  }
  .agent {
    padding-bottom: 20px;
  }
  .thinking {
    color: var(--muted);
    margin: 8px 0;
  }
  .thinking summary {
    cursor: pointer;
    width: fit-content;
  }
  .thinking .pre {
    padding-left: 16px;
    font-size: 12px;
  }
  .pulse {
    display: inline-block;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--tx-blue, #93c5fd);
    margin-right: 7px;
    animation: breathe 1.8s ease-in-out infinite;
  }
  @keyframes breathe {
    50% {
      opacity: 0.35;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .pulse {
      animation: none;
    }
  }
  .tool {
    border: 1px solid var(--line);
    border-radius: 8px;
    overflow: hidden;
    margin: 8px 0;
  }
  .tool summary {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 10px;
    padding: 8px 12px;
    background: var(--card);
    cursor: pointer;
  }
  .tool summary code {
    color: var(--muted);
    min-width: 0;
  }
  .tool > pre,
  .tnote {
    display: block;
    padding: 10px 12px;
    border-top: 1px solid var(--line);
    color: var(--muted);
  }
  .st {
    margin-left: auto;
    font-size: 11px;
    color: var(--muted);
  }
  .st.done {
    color: var(--tx-green, #86efac);
  }
  .st.running,
  .st.queued {
    color: var(--tx-blue, #93c5fd);
  }
  .st.error,
  .st.denied,
  .st.blocked {
    color: var(--tx-red, #fca5a5);
  }
  .st.unknown {
    color: var(--tx-amber, #fcd34d);
  }
  .diff {
    border-top: 1px solid var(--line);
  }
  .dh {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
  }
  .dh small {
    margin-left: auto;
    color: var(--muted);
  }
  .diff pre {
    padding: 6px 0;
  }
  .diff pre span {
    display: block;
    padding: 0 12px;
  }
  .minus {
    background: rgba(220, 38, 38, 0.15);
    color: var(--tx-red, #f4b4b4);
  }
  .plus {
    background: rgba(34, 197, 94, 0.12);
    color: var(--tx-green, #b1e3be);
  }
  .notice {
    font-size: 12px;
    color: var(--muted);
    margin: 6px 0;
  }
  .notice.warn {
    color: var(--tx-amber, #fcd34d);
  }
  .notice.error,
  .err {
    color: var(--tx-red, #fca5a5);
  }
  .errtext {
    color: var(--muted);
  }
  .foot {
    font-size: 11px;
    color: var(--muted);
    padding-top: 8px;
    border-top: 1px solid var(--line);
    font-variant-numeric: tabular-nums;
  }
  .composer {
    padding: 10px 14px 0;
    border-top: 1px solid var(--line);
  }
  .frame {
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 8px 10px;
    background: var(--card);
  }
  .frame:focus-within {
    border-color: #709bcf;
  }
  textarea {
    display: block;
    width: 100%;
    min-height: 44px;
    max-height: 150px;
    resize: vertical;
    background: transparent;
    border: 0;
    outline: none;
    padding: 0;
    color: inherit;
    font: inherit;
  }
  .acts {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 6px;
  }
  .acts small {
    color: var(--muted);
    font-size: 11px;
  }
  .grow {
    flex: 1;
  }
  .status {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 12px;
    padding: 8px 2px;
    font-size: 11px;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .status select {
    margin-left: 4px;
    font: inherit;
    color: var(--fg);
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: 4px;
  }
  .status b {
    color: var(--fg);
    font-weight: 500;
  }
  .grp {
    white-space: nowrap;
  }
  .state {
    margin-left: auto;
  }
  .session {
    padding: 0 16px 8px;
    color: var(--muted);
    font-size: 11px;
  }
  .session summary {
    cursor: pointer;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
  }
</style>
