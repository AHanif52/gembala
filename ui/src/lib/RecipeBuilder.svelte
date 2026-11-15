<script lang="ts">
  // FR-24, FR-25 (Harness 3b, B2 + B3): the card builder. It is another way to write the same recipe file: the UI keeps
  // the recipe as the daemon's JSON, `recipe.render` turns it into text + problems (nothing is written), and Simpan sends
  // only the hash of the text the user reviewed (`recipe.save`, SEC-27). Mockup: requirements/mockup-builder-3b.html.
  import { call } from "./daemon";
  import { lineDiff } from "./diff";
  import { errText, t, tOr } from "./i18n/state.svelte";
  import {
    MAX_CHAIN, addAgent, addStep, canAddAgent, canMove, diagsFor, diagsRecipe, dropIndex, modelOf, moveAgent, moveStep, removeAgent, removeStep, same,
    setModel, setPrimary, skipped, slugId, used, type BRecipe, type BStep, type Diag, type LibAgent,
  } from "./recipe-builder";

  let { id, tpl = "", names = {}, onclose }: {
    /// recipe id (file name); ignored for a new recipe
    id: string;
    /// template id from `recipe.library`: a new recipe (not a file yet)
    tpl?: string;
    /// agent kind -> "✳ Claude Code"
    names?: Record<string, string>;
    onclose: () => void;
  } = $props();

  type Lib = { steps: BStep[]; templates: { id: string; recipe: BRecipe }[]; agents: LibAgent[] };
  type Prev = { text: string; hash: string; base: string };

  // svelte-ignore state_referenced_locally
  let curId = $state(id);
  // svelte-ignore state_referenced_locally
  let fresh = $state(!!tpl);
  let lib = $state<Lib | null>(null);
  let rec = $state<BRecipe | null>(null);
  let base = $state("");
  let status = $state("");
  let taken = $state<string[]>([]);
  let broken = $state<Diag[] | null>(null);
  let diags = $state<Diag[]>([]);
  let warns = $state<Diag[]>([]);
  let prev = $state<Prev | null>(null);
  let mode = $state<"edit" | "review">("edit");
  let compact = $state(false);
  let chain = $state<string | null>(null);
  let libOpen = $state(false);
  let leaving = $state(false);
  let fail = $state("");
  let busy = $state(false);
  let saved = $state(false);

  const KEY = "gembala.builder.compact";
  try {
    compact = localStorage.getItem(KEY) === "1";
  } catch {}
  function setCompact(v: boolean) {
    compact = v;
    try {
      localStorage.setItem(KEY, v ? "1" : "0");
    } catch {}
  }

  const label = (a: string) => names[a] ?? a;
  const glyph = (a: string) => (names[a] ?? a).split(" ")[0];
  const flags = (a: string) => lib?.agents.find((x) => x.agent === a)?.flags ?? { model: false, effort: false, cost: false };
  const installed = $derived((lib?.agents ?? []).filter((a) => a.installed));
  const dirty = $derived(fresh || (!!rec && base !== "" && !same(rec, JSON.parse(base))));
  // a new recipe is validated under a throwaway id; the real one (from the name) is only used when it goes to review
  const liveId = $derived(fresh ? "tmp-new" : curId);
  const fileId = $derived(fresh ? slugId(rec?.name ?? "", taken) : curId);
  const stepName = (s: BStep) => s.name || s.id;
  const kindOf = (s: BStep) => (s.human ? "gate" : s.shell ? "shell" : s.kind === "check" ? "check" : "work");
  const diagText = (d: Diag) => errText(d.code, d.args);

  async function load() {
    try {
      lib = (await call("recipe.library")) as Lib;
      if (tpl) {
        const tp = lib.templates.find((x) => x.id === tpl);
        rec = tp ? JSON.parse(JSON.stringify(tp.recipe)) : null;
        taken = ((await call("recipe.list")) as { id: string }[]).map((r) => r.id);
      } else {
        const r = await call("recipe.get", { id });
        if (r.error) fail = errText(r.error, r.args);
        else if (!r.full) broken = r.errors ?? [];
        else (rec = r.full), (base = JSON.stringify(r.full)), (status = r.status);
      }
    } catch (e) {
      fail = errText(e);
    }
  }
  load();

  // problems follow the cards a beat after the last change (nothing is written by a render)
  let gen = 0;
  $effect(() => {
    if (!rec || mode !== "edit") return;
    JSON.stringify(rec);
    const rid = liveId;
    const h = setTimeout(async () => {
      const my = ++gen;
      const r = await call("recipe.render", { id: rid, recipe: $state.snapshot(rec) }).catch((e) => ({ error: String(e) }));
      if (my !== gen) return;
      if (r.error) return void (fail = errText(r.error, r.args));
      fail = "";
      diags = r.errors ?? [];
      warns = r.warnings ?? [];
    }, 350);
    return () => clearTimeout(h);
  });

  async function review() {
    if (!rec || busy) return;
    busy = true;
    const r = await call("recipe.render", { id: fileId, recipe: $state.snapshot(rec) }).catch((e) => ({ error: String(e) }));
    busy = false;
    if (r.error) return void (fail = errText(r.error, r.args));
    fail = "";
    diags = r.errors ?? [];
    warns = r.warnings ?? [];
    prev = { text: r.text, hash: r.hash, base: r.diff_base ?? "" };
    chain = null;
    libOpen = false;
    mode = "review";
  }

  async function save() {
    if (!prev || !rec || busy || diags.length) return;
    busy = true;
    const r = await call("recipe.save", { id: fileId, hash: prev.hash }).catch((e) => ({ error: String(e) }));
    busy = false;
    if (r?.error) {
      fail = errText(r.error, r.args);
      // the file moved under us: show the diff against what is there now
      if (r.error === "recipe_changed") await review();
      return;
    }
    fail = "";
    curId = fileId;
    fresh = false;
    base = JSON.stringify($state.snapshot(rec));
    status = "approved";
    saved = true;
    mode = "edit";
  }

  function leave() {
    if (dirty && !saved) leaving = true;
    else onclose();
  }
  $effect(() => {
    if (saved && rec && !same(rec, JSON.parse(base))) saved = false;
  });

  const lines = $derived(prev ? lineDiff(prev.base, prev.text) : []);

  // ---- editing helpers ----
  const patch = (i: number, f: (s: BStep) => BStep) => rec && (rec.steps[i] = f($state.snapshot(rec.steps[i]) as BStep));
  const idxOf = (sid: string) => rec?.steps.findIndex((s) => s.id === sid) ?? -1;
  const mins = (secs: number) => t("rb.mins", { n: Math.round(secs / 60) });
  const timeouts = (cur: number) => [...new Set([300, 600, 900, 1500, 2400, 3600, cur])].sort((a, b) => a - b);
  const EFFORTS = ["low", "medium", "high", "max"];
  const toggleFallback = (k: string) => rec && (rec.fallback_on = rec.fallback_on.includes(k) ? rec.fallback_on.filter((x) => x !== k) : [...rec.fallback_on, k]);

  // ---- drag (pointer events: HTML5 drag-and-drop does not fire `drop` reliably in WKWebView) ----
  type Drag = { scope: "flow" | "chain"; from: number; slot: number; ok: boolean };
  let drag = $state<Drag | null>(null);
  let cardEls = $state<HTMLElement[]>([]);
  let rowEls = $state<HTMLElement[]>([]);

  function track(e: PointerEvent, scope: Drag["scope"], from: number, els: () => HTMLElement[], test: (to: number) => boolean, apply: (to: number) => void) {
    e.preventDefault();
    const grip = e.currentTarget as HTMLElement;
    grip.setPointerCapture(e.pointerId);
    const target = (y: number) => {
      const slot = dropIndex(els().filter(Boolean).map((el) => el.getBoundingClientRect().top + el.getBoundingClientRect().height / 2), y);
      const to = slot > from ? slot - 1 : slot;
      return { slot, to };
    };
    drag = { scope, from, slot: from, ok: false };
    const move = (ev: PointerEvent) => {
      const { slot, to } = target(ev.clientY);
      drag = { scope, from, slot, ok: test(to) };
    };
    const end = (ev: PointerEvent) => {
      grip.removeEventListener("pointermove", move);
      grip.removeEventListener("pointerup", end);
      grip.removeEventListener("pointercancel", end);
      if (ev.type === "pointerup") {
        const { to } = target(ev.clientY);
        if (test(to)) apply(to);
      }
      drag = null;
    };
    grip.addEventListener("pointermove", move);
    grip.addEventListener("pointerup", end);
    grip.addEventListener("pointercancel", end);
  }
  const dragStep = (e: PointerEvent, from: number) =>
    rec && !rec.steps[from].human && track(e, "flow", from, () => cardEls, (to) => canMove(rec!.steps, from, to), (to) => (rec!.steps = moveStep($state.snapshot(rec!.steps) as BStep[], from, to)));
  const dragAgent = (e: PointerEvent, i: number, from: number) =>
    track(e, "chain", from, () => rowEls, (to) => to !== from && to >= 0, (to) => patch(i, (s) => moveAgent(s, from, to)));

  function onkey(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    const typing = document.activeElement instanceof HTMLInputElement || document.activeElement instanceof HTMLTextAreaElement;
    if (typing) return;
    e.preventDefault();
    if (leaving) leaving = false;
    else if (chain) chain = null;
    else if (libOpen) libOpen = false;
    else if (mode === "review") mode = "edit";
    else leave();
  }
