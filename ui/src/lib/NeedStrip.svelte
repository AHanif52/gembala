<script lang="ts">
  import { word, type NeedRow } from "./attention";
  import { t, tOr } from "./i18n/state.svelte";
  import type { Key } from "./i18n";

  let { rows, simple, nextKey, titleOf, iconOf, dotOf, paramsOf, subOf, ageOf, onneed }: {
    rows: NeedRow[];
    simple: boolean;
    nextKey: string;
    titleOf: (row: NeedRow) => string;
    iconOf: (row: NeedRow) => string;
    dotOf: (kind: NeedRow["kind"]) => string;
    paramsOf: (row: NeedRow) => Record<string, string>;
    subOf: (row: NeedRow) => string;
    ageOf: (at: number) => string;
    onneed: (row: NeedRow) => void;
  } = $props();

  const GLYPH: Record<string, string> = { claude: "✳", omp: "π", codex: "◎", opencode: "◇", gemini: "✦", commandcode: "⌘" };
</script>

{#if rows.length}
  <div class="queue" role="region" aria-label={t("side.needsYou")}>
    <div class="qhead">{t("side.needsYou")} <span class="n">{rows.length}</span><small>{simple ? t("card.countHint") : t("need.next", { key: nextKey })}</small></div>
    {#each [1, 2, 3] as g (g)}
      {@const groupRows = rows.filter((r) => r.group === g)}
      {#if groupRows.length}
        {#if simple}<div class="grp">{tOr(`need.group.${g}`, "")}</div>{/if}
        {#each groupRows as r (r.key)}
          <div class="need {r.kind}">
            <button class="nrow" onclick={() => onneed(r)}>
              <span class="dot {dotOf(r.kind)}" title={t(word(r.kind) as Key)} aria-hidden="true"></span>
              <span class="nmain">
                <span class="ntitle">
                  {#if GLYPH[iconOf(r)]}<span class="aico {iconOf(r)}">{GLYPH[iconOf(r)]}</span>{/if}{titleOf(r)}
                </span>
                <span class="kind">{t(r.what as Key, paramsOf(r))}</span>
                {#if subOf(r)}<span class="sub">{subOf(r)}</span>{/if}
              </span>
              <span class="k">{ageOf(r.at)}</span>
            </button>
            <button class="act" onclick={() => onneed(r)}>{tOr(`need.act.${r.act}`, "")}</button>
          </div>
        {/each}
      {/if}
    {/each}
  </div>
{/if}

<style>
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
    gap: 6px;
    align-items: baseline;
    padding: 2px 8px;
    color: var(--tx-amber, #d97706);
    font-size: 11px;
    font-weight: 600;
    white-space: nowrap;
  }
  .qhead small {
    min-width: 0;
    margin-left: auto;
    overflow: hidden;
    color: var(--muted);
    font-weight: 400;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .qhead .n {
    padding: 0 6px;
    border-radius: 999px;
    background: rgba(217, 119, 6, 0.2);
    font-size: 10.5px;
  }
  .grp {
    padding: 5px 8px 1px;
    color: var(--tx-amber, #b45309);
    font-size: 10.5px;
    font-weight: 600;
  }
  .need {
    position: relative;
    display: flex;
  }
  .need .nrow {
    flex: 1;
    display: grid;
    grid-template-columns: 7px 1fr auto;
    gap: 6px;
    align-items: start;
    min-width: 0;
    padding: 5px 8px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .need .nrow:hover { background: rgba(217, 119, 6, 0.12); }
  .need .nrow .dot { margin-top: 5px; }
  .need .dot {
    display: inline-block;
    width: 7px;
    height: 7px;
    margin: 0 7px 1px 0;
    border-radius: 50%;
    background: var(--line);
  }
  .need .dot.blocked, .need .dot.permission, .need .dot.answer, .need .dot.password { background: #d97706; }
  .need .dot.approve { background: #a855f7; }
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
  .need .nmain {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .need .ntitle {
    overflow: hidden;
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .need .kind, .need .sub {
    overflow: hidden;
    color: var(--muted);
    font-size: 11px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .need .kind {
    display: -webkit-box;
    white-space: normal;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
  }
  .need .k { color: var(--muted); font-size: 10.5px; white-space: nowrap; }
  .need .act {
    display: none;
    position: absolute;
    right: 6px;
    top: 50%;
    transform: translateY(-50%);
    padding: 2px 8px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--card);
    color: var(--fg);
    font: 11px system-ui, -apple-system, sans-serif;
    cursor: pointer;
  }
  .need:hover .act, .need .act:focus-visible { display: inline-block; }
</style>
