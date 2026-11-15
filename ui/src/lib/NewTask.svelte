<script lang="ts">
  // FR-73: "Tugas baru". A task box, the folder, and the way to run it: by hand (one agent pane, the task as its first
  // prompt) or by a harness recipe. The recipe must be approved (SEC-27); the dialog shows what blocks the run
  // (harness.preflight) and starts nothing until every blocking item is ok.
  import { invoke } from "@tauri-apps/api/core";
  import { call } from "./daemon";
  import { canStart, testCommand } from "./harness";
  import { errText, t, tOr } from "./i18n/state.svelte";
  import { isMac, keyText, mod } from "./keys.svelte";

  type Row = { id: string; status: "approved" | "needs_approval" | "changed" | "broken"; hash?: string; recipe?: { name: string; steps: { id: string; name: string; shell: boolean }[] } | null; errors: any[]; warnings: any[] };
  let {
    ws,
    cwd,
    brief,
    agents,
    simple = false,
    names = {},
    ontemplate,
    onclose,
    onstarted,
    onmanual,
    onrecipe,
  }: {
    ws: { id: string; name: string };
    cwd: string;
    brief?: string;
    agents: string[];
    /// FR-97: everyday words, folder by name, technical fields folded
    simple?: boolean;
    /// agent kind -> shown name with its glyph ("✳ Claude Code"), never the raw id
    names?: Record<string, string>;
    /// FR-99 P1 / P4: "Kerja bareng beberapa agent" opens the role template dialog
    ontemplate?: (task: string) => void;
    onclose: () => void;
    onstarted: (snap: any) => void;
    onmanual: (o: { agent: string; task: string; cwd: string }) => void;
    /// 3b: open the card builder on the picked recipe
    onrecipe?: (id: string) => void;
  } = $props();

  // the dialog is opened fresh each time, so these start from the props once on purpose
  // svelte-ignore state_referenced_locally
  const KEY = `gembala.newtask.${ws.id}`;
  const saved = (() => {
    try {
      return JSON.parse(localStorage.getItem(KEY) ?? "{}");
    } catch {
      return {};
    }
  })();

  let task = $state("");
  // svelte-ignore state_referenced_locally
  let folder = $state(cwd);
  // FR-99 P1: a new user starts by hand; the last choice is kept per workspace after that
  // svelte-ignore state_referenced_locally
  let mode = $state<"manual" | "harness" | "team">(saved.mode === "harness" || (saved.mode === "team" && ontemplate) ? saved.mode : "manual");
  // svelte-ignore state_referenced_locally
  let agent = $state<string>(agents.includes(saved.agent) ? saved.agent : (agents[0] ?? ""));
  let recipeId = $state<string>(saved.recipe ?? "");
  // svelte-ignore state_referenced_locally
  let test = $state(testCommand(brief));
  let rows = $state<Row[]>([]);
  let items = $state<{ check: string; ok: boolean; blocking: boolean; detail: string }[] | undefined>();
  let review = $state<{ id: string; text: string; hash: string } | null>(null);
  let error = $state("");
  let busy = $state(false);

  const row = $derived(rows.find((r) => r.id === recipeId));
  const needsTest = $derived(!!row?.recipe?.steps.some((s) => s.shell));
  // FR-99 P3: an empty command is allowed: the test stage is dropped for this run (`skip_shell`), and the chain shows it so
  const skipTest = $derived(needsTest && !test.trim());
  let testOpen = $state(false);
  /// the simple mode keeps the field folded while the command is unknown (L5b); the full mode always shows it
  const testShown = $derived(needsTest && (!simple || testOpen || !!test.trim()));
  const pfParams = () => ({ recipe: recipeId, cwd: folder, workspace: ws.id, test: needsTest ? test.trim() : "", skip_shell: skipTest });
  const ready = $derived(task.trim().length > 0 && folder.startsWith("/") && (mode === "manual" ? !!agent : mode === "team" ? true : canStart(items)));

  $effect(() => {
    try {
      localStorage.setItem(KEY, JSON.stringify({ mode, agent, recipe: recipeId }));
    } catch {}
  });

  async function load() {
    rows = ((await call("recipe.list")) as Row[] | null) ?? [];
    if (!rows.some((r) => r.id === recipeId)) recipeId = (rows.find((r) => r.status === "approved") ?? rows[0])?.id ?? "";
  }
  load().catch((e) => (error = errText(e)));

  // preflight follows what the user types, a beat after the last key
  $effect(() => {
    const params = pfParams();
    if (mode !== "harness" || !recipeId || !folder.startsWith("/")) {
      items = undefined;
      return;
    }
    const h = setTimeout(() => {
      call("harness.preflight", params).then((r) => (items = r.items)).catch((e) => (error = errText(e)));
    }, 250);
    return () => clearTimeout(h);
  });

  async function open(id: string) {
    const r = await call("recipe.get", { id });
    review = r.error ? null : { id, text: r.text, hash: r.hash };
    if (r.error) error = errText(r.error, r.args);
  }
  async function approve() {
    if (!review) return;
    const r = await call("recipe.approve", { id: review.id, hash: review.hash });
    if (r.error) error = errText(r.error, r.args);
    else review = null;
    await load();
    const p = await call("harness.preflight", pfParams());
    items = p.items;
  }
  async function pickFolder() {
    const f = await invoke<string | null>("pick_folder").catch(() => null);
    if (f) folder = f;
  }
  async function go() {
    if (!ready || busy) return;
    error = "";
    if (mode === "team") return ontemplate?.(task.trim());
    if (mode === "manual") return onmanual({ agent, task: task.trim(), cwd: folder });
    busy = true;
    try {
      const r = await call("harness.start", { ...pfParams(), request: task });
      if (r.error) error = r.error === "preflight" ? todo(r.args?.check) : errText(r.error, r.args);
      else onstarted(r);
    } catch (e) {
      error = errText(e);
    } finally {
      busy = false;
    }
  }
  const chain = (r?: Row) => r?.recipe?.steps.filter((s) => !(skipTest && s.shell)).map((s) => s.name).join(" → ") ?? "";
  /// "Tanya agent": the default agent gets one question in a pane of its own; the answer is typed here by the user, never read back
  const askAgent = () => onmanual({ agent, task: t("hn.testAsk"), cwd: folder });
  /// a failed check reads as what to do ("Isi perintah test dulu"), a passed one as the fact (audit u4 #20)
  const todo = (check: string) => tOr(`hn.fix.${check}`, tOr(`hn.check.${check}`, check));
  /// the reason the start button is off, next to it (audit #23)
  const why = $derived.by(() => {
    if (!task.trim()) return t("hn.why.task");
    if (!folder.startsWith("/")) return t("hn.why.folder");
    if (mode === "manual") return agent ? "" : t("hn.why.agent");
    if (mode === "team") return "";
    const bad = items?.find((i) => !i.ok && i.blocking);
    return bad ? todo(bad.check) : items ? "" : "…";
  });
  const diag = (d: any) => errText(d.code, d.args);
  function key(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      if (review) review = null;
      else onclose();
    } else if (e.key === "Enter" && (mod(e) || (isMac && e.ctrlKey))) go();
  }
