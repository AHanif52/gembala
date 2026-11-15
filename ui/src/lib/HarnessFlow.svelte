<script lang="ts">
  // FR-26: the flow of one harness run. Stages as boxes with their state, the agent doing each, the fix rounds, the
  // event trail, and the user's buttons: approve the gate (Ship included), stop, take over, give back. All through the
  // UI channel (SEC-20); the daemon decides what each button means in the current state.
  import { call } from "./daemon";
  import { clock, findingFile, live, restartDefault, restartStages, stagePanes, stepView, stuckStep, type Snap } from "./harness";
  import { errText, t, tOr } from "./i18n/state.svelte";

  // `has`: the pane is still open (FR-86: a finished run's panes close, a stopped one's stay until the user closes them)
  let { snap, has, onchange, onjump, onchanges, onrecipe = () => {}, onclose, simple = false, agentName = (a: string) => a }: { snap: Snap; has: (pane: number) => boolean; onchange: (s: Snap) => void; onjump: (pane: number) => void; onchanges: (pane: number) => void; onrecipe?: (id: string) => void; onclose: () => void; simple?: boolean; agentName?: (a: string) => string } = $props();

  /// FR-23/FR-26: the Ship card opens the changes panel for the run's work pane (all stage panes share the run folder)
  const lastPane = (s: Snap) => stagePanes(s).at(-1)?.pane ?? null;

  let error = $state("");
  // FR-99 P3: the plan (or any stage file) read as plain text; the stopped run's "run again" with its picked stage
  let reader = $state<{ name: string; title: string; text: string; truncated: boolean } | null>(null);
  let again = $state<number | null>(null);
  const readFile = (name: string, title: string) =>
    call("harness.artifact", { run: snap.run, name })
      .then((r) => (r.error ? (error = errText(r.error, r.args)) : (reader = { name, title, text: r.text, truncated: r.truncated })))
      .catch((e) => (error = errText(e)));
  const rerun = () =>
    call("harness.restart", { run: snap.run, from: again })
      .then((r) => {
        if (!r.error) return ((again = null), onchange(r));
        error = r.error === "preflight" ? tOr(`hn.fix.${r.args?.check}`, tOr(`hn.check.${r.args?.check}`, r.args?.check)) : errText(r.error, r.args);
      })
      .catch((e) => (error = errText(e)));
  const act = (method: string) =>
    call(method, { run: snap.run })
      .then((r) => (r.error ? (error = errText(r.error, r.args)) : onchange(r)))
      .catch((e) => (error = errText(e)));
  /// the workspace's earlier runs, read from disk when the section opens; picking one shows its saved flow here
  let old = $state<{ run: string; name: string; started_at: number; state: string; halt: string | null }[] | null>(null);
  const loadOld = () =>
    call("harness.runs", { workspace: snap.workspace })
      .then((r) => (old = (r.runs ?? []).filter((x: { run: string }) => x.run !== snap.run)))
      .catch(() => (old = []));
  const showOld = (run: string) =>
    call("harness.state", { run })
      .then((r) => (r.error ? (error = errText(r.error, r.args)) : onchange(r)))
      .catch((e) => (error = errText(e)));
  const stepName = (i: number | null) => (i == null ? "" : (snap.steps[i]?.name ?? ""));
  /// finished stages that left a file (plan.md, security.md, laporan.md…): each is one click away, not only at the gate (audit u4 #32)
  const results = $derived(snap.steps.map((s, i) => ({ s, i })).filter(({ s, i }) => s.output && !s.human && !s.shell && stepView(snap, i) === "done"));
  const gateStep = $derived(snap.state === "gate" && snap.gate != null ? snap.steps[snap.gate] : null);
  /// the gate's own output file (plan.md): a Ship gate has none, its door is the changes panel
  const gateFile = $derived(gateStep && !gateStep.human && gateStep.output ? gateStep.output : null);
  const stuck = $derived(snap.state === "halted" ? (stuckStep(snap) ?? snap.current) : null);
  const finding = $derived(snap.state === "halted" ? findingFile(snap) : null);
  const stuckPane = $derived(stuck != null ? (snap.steps[stuck]?.pane ?? null) : null);
  /// Sederhana: "Claude Code (kalau macet: Codex, lalu Oh My Pi)" instead of "claude › codex › omp"
  const chain = (agents: string[]) => (!simple || agents.length < 2 ? agents.join(" › ") : t("hn.chainS", { first: agentName(agents[0]), rest: agents.slice(1).map(agentName).join(t("hn.chainSep")) }));
  const line = (e: Snap["trace"][number]) => [simple ? tOr(`hn.evS.${e.code}`, tOr(`hn.ev.${e.code}`, e.code)) : tOr(`hn.ev.${e.code}`, e.code), e.step != null ? stepName(e.step) : "", e.agent ? (simple ? agentName(e.agent) : e.agent) : ""].filter(Boolean).join(" · ");
