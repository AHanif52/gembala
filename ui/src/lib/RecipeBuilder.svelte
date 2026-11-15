<script lang="ts">
  // FR-24, FR-25 (Harness 3b, B2 + B3): the card builder. It is another way to write the same recipe file: the UI keeps
  // the recipe as the daemon's JSON, `recipe.render` turns it into text + problems (nothing is written), and Simpan sends
  // only the hash of the text the user reviewed (`recipe.save`, SEC-27). Layout follows Gembala.pen screens 23-24:
  // cards side by side in the ALUR panel (scrolls sideways), Pustaka tahap + Resep panels below.
  import { untrack } from "svelte";
  import { call } from "./daemon";
  import { lineDiff } from "./diff";
  import { errText, t, tOr } from "./i18n/state.svelte";
  import {
    MAX_CHAIN, addAgent, addStep, canAddAgent, canMove, diagsFor, diagsRecipe, dropIndex, modelOf, moveAgent, moveStep, removeAgent, removeStep, same,
    setModel, setPrimary, skipped, slugId, used, type BRecipe, type BStep, type Diag, type LibAgent,
  } from "./recipe-builder";

  let { id, tpl = "", names = {}, active = true, onclose, onstate = () => {} }: {
    /// recipe id (file name); ignored for a new recipe
    id: string;
    /// template id from `recipe.library`: a new recipe (not a file yet)
    tpl?: string;
    /// agent kind -> "✳ Claude Code"
    names?: Record<string, string>;
    /// shown in the main area; a tab behind another one keeps its state but ignores keys
    active?: boolean;
    onclose: () => void;
    /// sidebar row of the tab: name, saved id (a new recipe gets one on save) and state
    onstate?: (s: { name: string; rid: string; st: string }) => void;
  } = $props();

  type Lib = { steps: BStep[]; templates: { id: string; recipe: BRecipe }[]; agents: LibAgent[] };
  type Prev = { text: string; hash: string; base: string };

  // svelte-ignore state_referenced_locally
  let curId = $state(id);
  // svelte-ignore state_referenced_locally
  let fresh = $state(!!tpl);
  // svelte-ignore state_referenced_locally
  let tplId = $state(tpl);
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
  let chain = $state<{ id: string; left: number; top: number } | null>(null);
  let menu = $state<string | null>(null);
  let tplMenu = $state(false);
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
  const short = (a: string) => label(a).replace(/^\S+ /, "");
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

  /// "Templat: X" on a new recipe: only the starting cards change, the name the user typed stays
  function applyTemplate(tid: string) {
    const tp = lib?.templates.find((x) => x.id === tid);
    if (!tp || !rec) return;
    const name = rec.name;
    rec = { ...JSON.parse(JSON.stringify(tp.recipe)), name };
    tplId = tid;
    tplMenu = false;
    chain = null;
  }

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

  export function leave() {
    if (dirty && !saved) leaving = true;
    else onclose();
  }
  $effect(() => {
    if (saved && rec && !same(rec, JSON.parse(base))) saved = false;
  });

  $effect(() => {
    if (!rec) return;
    const x = { name: rec.name, rid: fresh ? "" : curId, st: fresh ? "unsaved" : dirty ? "dirty" : status };
    untrack(() => onstate(x)); // the parent reads its own tab list in there; that must not become a dependency of this effect
  });

  const lines = $derived(prev ? lineDiff(prev.base, prev.text) : []);

  // ---- editing helpers ----
  const patch = (i: number, f: (s: BStep) => BStep) => rec && (rec.steps[i] = f($state.snapshot(rec.steps[i]) as BStep));
  const idxOf = (sid: string) => rec?.steps.findIndex((s) => s.id === sid) ?? -1;
  const mins = (secs: number) => t("rb.mins", { n: Math.round(secs / 60) });
  const timeouts = (cur: number) => [...new Set([300, 600, 900, 1500, 2400, 3600, cur])].sort((a, b) => a - b);
  const EFFORTS = ["low", "medium", "high", "max"];
  const toggleFallback = (k: string) => rec && (rec.fallback_on = rec.fallback_on.includes(k) ? rec.fallback_on.filter((x) => x !== k) : [...rec.fallback_on, k]);
  const remove = (sid: string) => {
    if (!rec) return;
    rec.steps = removeStep($state.snapshot(rec.steps) as BStep[], sid);
    menu = null;
    if (chain?.id === sid) chain = null;
  };

  // ---- drag (pointer events: HTML5 drag-and-drop does not fire `drop` reliably in WKWebView) ----
  type Drag = { scope: "flow" | "chain"; from: number; slot: number; ok: boolean };
  let drag = $state<Drag | null>(null);
  let cardEls = $state<HTMLElement[]>([]);
  let rowEls = $state<HTMLElement[]>([]);

  /// cards run left to right, chain rows top to bottom
  function track(e: PointerEvent, scope: Drag["scope"], from: number, els: () => HTMLElement[], test: (to: number) => boolean, apply: (to: number) => void) {
    e.preventDefault();
    const grip = e.currentTarget as HTMLElement;
    grip.setPointerCapture(e.pointerId);
    const across = scope === "flow";
    const target = (ev: PointerEvent) => {
      const mids = els().filter(Boolean).map((el) => {
        const r = el.getBoundingClientRect();
        return across ? r.left + r.width / 2 : r.top + r.height / 2;
      });
      const slot = dropIndex(mids, across ? ev.clientX : ev.clientY);
      return { slot, to: slot > from ? slot - 1 : slot };
    };
    drag = { scope, from, slot: from, ok: false };
    const move = (ev: PointerEvent) => {
      const { slot, to } = target(ev);
      drag = { scope, from, slot, ok: test(to) };
    };
    const end = (ev: PointerEvent) => {
      grip.removeEventListener("pointermove", move);
      grip.removeEventListener("pointerup", end);
      grip.removeEventListener("pointercancel", end);
      if (ev.type === "pointerup") {
        const { to } = target(ev);
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

  // the chain editor opens under its card, inside the ALUR panel (the card row scrolls, so the popover lives outside it)
  let alurEl = $state<HTMLElement>();
  let cardsEl = $state<HTMLElement>();
  function openChain(sid: string, i: number) {
    if (chain?.id === sid) return void (chain = null);
    const c = cardEls[i]?.getBoundingClientRect();
    const a = alurEl?.getBoundingClientRect();
    const k = cardsEl?.getBoundingClientRect();
    if (!c || !a || !k) return void (chain = { id: sid, left: 16, top: 200 });
    chain = { id: sid, left: Math.max(8, Math.min(c.left - a.left, a.width - 496)), top: k.bottom - a.top - 6 };
  }
  let libEl = $state<HTMLElement>();
  const focusLib = () => libEl?.scrollIntoView({ block: "nearest", behavior: "smooth" });

  function onkey(e: KeyboardEvent) {
    if (!active || e.key !== "Escape") return;
    const typing = document.activeElement instanceof HTMLInputElement || document.activeElement instanceof HTMLTextAreaElement;
    if (typing) return;
    e.preventDefault();
    if (leaving) leaving = false;
    else if (menu || tplMenu) (menu = null), (tplMenu = false);
    else if (chain) chain = null;
    else if (mode === "review") mode = "edit";
    else leave();
  }
  const chainStep = $derived(chain && rec ? rec.steps.find((s) => s.id === chain!.id) : undefined);
  const chainIdx = $derived(chain ? idxOf(chain.id) : -1);
</script>

<svelte:window onkeydown={onkey} onpointerdown={(e) => (e.target as HTMLElement).closest?.(".menu, .mbtn, .tplbtn") || ((menu = null), (tplMenu = false))} />

{#snippet card(s: BStep, i: number)}
  {@const errs = diagsFor(diags, s.id)}
  {@const a0 = s.agents[0]}
  <div class="scard" class:ship={s.human} class:err={errs.length > 0} class:lifted={drag?.scope === "flow" && drag.from === i} class:open={chain?.id === s.id} bind:this={cardEls[i]}>
    <div class="top">
      <span class="grip" class:lock={s.human} onpointerdown={(e) => dragStep(e, i)} role="button" tabindex="-1" aria-label={t("rb.grip")}>{s.human ? "🔒" : "⠿"}</span>
      <span class="num">{String(i + 1).padStart(2, "0")}</span>
      <span class="grow"></span>
      {#if !s.human}<span class="kind {kindOf(s)}">{t(`rb.kind.${kindOf(s)}`)}</span>
        <button class="mbtn" aria-label={t("rb.more")} onclick={() => (menu = menu === s.id ? null : s.id)}>⋯</button>{/if}
      {#if menu === s.id}<div class="menu" role="menu"><button class="danger" role="menuitem" onclick={() => remove(s.id)}>{t("rb.remove")}</button></div>{/if}
    </div>
    <b class="ttl">{stepName(s)}</b>
    {#if s.human}
      <p class="note">{t("rb.shipNote")}</p>
      <div class="foot ship"><span>{t("rb.shipLast")}</span><span>{t("rb.shipFixed")}</span></div>
    {:else}
      {#if s.shell}
        <p class="note">{t("rb.shellNote")}</p>
      {:else}
        <label class="fld"><span class="fl">{t("rb.agent")}</span>
          <select value={a0} onchange={(e) => patch(i, (x) => setPrimary(x, e.currentTarget.value))} aria-label={t("rb.agent")}>
            {#each [...new Set([...installed.map((a) => a.agent), ...s.agents])] as a}<option value={a}>{label(a)}</option>{/each}</select></label>
        {#if flags(a0).model}<label class="fld"><span class="fl">{t("rb.model")}</span>
          <input value={s.model} placeholder={t("rb.default")} aria-label={t("rb.model")} onchange={(e) => patch(i, (x) => setModel(x, a0, e.currentTarget.value))} /></label>{/if}
        {#if flags(a0).effort}<label class="fld"><span class="fl">{t("rb.effort")}</span>
          <select value={s.effort} aria-label={t("rb.effort")} onchange={(e) => patch(i, (x) => ({ ...x, effort: e.currentTarget.value }))}>
            <option value="">{t("rb.default")}</option>{#each EFFORTS as ef}<option value={ef}>{ef}</option>{/each}</select></label>{/if}
        <button class="chainlink" class:on={chain?.id === s.id} onclick={() => openChain(s.id, i)}>
          {s.agents.length > 1 ? t("rb.chainTo", { list: s.agents.slice(1).map(short).join(", ") }) : t("rb.chainAdd")}</button>
      {/if}
      {#each errs as d}<div class="errmsg">{diagText(d)}</div>{/each}
      <div class="foot">
        {#if !s.shell}<button class="gt" class:on={s.gate} aria-pressed={s.gate} title={t("rb.gate")} onclick={() => patch(i, (x) => ({ ...x, gate: !x.gate }))}>{s.gate ? t("rb.gateOn") : t("rb.gateOff")}</button>{/if}
        <select class="tm" value={s.timeout_secs} aria-label={t("rb.timeout")} onchange={(e) => patch(i, (x) => ({ ...x, timeout_secs: +e.currentTarget.value }))}>
          {#each timeouts(s.timeout_secs) as n}<option value={n}>{mins(n)}</option>{/each}</select>
        {#if s.output}<span class="out" title={tOr(`rb.pass.${s.pass || "reply"}`, s.pass)}>{s.pass === "first_line_pass" ? "PASS · " : ""}{s.output}</span>{/if}
      </div>
    {/if}
  </div>
{/snippet}

<div class="rb" role="region" aria-label={t("rb.title")}>
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
        <div class="head">
          <div class="who">
            <button class="back" onclick={leave}>← {t("rb.crumb")}</button>
            <input class="name" bind:value={rec.name} aria-label={t("rb.name")} maxlength="60" />
            <span class="fid">recipes/{fileId}.toml</span>
            <small>{t("rb.sub")}</small>
          </div>
          <span class="grow"></span>
          {#if diags.length}<span class="pill bad">{t("rb.errCount", { n: diags.length })}</span>{/if}
          <span class="pill {fresh || dirty ? 'chg' : status}">{fresh ? t("rb.unsaved") : dirty ? t("rb.dirty") : tOr(`hn.set.st.${status}`, status)}</span>
          <span class="seg" role="group" aria-label={t("rb.view")}>
            <button class:on={!compact} onclick={() => setCompact(false)}>{t("rb.detail")}</button>
            <button class:on={compact} onclick={() => setCompact(true)}>{t("rb.compact")}</button>
          </span>
          {#if fresh && lib}
            <span class="tplwrap"><button class="mini tplbtn" onclick={() => (tplMenu = !tplMenu)}>{t("rb.tpl", { name: lib.templates.find((x) => x.id === tplId)?.recipe.name ?? "…" })}</button>
              {#if tplMenu}<div class="menu left" role="menu">{#each lib.templates as tp (tp.id)}<button role="menuitem" onclick={() => applyTemplate(tp.id)}>{tp.recipe.name}</button>{/each}</div>{/if}</span>
          {/if}
          <button class="mini pri" disabled={busy} onclick={review}>✓ {t("rb.review")}</button>
        </div>
        {#if leaving}
          <div class="warn" role="alertdialog">{t("rb.leaveAsk")}
            <span class="acts"><button class="mini" onclick={() => (leaving = false)}>{t("rb.stay")}</button>
              <button class="mini danger" onclick={onclose}>{t("rb.discard")}</button></span></div>
        {/if}
        {#if saved}<div class="ok" role="status">{t("rb.savedOk")}</div>{/if}
        {#if fail}<div class="bad" role="alert">{fail}</div>{/if}
        {#each diagsRecipe(diags) as d}<div class="warn" role="alert">{diagText(d)}</div>{/each}

        <section class="alur" bind:this={alurEl}>
          <div class="sec"><span>{t("rb.flow")}</span><i></i><span>{t("rb.dragHint")}</span></div>
          {#if compact}
            <div class="rtab">
              <div class="rh"><span></span><span>#</span><span>{t("rb.col.step")}</span><span>{t("rb.col.chain")}</span><span>{t("rb.col.model")}</span><span>{t("rb.col.time")}</span><span>{t("rb.col.gate")}</span></div>
              {#each rec.steps as s, i (s.id)}
                <div class="rr" class:ship={s.human} class:err={diagsFor(diags, s.id).length > 0}>
                  <span class="grip" class:lock={s.human}>{s.human ? "" : "⠿"}</span>
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
            </div>
            <small class="legend">{installed.map((a) => label(a.agent)).join(" · ")} · {t("rb.legend")}</small>
          {:else}
            <div class="cards" class:dragging={!!drag} bind:this={cardsEl}>
              {#each rec.steps as s, i (s.id)}
                {#if drag?.scope === "flow" && drag.ok && drag.slot === i}<div class="dropv" aria-hidden="true"><span>{t("rb.drop")}</span></div>{/if}
                {#if s.human}
                  <button class="addcard" onclick={focusLib}><span class="plus">+</span>{t("rb.addStep")}</button><i class="wire"></i>
                {/if}
                {@render card(s, i)}
                {#if i < rec.steps.length - 1}<i class="wire"></i>{/if}
              {/each}
              {#if drag?.scope === "flow" && drag.ok && drag.slot === rec.steps.length}<div class="dropv" aria-hidden="true"><span>{t("rb.drop")}</span></div>{/if}
            </div>
          {/if}
          <p class="rule">↵ {t("rb.rule", { n: rec.max_rounds })}</p>

          {#if chain && chainStep && !chainStep.shell && !chainStep.human}
            {@const s = chainStep}
            {@const i = chainIdx}
            <!-- layar 24: the fallback chain of this step (FR-25) -->
            <div class="chainbox" style="left:{chain.left}px; top:{chain.top}px">
              <div class="ch"><span class="lbl">{t("rb.chain")} · {stepName(s)}</span><span class="grow"></span><small>{t("rb.chainCount", { n: s.agents.length, max: MAX_CHAIN })}</small></div>
              <p class="hint">{s.kind === "check" ? t("rb.chainCheck") : t("rb.chainWork")}</p>
              {#each s.agents as a, j (a)}
                <div class="crow" bind:this={rowEls[j]} class:lifted={drag?.scope === "chain" && drag.from === j} class:target={drag?.scope === "chain" && drag.from !== j && drag.slot === j}>
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
              <div class="acts"><button class="mini pri" onclick={() => (chain = null)}>{t("rb.chainDone")}</button></div>
            </div>
          {/if}
        </section>

        <div class="below">
          <section class="panel" bind:this={libEl}>
            <h4>{t("rb.library")}</h4>
            <div class="libgrid">
              {#each lib?.steps ?? [] as c (c.id)}
                <button class="li" disabled={used(rec.steps, c.id)} onclick={() => rec && (rec.steps = addStep($state.snapshot(rec.steps) as BStep[], c))}>
                  <b>{c.name || c.id}</b>
                  <small>{used(rec.steps, c.id) ? t("rb.inFlow") : t(`rb.kind.${c.human ? "gate" : c.shell ? "shell" : c.kind === "check" ? "check" : "work"}`)}</small></button>
              {/each}
              <button class="li cust" disabled title={t("rb.customSoon")}><b>{t("rb.custom")}</b><small>{t("rb.customSub")}</small></button>
            </div>
          </section>

          <section class="panel">
            <h4>{t("rb.recipe")}</h4>
            <div class="prow"><span class="grow">{t("rb.rounds")}</span>
              <select value={rec.max_rounds} aria-label={t("rb.rounds")} onchange={(e) => rec && (rec.max_rounds = +e.currentTarget.value)}>{#each [0, 1, 2, 3] as n}<option value={n}>{t("rb.roundsOf", { n })}</option>{/each}</select></div>
            <div class="prow"><span class="grow">{t("rb.total")}</span>
              <select value={rec.total_secs} aria-label={t("rb.total")} onchange={(e) => rec && (rec.total_secs = +e.currentTarget.value)}>
                {#each [...new Set([1800, 3600, 5400, 7200, rec.total_secs])].sort((a, b) => a - b) as n}<option value={n}>{n < 3600 ? t("hn.set.mins", { n: n / 60 }) : t("hn.set.hours", { n: n / 3600 })}</option>{/each}</select></div>
            <div class="prow"><span class="grow">{t("rb.saver")}</span>
              <input type="checkbox" role="switch" checked={rec.token_saver} aria-label={t("rb.saver")} onchange={(e) => rec && (rec.token_saver = e.currentTarget.checked)} /></div>
            <div class="prow col"><span>{t("rb.trigger")}</span>
              <div class="chips">
                {#each ["exit", "timeout", "unknown"] as k}
                  <button class="chip" class:off={!rec.fallback_on.includes(k)} aria-pressed={rec.fallback_on.includes(k)} onclick={() => toggleFallback(k)}>{tOr(`rb.fb.${k}`, k)}</button>
                {/each}
                <span class="chip off dead" title={t("rb.limitOff")}>{t("rb.fb.limit")} · {t("rb.limitOff")}</span>
              </div>
              <small>{t("rb.triggerNote")}</small></div>
            <div class="hard">{t("rb.hard")}</div>
          </section>
        </div>
      </div>
    {/if}
</div>

<style>
  .rb {
    width: 100%;
    height: 100%;
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
    gap: 12px;
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
  .mono,
  .fid {
    font: 11.5px "SF Mono", Menlo, monospace;
  }
  .back {
    align-self: flex-start;
    padding: 2px 0;
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
  input:not([type="checkbox"]),
  .mini,
  .seg button {
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
  .mini.danger,
  .danger {
    color: var(--tx-red, #ef4444);
  }
  .mini:disabled,
  .li:disabled {
    opacity: 0.55;
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
    align-items: flex-start;
    gap: 10px;
  }
  .head > :not(.who):not(.grow) {
    margin-top: 18px;
  }
  .who {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  input.name {
    width: 300px;
    padding: 2px 6px;
    margin-left: -6px;
    border-color: transparent;
    background: transparent;
    font-size: 20px;
    font-weight: 600;
  }
  input.name:hover,
  input.name:focus {
    border-color: var(--line);
    background: var(--bg);
  }
  .fid {
    color: var(--muted);
  }
  .pill {
    padding: 2px 8px;
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
  .tplwrap {
    position: relative;
  }
  .menu {
    position: absolute;
    right: 0;
    top: 100%;
    z-index: 6;
    display: flex;
    flex-direction: column;
    min-width: 150px;
    padding: 4px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    box-shadow: 0 8px 20px rgba(0, 0, 0, 0.4);
  }
  .menu button {
    padding: 6px 10px;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
  }
  .menu button:hover {
    background: var(--line);
  }
  .menu.left {
    right: auto;
    left: 0;
  }

  /* ALUR panel: cards side by side, scrolls sideways when 6+ stages do not fit (Gembala.pen 23) */
  .alur {
    position: relative;
    padding: 14px 16px 12px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--bg);
  }
  .sec {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 12px;
    color: var(--muted);
    font: 10.5px "SF Mono", Menlo, monospace;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }
  .sec i {
    flex: 1;
    height: 1px;
    background: var(--line);
  }
  .cards {
    display: flex;
    align-items: stretch;
    padding: 4px 2px 12px;
    overflow-x: auto;
  }
  .cards.dragging {
    user-select: none;
    cursor: grabbing;
  }
  .wire {
    flex: none;
    align-self: center;
    width: 22px;
    border-top: 1px dashed var(--muted);
    opacity: 0.6;
  }
  .scard {
    position: relative;
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: 190px;
    padding: 10px 12px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--card);
  }
  .scard.open {
    border-color: var(--pri-bg, #3b82f6);
  }
  .scard.ship {
    border-color: var(--tx-amber, #d97706);
    min-height: 232px;
  }
  .scard.err,
  .rr.err {
    border-color: var(--tx-red, #ef4444);
  }
  .lifted {
    opacity: 0.4;
  }
  .top {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--muted);
  }
  .grip {
    flex: none;
    width: 14px;
    color: var(--muted);
    cursor: grab;
    touch-action: none;
    user-select: none;
  }
  .grip.lock {
    cursor: default;
  }
  .num {
    font: 11px "SF Mono", Menlo, monospace;
  }
  .kind {
    padding: 1px 6px;
    border-radius: 4px;
    font: 10px "SF Mono", Menlo, monospace;
    letter-spacing: 0.06em;
    background: var(--line);
    color: var(--fg);
  }
  .mbtn {
    padding: 0 4px;
    border: 0;
    background: none;
    color: var(--muted);
    font-size: 16px;
    line-height: 1;
  }
  .ttl {
    font-size: 17px;
  }
  .note {
    margin: 0;
    color: var(--muted);
    font-size: 12px;
    line-height: 1.4;
  }
  .fld {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
  }
  .fl {
    flex: none;
    width: 46px;
    color: var(--muted);
    font: 9.5px "SF Mono", Menlo, monospace;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .fld select,
  .fld input:not([type="checkbox"]) {
    flex: 1;
    min-width: 0;
    padding: 6px 0;
    border: 0;
    background: transparent;
  }
  .chainlink {
    padding: 0;
    border: 0;
    background: none;
    color: var(--pri-bg, #3b82f6);
    font: 12px system-ui;
    text-align: left;
    cursor: pointer;
  }
  .chainlink.on {
    text-decoration: underline;
  }
  .foot {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 10px;
    margin-top: auto;
    padding-top: 8px;
    border-top: 1px solid var(--line);
    color: var(--muted);
    font: 10px "SF Mono", Menlo, monospace;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .foot.ship {
    flex-direction: column;
    align-items: flex-start;
    border-top: 0;
  }
  .foot.ship span:first-child {
    color: var(--tx-amber, #d97706);
  }
  .foot select,
  .foot .gt {
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }
  .foot .gt.on {
    color: var(--tx-amber, #d97706);
  }
  .out {
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
    white-space: nowrap;
    text-transform: none;
  }
  .addcard {
    flex: none;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    width: 96px;
    border: 1px dashed var(--muted);
    border-radius: 10px;
    background: none;
    color: var(--fg);
    font: inherit;
  }
  .plus {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border: 1px solid var(--line);
    border-radius: 50%;
    font-size: 18px;
  }
  .dropv {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 64px;
    padding: 0 4px;
    margin: 0 4px;
    overflow: hidden;
    border: 1.5px dashed var(--pri-bg, #3b82f6);
    border-radius: 10px;
    color: var(--pri-bg, #3b82f6);
    font-size: 11px;
    text-align: center;
  }
  .rule {
    margin: 0;
    color: var(--muted);
    font: 10.5px "SF Mono", Menlo, monospace;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .errmsg,
  .warn,
  .ok {
    padding: 6px 8px;
    border-radius: 5px;
    font-size: 12px;
  }
  .errmsg {
    background: rgba(185, 28, 28, 0.15);
    color: var(--tx-red, #ef4444);
  }
  .warn {
    display: flex;
    align-items: center;
    gap: 8px;
    background: rgba(217, 119, 6, 0.15);
    color: var(--tx-amber, #d97706);
  }
  .ok {
    background: rgba(22, 163, 74, 0.15);
    color: var(--tx-green, #16a34a);
  }
  .bad {
    color: var(--tx-red, #ef4444);
  }

  /* fallback chain editor: under its card, over the panels below (Gembala.pen 24) */
  .chainbox {
    position: absolute;
    z-index: 5;
    width: 480px;
    padding: 12px 14px;
    border: 1px solid var(--pri-bg, #3b82f6);
    border-radius: 10px;
    background: var(--card);
    box-shadow: 0 12px 28px rgba(0, 0, 0, 0.45);
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
    grid-template-columns: 16px 22px 138px 1fr 118px 22px;
    gap: 8px;
    align-items: center;
    padding: 6px 8px;
    margin: 6px 0;
    border: 1px solid var(--line);
    border-radius: 7px;
    background: var(--bg);
  }
  .crow.target {
    border-top: 2px solid var(--pri-bg, #3b82f6);
  }
  .no {
    color: var(--muted);
    font: 11px "SF Mono", Menlo, monospace;
  }
  .mi {
    min-width: 0;
    font: 12px "SF Mono", Menlo, monospace;
  }
  .mi.sm {
    width: 80px;
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
    opacity: 0.4;
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

  .below {
    display: grid;
    grid-template-columns: 1fr 380px;
    gap: 16px;
    align-items: start;
  }
  .panel {
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--bg);
  }
  .panel h4 {
    margin: 0;
    padding: 12px 14px 8px;
    color: var(--muted);
    font: 10.5px "SF Mono", Menlo, monospace;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }
  .libgrid {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 8px;
    padding: 0 14px 14px;
  }
  .li {
    padding: 9px 11px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--card);
    color: var(--fg);
    font: inherit;
    text-align: left;
  }
  .li small {
    display: block;
    font: 9.5px "SF Mono", Menlo, monospace;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .li.cust {
    border-color: var(--pri-bg, #3b82f6);
  }
  .prow {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
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
    padding: 2px 9px;
    border: 1px solid var(--pri-bg, #3b82f6);
    border-radius: 999px;
    background: none;
    color: var(--fg);
    font: 10.5px "SF Mono", Menlo, monospace;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .chip.off {
    border-color: var(--line);
    color: var(--muted);
  }
  .chip.dead {
    border-style: dashed;
  }
  .hard {
    padding: 8px 14px 12px;
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
    grid-template-columns: 16px 24px 190px 1fr 140px 70px 40px;
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
  .rr + .rr {
    border-top: 1px solid var(--line);
  }
  .rr.ship {
    background: rgba(217, 119, 6, 0.08);
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
    margin-top: 6px;
  }
  .errs {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 10px 12px;
    border: 1px solid var(--tx-amber, #d97706);
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