</script>

<svelte:window onkeydowncapture={key} />

<div class="veil" role="presentation" onmousedown={onclose}></div>
<div class="card" role="dialog" aria-label={t("hn.title")} aria-modal="true">
  <div class="head"><b>{t("hn.title")}</b><span class="m">{simple ? t("hn.inProject", { name: ws.name }) : t("hn.inWs", { name: ws.name })}</span></div>

  <textarea rows="5" bind:value={task} placeholder={simple ? t("hn.taskPlaceholderS") : t("hn.taskPlaceholder")} aria-label={t("hn.task")}></textarea>

  <label class="line">
    <span class="lab">{t("hn.folder")}</span>
    {#if simple}
      <!-- FR-99 P1: the folder by its name; the full path stays in the tooltip -->
      <span class="grow" title={folder}>{folder.split("/").filter(Boolean).pop() ?? folder}</span>
    {:else}
      <input class="grow mono" bind:value={folder} spellcheck="false" />
    {/if}
    <button class="mini" onclick={pickFolder}>{t("hn.pick")}</button>
  </label>
  <!-- FR-84: whether a folder is trusted lives in the agent's own config (SEC-15), so it's a reminder, not a check -->
  <p class="hint">{simple ? t("hn.trustHintS") : t("hn.trustHint")}</p>

  <div class="seg" role="radiogroup" aria-label={t("hn.how")}>
    <button role="radio" aria-checked={mode === "manual"} class:on={mode === "manual"} onclick={() => (mode = "manual")}>{simple ? t("hn.manualS") : t("hn.manual")}</button>
    <button role="radio" aria-checked={mode === "harness"} class:on={mode === "harness"} onclick={() => (mode = "harness")}>{simple ? t("hn.harnessS") : t("hn.harness")}</button>
    {#if ontemplate}<button role="radio" aria-checked={mode === "team"} class:on={mode === "team"} onclick={() => (mode = "team")}>{t("hn.team3")}</button>{/if}
  </div>

  {#if mode === "manual"}
    <label class="line">
      <span class="lab">{t("hn.agent")}</span>
      <select bind:value={agent}>
        {#each agents as a (a)}<option value={a}>{names[a] ?? a}</option>{/each}
      </select>
    </label>
    <p class="hint">{simple ? t("hn.manualWhenS") : t("hn.manualWhen")}</p>
  {:else if mode === "team"}
    <p class="hint">{t("hn.team3Sub")}</p>
  {:else}
    <label class="line">
      <span class="lab">{t("hn.recipe")}</span>
      <select bind:value={recipeId}>
        {#each rows as r (r.id)}<option value={r.id}>{r.recipe?.name ?? r.id}{r.status === "approved" ? "" : ` · ${tOr(`hn.status.${r.status}`, r.status)}`}</option>{/each}
      </select>
      {#if onrecipe && recipeId}<button type="button" class="mini" onclick={(e) => (e.preventDefault(), onrecipe(recipeId))}>{t("rb.setRecipe")}</button>{/if}
    </label>
    {#if row}
      <div class="chain">{row.recipe ? chain(row) : ""}</div>
      {#each row.errors as d}<div class="bad">{diag(d)}</div>{/each}
      {#if !simple}{#each row.warnings as d}<div class="warn">{diag(d)}</div>{/each}{/if}
      {#if row.status !== "approved" && row.status !== "broken"}
        <div class="line"><span class="m grow">{tOr(`hn.status.${row.status}Sub`, row.status)}</span><button class="mini" onclick={() => open(row.id)}>{t("hn.review")}</button></div>
      {/if}
    {/if}
    {#if testShown}
      <div class="line">
        <label class="lab wide" for="nt-test">{t("hn.test")} <span class="m">{t("hn.testOpt")}</span></label>
        <input id="nt-test" class="grow mono" bind:value={test} placeholder={t("hn.testPlaceholder")} spellcheck="false" />
        <button type="button" class="mini" title={t("hn.askTip")} onclick={askAgent} disabled={!agent}>{t("hn.ask")}</button>
      </div>
      <p class="hint">{simple ? t("hn.testHelpS") : t("hn.testHelp")}</p>
    {:else if needsTest}
      <div class="line note"><span class="grow">ℹ {t("hn.testSkipped")}</span><button type="button" class="mini" onclick={() => (testOpen = true)}>{t("hn.testFill")}</button></div>
    {/if}
    {#if items}
      <ul class="pre" aria-label={t("hn.preflight")}>
        {#each items as i (i.check)}
          <li class:ok={i.ok} class:bad={!i.ok && i.blocking} class:warn={!i.ok && !i.blocking}>
            <span aria-hidden="true">{i.ok ? "✓" : i.blocking ? "✕" : "!"}</span>
            {i.ok ? tOr(`hn.check.${i.check}`, i.check) : todo(i.check)}{#if i.detail && !i.ok}<span class="m"> · {tOr(`hn.detail.${i.detail}`, i.detail)}</span>{/if}
          </li>
        {/each}
      </ul>
    {/if}
    <p class="hint">{t("hn.harnessWhen")}</p>
  {/if}

  {#if error}<div class="bad" role="alert">{error}</div>{/if}
  <div class="acts">
    {#if why && !busy}<span class="m why grow" role="status">{why}</span>{/if}
    <button class="mini" onclick={onclose}>{t("hn.cancel")} <span class="k">Esc</span></button>
    <button class="mini go" disabled={!ready || busy} onclick={go}>{mode === "team" ? t("hn.next") : t("hn.start")} <span class="k">{keyText("Cmd+Enter")}</span></button>
  </div>

  {#if review}
    <div class="review" role="dialog" aria-label={t("hn.reviewTitle", { name: review.id })}>
      <div class="head"><b>{t("hn.reviewTitle", { name: review.id })}</b></div>
      <p class="hint">{t("hn.reviewHint")}</p>
      <pre>{review.text}</pre>
      <div class="acts">
        <button class="mini" onclick={() => (review = null)}>{t("hn.cancel")} <span class="k">Esc</span></button>
        <button class="mini go" onclick={approve}>{t("hn.approve")}</button>
      </div>
    </div>
  {/if}
</div>

<style>
  .veil {
    position: absolute;
    inset: 0;
    z-index: 7;
    background: rgba(0, 0, 0, 0.35);
  }
  .card {
    position: absolute;
    left: 50%;
    top: 48px;
    transform: translateX(-50%);
    z-index: 8;
    width: min(560px, 92%);
    max-height: calc(100% - 96px);
    overflow: auto;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px 16px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
  }
  .head {
    display: flex;
    gap: 8px;
    align-items: baseline;
    font-size: 14px;
  }
  textarea,
  input,
  select {
    box-sizing: border-box;
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--bg);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
  }
  textarea {
    resize: vertical;
    width: 100%;
  }
  .mono {
    font: 12px "SF Mono", Menlo, monospace;
  }
  .line {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .lab.wide {
    width: auto;
    min-width: 64px;
  }
  .note {
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    color: var(--muted);
    font-size: 12px;
  }
  .why {
    align-self: center;
  }
  .lab {
    width: 64px;
    flex: none;
    color: var(--muted);
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .m,
  .hint {
    color: var(--muted);
    font-size: 12px;
  }
  .hint {
    margin: 0;
    line-height: 1.45;
  }
  .seg {
    display: flex;
    gap: 2px;
    padding: 2px;
    border: 1px solid var(--line);
    border-radius: 6px;
    align-self: flex-start;
  }
  .seg button {
    padding: 4px 12px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--muted);
    font: inherit;
  }
  .seg button.on {
    background: var(--hover, rgba(127, 127, 127, 0.2));
    color: var(--fg);
  }
  .chain {
    color: var(--muted);
    font-size: 12px;
  }
  .pre {
    list-style: none;
    margin: 0;
    padding: 6px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .ok span:first-child {
    color: var(--tx-green, #16a34a);
  }
  li.bad,
  div.bad {
    color: var(--tx-red, #dc2626);
  }
  li.warn,
  div.warn {
    color: var(--tx-amber, #d97706);
  }
  .acts {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .mini {
    padding: 4px 10px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--bg);
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .mini:disabled {
    opacity: 0.45;
  }
  .mini.go {
    border-color: var(--accent, #3b82f6);
  }
  .k {
    color: var(--muted);
  }
  .review {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px 16px;
    border-radius: 8px;
    background: var(--card);
    overflow: auto;
  }
  pre {
    margin: 0;
    flex: 1;
    overflow: auto;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    white-space: pre-wrap;
    font: 12px "SF Mono", Menlo, monospace;
  }
</style>
