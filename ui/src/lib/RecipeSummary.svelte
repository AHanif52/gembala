<script lang="ts">
  // FR-99 P3 (layar L1): the recipe in one sentence per stage, for the approve step. The TOML stays behind "Lihat file".
  import { t } from "./i18n/state.svelte";
  import { describeLimits, describeRecipe, type BRecipe } from "./recipe-builder";

  let { recipe, names = {} }: { recipe: BRecipe; names?: Record<string, string> } = $props();

  // "✳ Claude Code" / "π Oh My Pi" -> the sentence reads better without the one-character glyph
  const name = (a: string) => (names[a] ?? a).replace(/^\S\s+(?=\S)/u, "");
  const tr = (k: string, p?: Record<string, string | number>) => t(k as never, p);
  const rows = $derived(describeRecipe(recipe, name, tr));
  const limits = $derived(describeLimits(recipe, tr));
</script>

<ol class="sum" aria-label={t("hn.sum.aria")}>
  {#each rows as r, i (r.id)}
    <li>
      <span class="n">{String(i + 1).padStart(2, "0")}</span>
      <span class="what"><b>{r.title}</b><span class="tag {r.kind}">{t(`hn.sum.kind.${r.kind}` as never)}</span></span>
      <span class="s">{r.sentence}{#if r.note}<small>{r.note}</small>{/if}</span>
    </li>
  {/each}
</ol>
{#if limits}<p class="lim">{limits}</p>{/if}

<style>
  .sum {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: 8px;
  }
  li {
    display: grid;
    grid-template-columns: 28px 170px 1fr;
    gap: 10px;
    padding: 9px 12px;
    align-items: start;
  }
  li + li {
    border-top: 1px solid var(--line);
  }
  .n {
    color: var(--muted);
    font: 12px "SF Mono", Menlo, monospace;
    padding-top: 2px;
  }
  .what {
    display: flex;
    flex-direction: column;
    gap: 3px;
    align-items: flex-start;
  }
  .tag {
    padding: 0 6px;
    border-radius: 4px;
    border: 1px solid var(--line);
    color: var(--muted);
    font: 600 10px system-ui, -apple-system, sans-serif;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .tag.check {
    color: var(--tx-blue, #2563eb);
  }
  .tag.gate {
    color: var(--tx-amber, #b45309);
  }
  .s {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  small,
  .lim {
    color: var(--muted);
    font-size: 12px;
  }
  .lim {
    margin: 6px 0 0;
  }
</style>