</script>

<svelte:window onkeydown={onkey} />

<div class="veil" role="presentation">
  <div class="modal" role="dialog" aria-label={t("rb.title")}>
    {#if !rec && !broken}
      <div class="pad">{fail || t("common.loading")}
        <div class="acts"><button class="mini" onclick={onclose}>{t("common.close")}</button></div></div>
    {:else if broken}
      <!-- TOML that does not parse: the builder refuses to open it and never rewrites it (decision d) -->
      <div class="pad">
        <button class="back" onclick={onclose}>← {t("rb.back")}</button>
        <h3>{id}</h3>
        <div class="errs" role="alert">
          <b>{t("rb.broken")}</b>
          {#each broken as d}<span>{diagText(d)}</span>{/each}
        </div>
        <p class="hint">{t("rb.brokenHint", { file: `recipes/${id}.toml` })}</p>
        <div class="acts"><button class="mini" onclick={onclose}>{t("common.close")}</button></div>
      </div>
    {:else if rec && mode === "review" && prev}
      <div class="pad col">
        <button class="back" onclick={() => (mode = "edit")}>← {t("rb.toBuilder")}</button>
        <div class="head"><b>recipes/{fileId}.toml</b>
          <span class="pill">{prev.base ? t("rb.changed") : t("rb.newFile")}</span><span class="grow"></span></div>
        <div class="diff">
          {#each lines as l}<div class="dl {l.t === '+' ? 'a' : l.t === '-' ? 'r' : ''}">{l.t}{l.s}</div>{/each}
        </div>
        {#if diags.length}
          <section class="diag" role="alert">
            <div class="row"><span class="pill bad">{t("rb.errCount", { n: diags.length })}</span></div>
            {#each diags as d}
              {@const sid = /^step\.([^.]+)/.exec(d.args?.at ?? "")?.[1]}
              <div class="row"><span class="grow">{#if sid}<b>{stepName(rec.steps[idxOf(sid)] ?? ({ id: sid, name: "" } as BStep))}</b>: {/if}{diagText(d)}</span>
                {#if sid && idxOf(sid) >= 0}<button class="mini" onclick={() => (mode = "edit")}>{t("rb.seeCard")}</button>{/if}</div>
            {/each}
          </section>
        {/if}
        {#each warns as w}<div class="warn">{diagText(w)}</div>{/each}
        {#if fail}<div class="bad" role="alert">{fail}</div>{/if}
        <div class="acts">
          <small class="grow">{diags.length ? t("rb.fixFirst") : t("rb.saveNote")}</small>
          <button class="mini" onclick={() => (mode = "edit")}>{t("rb.back2")}</button>
          <button class="mini pri" disabled={busy || diags.length > 0} onclick={save}>{t("rb.approveSave")}</button>
        </div>
      </div>
    {:else if rec}
      <div class="pad col">
        <button class="back" onclick={leave}>← {t("rb.back")}</button>
        <div class="head">
          <div class="who">
            <input class="name" bind:value={rec.name} aria-label={t("rb.name")} maxlength="60" />
            <span class="fid">recipes/{fileId}.toml</span>
          </div>
          <span class="pill {fresh || dirty ? 'chg' : status}">{fresh ? t("rb.unsaved") : dirty ? t("rb.dirty") : tOr(`hn.set.st.${status}`, status)}</span>
          <span class="grow"></span>
          {#if diags.length}<span class="pill bad">{t("rb.errCount", { n: diags.length })}</span>{/if}
          <small>{t("rb.stepCount", { n: rec.steps.length })}</small>
          <span class="seg" role="group" aria-label={t("rb.view")}>
            <button class:on={!compact} onclick={() => setCompact(false)}>{t("rb.detail")}</button>
            <button class:on={compact} onclick={() => setCompact(true)}>{t("rb.compact")}</button>
          </span>
          <button class="mini pri" disabled={busy} onclick={review}>{t("rb.review")}</button>
        </div>
        {#if leaving}
          <div class="warn" role="alertdialog">{t("rb.leaveAsk")}
            <span class="acts"><button class="mini" onclick={() => (leaving = false)}>{t("rb.stay")}</button>
              <button class="mini danger" onclick={onclose}>{t("rb.discard")}</button></span></div>
        {/if}
        {#if saved}<div class="ok" role="status">{t("rb.savedOk")}</div>{/if}
        {#if fail}<div class="bad" role="alert">{fail}</div>{/if}
        {#each diagsRecipe(diags) as d}<div class="warn" role="alert">{diagText(d)}</div>{/each}

        <div class="cols">
          <div class="flow" class:dragging={!!drag}>
            {#if compact}
              <div class="rtab">
                <div class="rh"><span></span><span>#</span><span>{t("rb.col.step")}</span><span>{t("rb.col.chain")}</span><span>{t("rb.col.model")}</span><span>{t("rb.col.time")}</span><span>{t("rb.col.gate")}</span></div>
                {#each rec.steps as s, i (s.id)}
                  {#if drag?.scope === "flow" && drag.ok && drag.slot === i}<div class="drop">{t("rb.drop")}</div>{/if}
                  <div class="rr" class:ship={s.human} class:err={diagsFor(diags, s.id).length > 0} bind:this={cardEls[i]}>
                    <span class="grip" class:lock={s.human} onpointerdown={(e) => dragStep(e, i)} role="button" tabindex="-1" aria-label={t("rb.grip")}>{s.human ? "" : "⠿"}</span>
                    <span class="num">{String(i + 1).padStart(2, "0")}</span>
                    <span><span class="kind {kindOf(s)}">{t(`rb.kind.${kindOf(s)}`)}</span> <b>{stepName(s)}</b></span>
                    <span class="chainic">
                      {#if s.human}<small>{t("rb.shipHuman")}</small>
                      {:else if s.shell}<small>{t("rb.noAgent")}</small>
                      {:else}{#each s.agents as a, j}{#if j}<u>›</u>{/if}<i title={label(a)}>{glyph(a)}</i>{/each}{/if}
                    </span>
                    <span class="mono">{s.human || s.shell ? "—" : `${s.model || t("rb.default")}${s.effort ? ` · ${s.effort}` : ""}`}</span>
                    <span>{s.human ? "—" : mins(s.timeout_secs)}</span>
                    <span>{s.human ? "🔒" : s.gate ? "●" : ""}</span>
                  </div>
                {/each}
                {#if drag?.scope === "flow" && drag.ok && drag.slot === rec.steps.length}<div class="drop">{t("rb.drop")}</div>{/if}
              </div>
              <small class="legend">{installed.map((a) => label(a.agent)).join(" · ")} · {t("rb.legend")}</small>
            {:else}
              {#each rec.steps as s, i (s.id)}
                {#if drag?.scope === "flow" && drag.ok && drag.slot === i}<div class="drop">{t("rb.drop")}</div>{/if}
                {@const errs = diagsFor(diags, s.id)}
                <div class="scard" class:ship={s.human} class:err={errs.length > 0} class:lifted={drag?.scope === "flow" && drag.from === i} bind:this={cardEls[i]}>
                  <span class="grip" class:lock={s.human} onpointerdown={(e) => dragStep(e, i)} role="button" tabindex="-1" aria-label={t("rb.grip")}>{s.human ? "🔒" : "⠿"}</span>
                  <div class="grow">
                    <div class="ttl"><span class="num">{String(i + 1).padStart(2, "0")}</span><span class="kind {kindOf(s)}">{t(`rb.kind.${kindOf(s)}`)}</span><b>{stepName(s)}</b>
                      <span class="grow"></span>
                      {#if !s.human}<button class="mini danger del" onclick={() => rec && (rec.steps = removeStep($state.snapshot(rec.steps) as BStep[], s.id))}>{t("rb.remove")}</button>{/if}</div>
                    {#if s.human}
                      <small>{t("rb.shipNote")}</small>
                    {:else if s.shell}
                      <small>{t("rb.shellNote")}</small>
                      <div class="fields"><label class="f"><span>{t("rb.timeout")}</span>
                        <select value={s.timeout_secs} onchange={(e) => patch(i, (x) => ({ ...x, timeout_secs: +e.currentTarget.value }))}>
                          {#each timeouts(s.timeout_secs) as n}<option value={n}>{mins(n)}</option>{/each}</select></label></div>
                    {:else}
                      {@const a0 = s.agents[0]}
                      <div class="fields">
                        <label class="f"><span>{t("rb.agent")}</span>
                          <select value={a0} onchange={(e) => patch(i, (x) => setPrimary(x, e.currentTarget.value))}>
                            {#each [...new Set([...installed.map((a) => a.agent), ...s.agents])] as a}<option value={a}>{label(a)}</option>{/each}</select></label>
                        <button class="link" class:open={chain === s.id} onclick={() => (chain = chain === s.id ? null : s.id)}>
                          {s.agents.length > 1 ? t("rb.chainTo", { list: s.agents.slice(1).map((a) => label(a).replace(/^\S+ /, "")).join(", ") }) : t("rb.chainAdd")}</button>
                        {#if flags(a0).model}<label class="f"><span>{t("rb.model")}</span>
                          <input class="mi" value={s.model} placeholder={t("rb.default")} onchange={(e) => patch(i, (x) => setModel(x, a0, e.currentTarget.value))} /></label>{/if}
                        {#if flags(a0).effort}<label class="f"><span>{t("rb.effort")}</span>
                          <select value={s.effort} onchange={(e) => patch(i, (x) => ({ ...x, effort: e.currentTarget.value }))}>
                            <option value="">{t("rb.default")}</option>{#each EFFORTS as ef}<option value={ef}>{ef}</option>{/each}</select></label>{/if}
                        <label class="f"><span>{t("rb.timeout")}</span>
                          <select value={s.timeout_secs} onchange={(e) => patch(i, (x) => ({ ...x, timeout_secs: +e.currentTarget.value }))}>
                            {#each timeouts(s.timeout_secs) as n}<option value={n}>{mins(n)}</option>{/each}</select></label>
                        <label class="f"><span>{t("rb.gate")}</span>
                          <input type="checkbox" role="switch" checked={s.gate} onchange={(e) => patch(i, (x) => ({ ...x, gate: e.currentTarget.checked }))} /></label>
                      </div>
                    {/if}
                    {#if !s.human}
                      <div class="art">
                        {#if s.inputs.length}<span>{t("rb.inputs")} <code>{s.inputs.join(", ")}</code></span>{/if}
                        {#if s.output}<span>{t("rb.output")} <code>{s.output}</code></span>{/if}
                        <span>{tOr(`rb.pass.${s.pass || "reply"}`, s.pass)}</span>
                      </div>
                    {/if}
                    {#each errs as d}<div class="errmsg">{diagText(d)}</div>{/each}

                    {#if chain === s.id && !s.shell && !s.human}
                      <!-- layar 24: the fallback chain of this step (FR-25) -->
                      <div class="chainbox">
                        <div class="ch"><span class="lbl">{t("rb.chain")} · {stepName(s)}</span><span class="grow"></span><small>{t("rb.chainCount", { n: s.agents.length, max: MAX_CHAIN })}</small></div>
                        <p class="hint">{s.kind === "check" ? t("rb.chainCheck") : t("rb.chainWork")}</p>
                        {#each s.agents as a, j (a)}
                          {#if drag?.scope === "chain" && drag.from !== j && drag.slot === j}<div class="drop thin"></div>{/if}
                          <div class="crow" bind:this={rowEls[j]} class:lifted={drag?.scope === "chain" && drag.from === j}>
                            <span class="grip" onpointerdown={(e) => dragAgent(e, i, j)} role="button" tabindex="-1" aria-label={t("rb.grip")}>⠿</span>
                            <span class="no">{String(j + 1).padStart(2, "0")}</span>
                            <span class="who2"><b>{label(a)}</b> <small>{j === 0 ? t("rb.primary") : t("rb.backup", { n: j })}</small></span>
                            {#if flags(a).model}
                              <input class="mi" value={modelOf(s, a)} placeholder={t("rb.defaultAgent")} aria-label={t("rb.model")} onchange={(e) => patch(i, (x) => setModel(x, a, e.currentTarget.value))} />
                            {:else}<small>{t("rb.noModel")}</small>{/if}
                            <span class="effc">
                              {#if !flags(a).effort}<small>{t("rb.noEffort")}</small>
                              {:else if j === 0}
                                <select value={s.effort} aria-label={t("rb.effort")} onchange={(e) => patch(i, (x) => ({ ...x, effort: e.currentTarget.value }))}>
                                  <option value="">{t("rb.default")}</option>{#each EFFORTS as ef}<option value={ef}>{ef}</option>{/each}</select>
                              {:else}{s.effort || t("rb.default")} <small>{t("rb.followCard")}</small>{/if}
                            </span>
                            <button class="x" disabled={s.agents.length < 2} aria-label={t("rb.removeAgent")} onclick={() => patch(i, (x) => removeAgent(x, a))}>×</button>
                            {#if flags(a).cost}<div class="extra">{t("rb.cost")}
                              <input class="mi sm" inputmode="decimal" value={s.max_cost ?? ""} placeholder={t("rb.noCap")}
                                onchange={(e) => patch(i, (x) => ({ ...x, max_cost: e.currentTarget.value.trim() === "" || !(+e.currentTarget.value > 0) ? null : +e.currentTarget.value }))} />
                              <small>{t("rb.costNote")}</small></div>{/if}
                            {#if skipped(rec.steps, i).includes(a)}<div class="flag">⤳ {t("rb.skipped")}</div>{/if}
                          </div>
                        {/each}
                        <div class="addrow">
                          {#each (lib?.agents ?? []).filter((x) => !s.agents.includes(x.agent)) as x (x.agent)}
                            <button class="mini" disabled={!canAddAgent(s, x.agent, lib?.agents ?? [])} title={!x.installed ? t("rb.notInstalled") : s.agents.length >= MAX_CHAIN ? t("rb.chainFull") : ""}
                              onclick={() => patch(i, (y) => addAgent(y, x.agent, lib?.agents ?? []))}>+ {label(x.agent)}</button>
                          {/each}
                        </div>
                        {#if s.agents.length >= MAX_CHAIN}<small>{t("rb.chainFull")}</small>{/if}
                        <small class="c9">⊘ {t("rb.c9")}</small>
                        <div class="acts"><button class="mini" onclick={() => (chain = null)}>{t("rb.chainDone")}</button></div>
                      </div>
                    {/if}
                  </div>
                </div>
              {/each}
              {#if drag?.scope === "flow" && drag.ok && drag.slot === rec.steps.length}<div class="drop">{t("rb.drop")}</div>{/if}
            {/if}

            <div class="add">
              <button class="addbtn" onclick={() => (libOpen = !libOpen)}>+ {t("rb.addStep")}</button>
              {#if libOpen && lib}
                <div class="lib">
                  <div class="libgrid">
                    {#each lib.steps as c (c.id)}
                      <button class="li" disabled={used(rec.steps, c.id)} onclick={() => rec && ((rec.steps = addStep($state.snapshot(rec.steps) as BStep[], c)), (libOpen = false))}>
                        <b>{c.name || c.id}</b>
                        <small>{used(rec.steps, c.id) ? t("rb.inFlow") : t(`rb.kind.${c.human ? "gate" : c.shell ? "shell" : c.kind === "check" ? "check" : "work"}`)}</small></button>
                    {/each}
                    <button class="li cust" disabled title={t("rb.customSoon")}><b>{t("rb.custom")}</b><small>{t("rb.customSoon")}</small></button>
                  </div>
                </div>
              {/if}
            </div>
          </div>

          <aside class="panel">
            <h4>{t("rb.recipe")}</h4>
            <div class="prow"><span class="grow">{t("rb.rounds")}<small>{t("rb.roundsSub")}</small></span>
              <select value={rec.max_rounds} aria-label={t("rb.rounds")} onchange={(e) => rec && (rec.max_rounds = +e.currentTarget.value)}>{#each [0, 1, 2, 3] as n}<option value={n}>{n}</option>{/each}</select></div>
            <div class="prow"><span class="grow">{t("rb.total")}</span>
              <select value={rec.total_secs} aria-label={t("rb.total")} onchange={(e) => rec && (rec.total_secs = +e.currentTarget.value)}>
                {#each [...new Set([1800, 3600, 5400, 7200, rec.total_secs])].sort((a, b) => a - b) as n}<option value={n}>{n < 3600 ? t("hn.set.mins", { n: n / 60 }) : t("hn.set.hours", { n: n / 3600 })}</option>{/each}</select></div>
            <div class="prow"><span class="grow">{t("rb.saver")}</span>
              <input type="checkbox" role="switch" checked={rec.token_saver} aria-label={t("rb.saver")} onchange={(e) => rec && (rec.token_saver = e.currentTarget.checked)} /></div>
            <div class="prow col"><span>{t("rb.trigger")}</span>
              <div class="chips">
                {#each ["exit", "timeout", "unknown"] as k}
                  <button class="chip" class:off={!rec.fallback_on.includes(k)} onclick={() => toggleFallback(k)}>{tOr(`rb.fb.${k}`, k)}</button>
                {/each}
                <button class="chip off" disabled title={t("rb.limitOff")}>{t("rb.fb.limit")} <em>{t("rb.limitOff")}</em></button>
              </div>
              <small>{t("rb.triggerNote")}</small></div>
            <div class="hard">{t("rb.hard")}</div>
          </aside>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .veil {
    position: fixed;
    inset: 0;
    z-index: 10;
    display: grid;
    place-items: center;
    background: rgba(0, 0, 0, 0.5);
  }
  .modal {
    width: min(1060px, calc(100vw - 32px));
    height: calc(100vh - 80px);
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
    overflow: auto;
  }
  .pad {
    padding: 14px 18px 18px;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  small,
  .hint {
    color: var(--muted);
    font-size: 12px;
  }
  .hint {
    margin: 0;
  }
  code,
  .mono,
  .fid {
    font: 11.5px "SF Mono", Menlo, monospace;
  }
  .back {
    align-self: flex-start;
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
  select,
  .mini,
  .seg button,
  .mi,
  .name {
    padding: 4px 8px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }
  .mini.pri {
    border-color: var(--pri-bg, #3b82f6);
    color: var(--pri-bg, #3b82f6);
  }
  .mini.danger {
    color: var(--tx-red, #ef4444);
  }
  .mini:disabled,
  .li:disabled {
    opacity: 0.45;
  }
  .seg {
    display: flex;
  }
  .seg button {
    border-radius: 0;
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
  }
  input[role="switch"]:checked {
    background-color: var(--pri-bg, #3b82f6);
    background-position: right 2px center;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .name {
    width: 260px;
    font-size: 16px;
    font-weight: 600;
  }
  .who {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .fid {
    color: var(--muted);
  }
  .pill {
    padding: 1px 8px;
    border: 1px solid var(--line);
    border-radius: 999px;
    color: var(--muted);
    font-size: 11px;
    white-space: nowrap;
  }
  .pill.approved {
    color: var(--tx-green, #16a34a);
  }
  .pill.chg,
  .pill.changed,
  .pill.needs_approval {
    color: var(--tx-amber, #d97706);
  }
  .pill.bad {
    color: var(--tx-red, #ef4444);
    border-color: var(--tx-red, #ef4444);
  }
  .cols {
    display: grid;
    grid-template-columns: 1fr 270px;
    gap: 16px;
    align-items: start;
  }
  .flow {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
    position: relative;
  }
  .flow.dragging {
    user-select: none;
    cursor: grabbing;
  }
  .scard {
    display: flex;
    gap: 10px;
    padding: 10px 12px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--bg);
  }
  .scard.ship {
    border-color: #b45309;
  }
  .scard.err,
  .rr.err {
    border-color: #b91c1c;
  }
  .lifted {
    opacity: 0.4;
  }
  .grip {
    color: var(--muted);
    cursor: grab;
    touch-action: none;
    user-select: none;
    width: 14px;
    flex: none;
  }
  .grip.lock {
    cursor: default;
  }
  .num {
    color: var(--muted);
    font: 11px "SF Mono", Menlo, monospace;
  }
  .ttl {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
  }
  .kind {
    padding: 1px 6px;
    border-radius: 4px;
    font: 10px "SF Mono", Menlo, monospace;
    letter-spacing: 0.05em;
    background: var(--line);
  }
  .kind.work {
    color: #3b82f6;
  }
  .kind.check,
  .kind.shell {
    color: #a78bfa;
  }
  .kind.gate {
    color: #d97706;
  }
  .fields {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    align-items: center;
  }
  .f {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .f > span {
    color: var(--muted);
    font-size: 11px;
  }
  .mi {
    width: 130px;
    font: 12px "SF Mono", Menlo, monospace;
  }
  .mi.sm {
    width: 80px;
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--pri-bg, #3b82f6);
    font: inherit;
    text-decoration: underline dotted;
    text-underline-offset: 3px;
    cursor: pointer;
  }
  .art {
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    margin-top: 8px;
    padding-top: 6px;
    border-top: 1px dashed var(--line);
    color: var(--muted);
    font-size: 11px;
  }
  .errmsg,
  .warn,
  .ok {
    margin-top: 6px;
    padding: 6px 8px;
    border-radius: 5px;
    font-size: 12px;
  }
  .errmsg {
    background: rgba(185, 28, 28, 0.15);
    color: var(--tx-red, #ef4444);
  }
  .warn {
    background: rgba(217, 119, 6, 0.15);
    color: var(--tx-amber, #d97706);
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .ok {
    background: rgba(22, 163, 74, 0.15);
    color: var(--tx-green, #16a34a);
  }
  .bad {
    color: var(--tx-red, #ef4444);
  }
  .drop {
    padding: 12px 16px;
    border: 1.5px dashed var(--pri-bg, #3b82f6);
    border-radius: 8px;
    color: var(--pri-bg, #3b82f6);
    font-size: 12px;
  }
  .drop.thin {
    padding: 0;
    height: 0;
    border-width: 0 0 2px;
    border-radius: 0;
  }
  .add {
    position: relative;
  }
  .addbtn {
    padding: 6px 12px;
    border: 1px dashed var(--line);
    border-radius: 6px;
    background: none;
    color: var(--fg);
    font: inherit;
  }
  .lib {
    position: absolute;
    left: 0;
    bottom: 38px;
    z-index: 4;
    width: min(520px, 100%);
    padding: 10px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--card);
    box-shadow: 0 12px 28px rgba(0, 0, 0, 0.45);
  }
  .libgrid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 6px;
  }
  .li {
    padding: 7px 9px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    text-align: left;
  }
  .li small {
    display: block;
    font-size: 10px;
  }
  .li.cust {
    grid-column: 1 / -1;
  }
  .chainbox {
    margin-top: 10px;
    padding: 10px 12px;
    border: 1px solid var(--pri-bg, #3b82f6);
    border-radius: 8px;
    background: var(--card);
  }
  .ch {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .lbl {
    color: var(--muted);
    font: 10px "SF Mono", Menlo, monospace;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .crow {
    display: grid;
    grid-template-columns: 16px 22px 150px 1fr 130px 22px;
    gap: 8px;
    align-items: center;
    padding: 6px 8px;
    margin: 6px 0;
    border: 1px solid var(--line);
    border-radius: 7px;
    background: var(--bg);
  }
  .no {
    color: var(--muted);
    font: 11px "SF Mono", Menlo, monospace;
  }
  .who2 small {
    margin-left: 4px;
  }
  .x {
    width: 22px;
    height: 22px;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--muted);
    font-size: 14px;
  }
  .x:hover:not(:disabled) {
    background: var(--line);
    color: var(--fg);
  }
  .x:disabled {
    opacity: 0.3;
  }
  .extra,
  .flag {
    grid-column: 3 / 6;
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 11.5px;
    color: var(--muted);
  }
  .flag {
    padding: 4px 8px;
    border-radius: 5px;
    background: rgba(217, 119, 6, 0.15);
    color: var(--tx-amber, #d97706);
  }
  .addrow {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 6px 0;
  }
  .c9 {
    display: block;
    margin: 6px 0;
  }
  .acts {
    display: flex;
    gap: 8px;
    align-items: center;
    justify-content: flex-end;
  }
  .panel {
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--bg);
  }
  .panel h4 {
    margin: 0;
    padding: 10px 12px 6px;
    color: var(--muted);
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .prow {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    border-top: 1px solid var(--line);
  }
  .prow small {
    display: block;
  }
  .prow.col {
    flex-direction: column;
    align-items: stretch;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
  }
  .chip {
    padding: 2px 8px;
    border: 1px solid var(--pri-bg, #3b82f6);
    border-radius: 999px;
    background: none;
    color: var(--fg);
    font: 11px system-ui;
  }
  .chip.off {
    border-color: var(--line);
    color: var(--muted);
    text-decoration: line-through;
  }
  .chip em {
    font-style: normal;
    text-decoration: none;
    display: inline-block;
  }
  .hard {
    padding: 8px 12px;
    border-top: 1px solid var(--line);
    color: var(--muted);
    font-size: 11px;
  }
  .rtab {
    border: 1px solid var(--line);
    border-radius: 8px;
    overflow: hidden;
  }
  .rh,
  .rr {
    display: grid;
    grid-template-columns: 16px 24px 190px 1fr 120px 60px 40px;
    gap: 8px;
    align-items: center;
    padding: 7px 10px;
  }
  .rh {
    color: var(--muted);
    font: 10px "SF Mono", Menlo, monospace;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    border-bottom: 1px solid var(--line);
  }
  .rr + .rr,
  .drop + .rr {
    border-top: 1px solid var(--line);
  }
  .chainic i {
    font-style: normal;
    font-size: 14px;
  }
  .chainic u {
    text-decoration: none;
    color: var(--muted);
    margin: 0 3px;
  }
  .legend {
    display: block;
  }
  .errs {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 10px 12px;
    border: 1px solid #b45309;
    border-radius: 6px;
    font: 12px "SF Mono", Menlo, monospace;
  }
  .diff {
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--bg);
    overflow: auto;
    max-height: 46vh;
    font: 12px/1.55 "SF Mono", Menlo, monospace;
    white-space: pre;
  }
  .dl {
    padding: 0 12px;
    color: var(--muted);
  }
  .dl.a {
    background: rgba(22, 163, 74, 0.18);
    color: var(--tx-green, #16a34a);
  }
  .dl.r {
    background: rgba(185, 28, 28, 0.18);
    color: var(--tx-red, #ef4444);
  }
  .diag {
    border: 1px solid var(--line);
    border-radius: 8px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 12px;
  }
  .row + .row {
    border-top: 1px solid var(--line);
  }
</style>