</script>

<svelte:window onkeydowncapture={(e) => e.key === "Escape" && (e.stopPropagation(), reader ? (reader = null) : again != null ? (again = null) : onclose())} />

<aside class="drawer" aria-label={t("hn.flowAria")}>
  <header>
    <span>{snap.name} <small class="m">·  {tOr(`hn.state.${snap.state}`, snap.state)}{snap.halt ? ` · ${tOr(`hn.halt.${snap.halt}`, snap.halt)}${snap.halt_step != null ? ` (${stepName(snap.halt_step)})` : ""}` : ""}</small></span>
    <!-- 3b: the recipe this run uses, as a builder tab (layar 22); a run started from text has no recipe file -->
    {#if snap.recipe}<button class="mini" onclick={() => onrecipe(snap.recipe)}>{t("hn.recipe")}</button>{/if}
    <button class="mini" onclick={onclose} aria-label={t("hn.close")}>Esc</button>
  </header>
  <div class="body">
    <ol class="steps">
      {#each snap.steps as s, i (s.id)}
        {@const v = stepView(snap, i)}
        <li class="s-{v}">
          <span class="mark" aria-hidden="true">{v === "done" ? "✓" : v === "failed" ? "✕" : v === "gate" ? "⏸" : v === "now" ? "●" : "○"}</span>
          <span class="grow">
            <b>{s.name}</b>
            <small>
              {#if s.human}{t("hn.ship")}{:else if s.shell}{t("hn.shellStage")}{:else}{s.pane_agent ? (simple ? agentName(s.pane_agent) : s.pane_agent) : chain(s.agents)}{/if}{s.gate && !s.human ? ` · ${t("hn.gate")}` : ""}{v === "now" && snap.round ? ` · ${t("hn.round", { n: snap.round, max: snap.max_rounds })}` : ""}
            </small>
          </span>
          {#if s.pane != null && has(s.pane)}<button class="mini" onclick={() => onjump(s.pane!)}>{t("hn.openPane")}</button>{/if}
        </li>
      {/each}
    </ol>

    {#if snap.state === "gate"}
      <div class="gate" role="status">
        <span>{snap.steps[snap.gate ?? 0]?.human ? t("hn.shipAsk") : t("hn.gateAsk", { name: stepName(snap.gate) })}</span>
        <div class="acts">
          {#if snap.steps[snap.gate ?? 0]?.human}
            {@const pane = lastPane(snap)}
            <button class="mini go" disabled={pane == null} title={t("hn.openChangesTip")}
              onclick={() => { if (pane != null) { onchanges(pane); onclose(); } }}>{t("hn.openChanges")}</button>
          {/if}
          {#if gateFile}
            <button class="mini" onclick={() => readFile(gateFile, t(gateStep?.id === "plan" ? "hn.readTitle" : "hn.readTitleOther", { name: snap.name, step: gateStep?.name ?? "" }))}>{t(gateStep?.id === "plan" ? "hn.read" : "hn.readResult")}</button>
          {/if}
          <button class={snap.steps[snap.gate ?? 0]?.human ? "mini" : "mini go"} onclick={() => act("harness.gate")}>{snap.steps[snap.gate ?? 0]?.human ? t("hn.shipDone") : t("hn.gateGo")}</button>
        </div>
      </div>
      {#if snap.steps[snap.gate ?? 0]?.human}<p class="hint">{t("hn.shipHint")}</p>{:else}<p class="hint">{t("hn.gateNote")}</p>{/if}
    {/if}

    {#if stuck != null && snap.halt !== "daemon_restart"}
      <div class="stuck" role="status">
        <b>{t("hn.stuckTitle", { name: stepName(stuck) })}</b>
        <span>{snap.halt ? `${tOr(`hn.halt.${snap.halt}`, snap.halt).replace(/^./u, (c) => c.toUpperCase())}. ` : ""}{t("hn.stuckSub")}</span>
      </div>
      <h4>{t("hn.stuckDo")}</h4>
      <section class="todo">
        {#if finding}
          <div class="row"><span class="grow"><b>{t("hn.do.findings")}</b><small>{t("hn.do.findingsSub", { file: finding })}</small></span>
            <button class="mini" onclick={() => readFile(finding, t("hn.readTitleOther", { name: snap.name, step: stepName(stuck) }))}>{t("hn.do.findingsBtn")}</button></div>
        {/if}
        {#if stuckPane != null && has(stuckPane)}
          <div class="row"><span class="grow"><b>{t("hn.do.pane")}</b><small>{t("hn.do.paneSub")}</small></span>
            <button class="mini" onclick={() => onjump(stuckPane)}>{t("hn.openPane")}</button></div>
        {/if}
        <div class="row"><span class="grow"><b>{t("hn.do.rerun")}</b><small>{t("hn.do.rerunSub")}</small></span>
          <button class="mini" aria-expanded={again != null} onclick={() => (again = again == null ? restartDefault(snap) : null)}>{t("hn.do.rerunBtn")}</button></div>
        {#if again != null}
          <div class="row again">
            <label class="grow">{t("hn.rerunFrom")}
              <select bind:value={again}>{#each restartStages(snap) as i (i)}<option value={i}>{snap.steps[i].name}</option>{/each}</select>
            </label>
            <button class="mini" onclick={() => (again = null)}>{t("common.cancel")}</button>
            <button class="mini go" onclick={rerun}>{t("hn.rerunGo")}</button>
            <small class="warn">{t("hn.rerunWarn")}</small>
          </div>
        {/if}
      </section>
    {/if}

    {#if live(snap)}
      <div class="acts">
        {#if snap.state === "taken_over"}
          <button class="mini" onclick={() => act("harness.release")}>{t("hn.release")}</button>
        {:else if snap.state === "running"}
          <button class="mini" onclick={() => act("harness.takeover")}>{t("hn.takeover")}</button>
        {/if}
        <button class="mini stop" onclick={() => act("harness.stop")}>{t("hn.stop")}</button>
      </div>
    {:else if snap.panes?.some(has)}
      <div class="acts">
        <button class="mini" title={t("hn.closePanesTip")}
          onclick={() => call("harness.close_panes", { run: snap.run }).then((r) => r.error && (error = errText(r.error, r.args))).catch((e) => (error = errText(e)))}>{t("hn.closePanes")}</button>
      </div>
    {/if}
    {#if error}<div class="bad" role="alert">{error}</div>{/if}

    {#if results.length}
      <h4>{t("hn.results")}</h4>
      <section>
        {#each results as { s } (s.id)}
          <div class="row"><span class="grow"><b>{s.name}</b><small>{s.output}</small></span>
            <button class="mini" onclick={() => readFile(s.output, t("hn.readTitleOther", { name: snap.name, step: s.name }))}>{t("hn.readResult")}</button></div>
        {/each}
      </section>
    {/if}
    <h4>{t("hn.trail")}</h4>
    <section>
      {#each [...snap.trace].reverse() as e, i (i)}
        <div class="row"><span class="grow">{line(e)}</span><span class="m">{clock(e.at)}</span></div>
      {/each}
    </section>

    <details ontoggle={(e) => e.currentTarget.open && loadOld()}>
      <summary>{t("hn.old")}</summary>
      <section>
        {#each old ?? [] as r (r.run)}
          <div class="row">
            <button class="mini grow" onclick={() => showOld(r.run)}>{r.name || r.run}</button>
            <span class="m">{tOr(`hn.state.${r.state}`, r.state)}{r.halt ? ` · ${tOr(`hn.halt.${r.halt}`, r.halt)}` : ""} · {new Date(r.started_at * 1000).toLocaleString()}</span>
          </div>
        {:else}
          <div class="row"><span class="m">{old ? t("hn.oldNone") : "…"}</span></div>
        {/each}
      </section>
    </details>
  </div>
</aside>

{#if reader}
  <div class="scrim" role="presentation" onclick={() => (reader = null)}>
    <div class="dlg" role="dialog" aria-modal="true" aria-label={reader.title} tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
      <header><span>{reader.title} <small class="m">· {reader.name}</small></span><button class="mini" onclick={() => (reader = null)} aria-label={t("hn.readClose")}>Esc</button></header>
      <p class="hint">{t("hn.readNote")}{reader.truncated ? ` ${t("hn.readTrunc")}` : ""}</p>
      <pre class="txt">{reader.text.trim() ? reader.text : t("hn.readEmpty")}</pre>
      <div class="acts">
        <button class="mini" onclick={() => (reader = null)}>{t("hn.readClose")}</button>
        {#if gateStep && gateFile === reader.name}<button class="mini go" onclick={() => ((reader = null), act("harness.gate"))}>{t("hn.gateGo")}</button>{/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .drawer {
    position: absolute;
    inset: 0 0 0 auto;
    width: min(44%, 560px);
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
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
  }
  header span:first-child {
    flex: 1;
    font-weight: 600;
  }
  header small {
    font-weight: 400;
  }
  .body {
    overflow: auto;
    padding: 8px 12px 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .steps {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: 6px;
  }
  .steps li {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
  }
  .steps li + li {
    border-top: 1px solid var(--line);
  }
  .mark {
    width: 14px;
    text-align: center;
    color: var(--muted);
  }
  li.s-done .mark {
    color: var(--tx-green, #16a34a);
  }
  li.s-now .mark {
    color: var(--accent, #3b82f6);
  }
  li.s-gate .mark {
    color: var(--tx-amber, #d97706);
  }
  li.s-failed .mark {
    color: var(--tx-red, #dc2626);
  }
  li.s-todo {
    opacity: 0.6;
  }
  .grow {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  small,
  .m,
  .hint {
    color: var(--muted);
    font-size: 12px;
  }
  .hint {
    margin: 0;
  }
  .gate {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border: 1px solid var(--tx-amber, #d97706);
    border-radius: 6px;
  }
  .gate span {
    flex: 1;
  }
  .acts {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
  }
  .bad {
    color: var(--tx-red, #dc2626);
  }
  .stuck {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 8px 10px;
    border: 1px solid var(--tx-red, #dc2626);
    border-radius: 6px;
  }
  .stuck span {
    color: var(--muted);
  }
  .row b + small {
    display: block;
  }
  .again {
    flex-wrap: wrap;
    align-items: center;
  }
  .again select {
    margin-left: 6px;
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .again .warn {
    flex-basis: 100%;
    color: var(--tx-amber, #b45309);
  }
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 20;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.45);
  }
  .dlg {
    width: min(760px, 92vw);
    max-height: 86vh;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
  }
  .dlg header {
    padding: 0 0 6px;
  }
  .txt {
    flex: 1;
    min-height: 120px;
    overflow: auto;
    margin: 0;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
    font: 12.5px/1.6 "SF Mono", Menlo, monospace;
    white-space: pre-wrap;
    word-break: break-word;
  }
  h4 {
    margin: 4px 0 0;
    color: var(--muted);
    font: 600 12px system-ui, -apple-system, sans-serif;
  }
  section {
    border: 1px solid var(--line);
    border-radius: 6px;
  }
  .row {
    display: flex;
    gap: 8px;
    padding: 5px 10px;
  }
  .row + .row {
    border-top: 1px solid var(--line);
  }
  .mini {
    padding: 3px 9px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--bg);
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .mini.go {
    border-color: var(--accent, #3b82f6);
  }
  .mini.stop {
    color: var(--tx-red, #dc2626);
  }
</style>
