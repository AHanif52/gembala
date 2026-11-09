<script lang="ts">
  // FR-26: the flow of one harness run. Stages as boxes with their state, the agent doing each, the fix rounds, the
  // event trail, and the user's buttons: approve the gate (Ship included), stop, take over, give back. All through the
  // UI channel (SEC-20); the daemon decides what each button means in the current state.
  import { call } from "./daemon";
  import { clock, live, stagePanes, stepView, type Snap } from "./harness";
  import { errText, t, tOr } from "./i18n/state.svelte";

  let { snap, onchange, onjump, onchanges, onclose }: { snap: Snap; onchange: (s: Snap) => void; onjump: (pane: number) => void; onchanges: (pane: number) => void; onclose: () => void } = $props();

  /// FR-23/FR-26: the Ship card opens the changes panel for the run's work pane (all stage panes share the run folder)
  const lastPane = (s: Snap) => stagePanes(s).at(-1)?.pane ?? null;

  let error = $state("");
  const act = (method: string) =>
    call(method, { run: snap.run })
      .then((r) => (r.error ? (error = errText(r.error, r.args)) : onchange(r)))
      .catch((e) => (error = errText(e)));
  const stepName = (i: number | null) => (i == null ? "" : (snap.steps[i]?.name ?? ""));
  const line = (e: Snap["trace"][number]) => [tOr(`hn.ev.${e.code}`, e.code), e.step != null ? stepName(e.step) : "", e.agent ?? ""].filter(Boolean).join(" · ");
</script>

<svelte:window onkeydowncapture={(e) => e.key === "Escape" && (e.stopPropagation(), onclose())} />

<aside class="drawer" aria-label={t("hn.flowAria")}>
  <header>
    <span>{snap.name} <small class="m">·  {tOr(`hn.state.${snap.state}`, snap.state)}{snap.halt ? ` · ${tOr(`hn.halt.${snap.halt}`, snap.halt)}${snap.halt_step != null ? ` (${stepName(snap.halt_step)})` : ""}` : ""}</small></span>
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
              {#if s.human}{t("hn.ship")}{:else if s.shell}{t("hn.shellStage")}{:else}{s.pane_agent ?? s.agents.join(" › ")}{/if}{s.gate && !s.human ? ` · ${t("hn.gate")}` : ""}{v === "now" && snap.round ? ` · ${t("hn.round", { n: snap.round, max: snap.max_rounds })}` : ""}
            </small>
          </span>
          {#if s.pane != null}<button class="mini" onclick={() => onjump(s.pane!)}>{t("hn.openPane")}</button>{/if}
        </li>
      {/each}
    </ol>

    {#if snap.state === "gate"}
      <div class="gate" role="status">
        <span>{snap.steps[snap.gate ?? 0]?.human ? t("hn.shipAsk") : t("hn.gateAsk", { name: stepName(snap.gate) })}</span>
        <div class="acts">
          {#if snap.steps[snap.gate ?? 0]?.human}
            {@const pane = lastPane(snap)}
            <button class="mini" disabled={pane == null} title={t("hn.openChangesTip")}
              onclick={() => { if (pane != null) { onchanges(pane); onclose(); } }}>{t("hn.openChanges")}</button>
          {/if}
          <button class="mini go" onclick={() => act("harness.gate")}>{snap.steps[snap.gate ?? 0]?.human ? t("hn.shipDone") : t("hn.gateGo")}</button>
        </div>
      </div>
      {#if snap.steps[snap.gate ?? 0]?.human}<p class="hint">{t("hn.shipHint")}</p>{/if}
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
    {/if}
    {#if error}<div class="bad" role="alert">{error}</div>{/if}

    <h4>{t("hn.trail")}</h4>
    <section>
      {#each [...snap.trace].reverse() as e, i (i)}
        <div class="row"><span class="grow">{line(e)}</span><span class="m">{clock(e.at)}</span></div>
      {/each}
    </section>
  </div>
</aside>

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
