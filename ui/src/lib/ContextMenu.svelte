<script module lang="ts">
  /// Right-click menu item (FR-37, FR-44, FR-52): [label, action | sub-items, enabled?, color?, extra?], "-" = separator.
  /// A submenu whose items all carry a color renders as a swatch panel (Gembala.pen 08).
  export type MItem = "-" | [string, (() => void) | MItem[], boolean?, string?, MExtra?];
  /// key = shortcut shown on the right (only one that really works); on = current choice; title/hint = swatch panel text
  export type MExtra = { icon?: string; key?: string; danger?: boolean; on?: boolean; title?: string; hint?: string };
  export type MenuAt = { x: number; y: number; items: MItem[] };
  export const HEX: Record<string, string> = { red: "#ef4444", orange: "#f97316", yellow: "#eab308", green: "#22c55e", blue: "#3b82f6", purple: "#a855f7", pink: "#ec4899", cyan: "#06b6d4", brown: "#a16207" };
</script>

<script lang="ts">
  import { tick, untrack } from "svelte";

  let { menu = $bindable() }: { menu: MenuAt | null } = $props();

  type Level = { items: MItem[]; x: number; y: number; sel: number; swatch: boolean; title?: string; hint?: string; shown: boolean };
  const COLS = 5;
  let levels = $state<Level[]>([]);
  let els: HTMLElement[] = $state([]);
  let back: HTMLElement | null = null;

  const real = (it: MItem): it is Exclude<MItem, "-"> => it !== "-";
  const usable = (it: MItem) => real(it) && it[2] !== false;

  $effect(() => {
    const m = menu;
    untrack(() => {
      if (!m) return void (levels = []);
      back = document.activeElement as HTMLElement | null;
      levels = [];
      open(0, m.items, { left: m.x, right: m.x, top: m.y }, true);
    });
  });

  /// lay out a level next to `from` (the cursor or the parent item), then flip left/up if it would leave the window
  async function open(i: number, items: MItem[], from: { left: number; right: number; top: number }, root = false, parent?: MExtra) {
    const swatch = !root && items.filter(real).length > 0 && items.filter(real).every((it) => !!it[3]);
    levels = [...levels.slice(0, i), { items: swatch ? items.filter(real) : items, x: from.right, y: from.top, sel: -1, swatch, title: parent?.title, hint: parent?.hint, shown: false }];
    await tick();
    const el = els[i];
    const L = levels[i];
    if (!el || !L) return;
    const { width: w, height: h } = el.getBoundingClientRect();
    if (L.x + w > innerWidth - 4) L.x = root ? L.x - w : from.left - w;
    if (L.y + h > innerHeight - 4) L.y = root ? L.y - h : innerHeight - 4 - h;
    L.x = Math.max(4, L.x);
    L.y = Math.max(4, L.y);
    L.shown = true;
    (el.querySelector<HTMLElement>(`[data-i="${L.sel}"]`) ?? el).focus();
  }

  function close() {
    menu = null;
    if (back?.isConnected) back.focus();
  }

  function pick(i: number, j: number, keyboard = false) {
    const L = levels[i];
    const it = L?.items[j];
    if (!it || !usable(it) || !real(it)) return;
    L.sel = j;
    if (Array.isArray(it[1])) {
      const r = els[i]?.querySelector(`[data-i="${j}"]`)?.getBoundingClientRect();
      if (r) open(i + 1, it[1], { left: r.left - 4, right: r.right + 4, top: r.top - 5 }, false, it[4]).then(() => {
        if (keyboard && levels[i + 1]) focusSel(i + 1, first(i + 1));
      });
      return;
    }
    const go = it[1];
    close();
    go();
  }

  const first = (i: number) => levels[i].items.findIndex(usable);
  function focusSel(i: number, j: number) {
    levels[i].sel = j;
    tick().then(() => els[i]?.querySelector<HTMLElement>(`[data-i="${j}"]`)?.focus());
  }
  function step(i: number, d: number) {
    const L = levels[i];
    const n = L.items.length;
    let j = L.sel;
    for (let k = 0; k < n; k++) {
      j = j < 0 ? (d > 0 ? 0 : n - 1) : (j + d + n) % n;
      if (usable(L.items[j])) return focusSel(i, j);
    }
  }

  function hover(i: number, j: number) {
    const L = levels[i];
    if (!L || (L.sel === j && levels.length > i + 1)) return;
    L.sel = j;
    levels = levels.slice(0, i + 1);
    const it = L.items[j];
    if (real(it) && Array.isArray(it[1]) && usable(it)) pick(i, j);
  }

  function onKey(e: KeyboardEvent) {
    if (!levels.length) return;
    e.stopPropagation();
    const i = levels.length - 1;
    const L = levels[i];
    const handled = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Enter", " ", "Escape", "Tab"].includes(e.key);
    if (handled) e.preventDefault();
    const up = () => (i > 0 ? ((levels = levels.slice(0, i)), focusSel(i - 1, levels[i - 1].sel)) : close());
    if (e.key === "Escape") return up();
    if (L.swatch) {
      const n = L.items.length;
      const at = Math.max(0, L.sel);
      if (e.key === "ArrowRight") return focusSel(i, Math.min(n - 1, L.sel + 1));
      if (e.key === "ArrowLeft") return at % COLS ? focusSel(i, at - 1) : up();
      if (e.key === "ArrowDown") return focusSel(i, Math.min(n - 1, L.sel < 0 ? 0 : at + COLS));
      if (e.key === "ArrowUp") return focusSel(i, at >= COLS ? at - COLS : at);
    } else {
      if (e.key === "ArrowDown") return step(i, 1);
      if (e.key === "ArrowUp") return step(i, -1);
      if (e.key === "ArrowLeft") return void (i > 0 && up());
      if (e.key === "ArrowRight") {
        const it = L.items[L.sel];
        return void (it && real(it) && Array.isArray(it[1]) && pick(i, L.sel, true));
      }
    }
    if ((e.key === "Enter" || e.key === " ") && L.sel >= 0) pick(i, L.sel, true);
  }

  // outside click, window blur, wheel or resize closes the menu (like a native one)
  const inside = (t: EventTarget | null) => els.some((el) => el?.contains(t as Node));
  const away = (e: Event) => levels.length && !inside(e.target) && close();
  const gone = () => levels.length && close();
</script>

<svelte:window onkeydowncapture={onKey} onpointerdowncapture={away} onwheelcapture={away} onblur={gone} onresize={gone} />

{#snippet icon(name: string)}
  <svg class="mic" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round">
    {#if name === "rename"}<path d="M10.5 2.5l3 3-8 8H2.5v-3zM9 4l3 3" />
    {:else if name === "color"}<path d="M8 1.5a6.5 6.5 0 1 0 0 13c1 0 1.5-.6 1.5-1.3 0-1-.9-1.2-.9-2s.6-1.2 1.4-1.2h1.7a2.8 2.8 0 0 0 2.8-2.8C14.5 4 11.6 1.5 8 1.5z" /><circle cx="5" cy="7" r=".8" /><circle cx="7.5" cy="4.5" r=".8" /><circle cx="10.5" cy="5" r=".8" />
    {:else if name === "close"}<path d="M4 4l8 8M12 4l-8 8" />
    {:else if name === "move"}<path d="M1.5 3.5h4l1.5 1.5h7.5v8h-13zM7 9h5M10 7l2 2-2 2" />
    {:else if name === "copy"}<path d="M5.5 5.5h8v8h-8zM10.5 5.5v-3h-8v8h3" />
    {:else if name === "folder"}<path d="M1.5 3.5h4l1.5 1.5h7.5v8h-13z" />
    {:else if name === "account"}<circle cx="8" cy="5" r="2.8" /><path d="M2.5 14c.6-3 2.8-4.5 5.5-4.5s4.9 1.5 5.5 4.5" />
    {:else if name === "collapse"}<path d="M4.5 7L8 3.5 11.5 7M4.5 12.5L8 9l3.5 3.5" />
    {:else if name === "delete"}<path d="M2.5 4h11M6 4V2.5h4V4M4 4l.7 9.5h6.6L12 4" />{/if}
  </svg>
{/snippet}

{#each levels as L, i (i)}
  <div class="cmenu" class:swatch={L.swatch} role="menu" tabindex="-1" aria-label={L.title}
    bind:this={els[i]} style:left="{L.x}px" style:top="{L.y}px" style:visibility={L.shown ? "visible" : "hidden"}
    oncontextmenu={(e) => e.preventDefault()}>
    {#if L.swatch}
      {#if L.title}<div class="swt">{L.title}</div>{/if}
      <div class="grid">
        {#each L.items as it, j}
          {#if real(it)}
            <button class="sw" class:none={it[3] === "none"} class:cur={!!it[4]?.on} class:on={j === L.sel} data-i={j}
              role="menuitemradio" aria-checked={!!it[4]?.on} aria-label={it[0]} title={it[0]} tabindex="-1"
              style:--c={HEX[it[3] ?? ""] ?? "transparent"} onpointerenter={() => hover(i, j)} onclick={() => pick(i, j)}></button>
          {/if}
        {/each}
      </div>
      {#if L.hint}<div class="swh">{L.hint}</div>{/if}
    {:else}
      {#each L.items as it, j}
        {#if !real(it)}
          <div class="sep" role="separator"></div>
        {:else}
          {@const sub = Array.isArray(it[1])}
          <button class="mi" class:on={j === L.sel} class:off={it[2] === false} class:danger={it[4]?.danger} data-i={j}
            role="menuitem" aria-disabled={it[2] === false} aria-haspopup={sub ? "menu" : undefined} aria-expanded={sub ? levels.length > i + 1 && L.sel === j : undefined}
            tabindex="-1" onpointerenter={() => hover(i, j)} onclick={() => pick(i, j)}>
            {#if it[4]?.icon}{@render icon(it[4].icon)}{/if}
            <span class="lbl">{it[0]}</span>
            {#if it[4]?.on}<span class="k" aria-label="dipilih">✓</span>{/if}
            {#if it[4]?.key}<span class="k">{it[4].key}</span>{/if}
            {#if sub}<span class="k arrow" aria-hidden="true">›</span>{/if}
          </button>
        {/if}
      {/each}
    {/if}
  </div>
{/each}

<style>
  /* Gembala.pen 08/13: dark card, thin border, full blue highlight, right-aligned shortcut */
  .cmenu {
    position: fixed;
    z-index: 60; /* above xterm, dialogs (8) and the drag ghost (50) */
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    min-width: 220px;
    max-width: 360px;
    max-height: calc(100vh - 8px);
    overflow-y: auto;
    padding: 4px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--card);
    color: var(--fg);
    box-shadow: 0 10px 30px rgb(0 0 0 / 0.45);
    font: 13px system-ui, -apple-system, sans-serif;
    outline: none;
    -webkit-user-select: none;
    user-select: none;
  }
  .mi {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 8px;
    border: 0;
    border-radius: 5px;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    outline: none;
  }
  .lbl {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .k {
    color: var(--muted);
    font-size: 12px;
  }
  .arrow {
    font-size: 14px;
    line-height: 1;
  }
  .mic {
    flex: none;
    width: 13px;
    height: 13px;
    color: var(--muted);
  }
  .mi.danger,
  .mi.danger .mic {
    color: var(--tx-red, #f87171);
  }
  .mi.on:not(.off) {
    background: var(--pri-bg, #2563eb);
    color: var(--pri-fg, #fff);
  }
  .mi.on:not(.off) :is(.k, .mic) {
    color: inherit;
  }
  .mi.off {
    opacity: 0.4;
  }
  .sep {
    height: 1px;
    margin: 4px 6px;
    background: var(--line);
  }
  .cmenu.swatch {
    min-width: 0;
    padding: 10px;
    gap: 8px;
  }
  .swt {
    color: var(--muted);
    font-size: 11px;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(5, 16px);
    gap: 6px;
  }
  .sw {
    width: 16px;
    height: 16px;
    padding: 0;
    border: 0;
    border-radius: 50%;
    background: var(--c);
    outline: none;
  }
  .sw.none {
    box-shadow: inset 0 0 0 1.5px var(--muted);
  }
  /* current color = ring; keyboard/hover target = brighter ring */
  .sw.cur {
    box-shadow: 0 0 0 2px var(--card), 0 0 0 3.5px var(--fg);
  }
  .sw.none.cur {
    box-shadow: inset 0 0 0 1.5px var(--muted), 0 0 0 2px var(--card), 0 0 0 3.5px var(--fg);
  }
  .sw.on:not(.cur) {
    box-shadow: 0 0 0 2px var(--card), 0 0 0 3.5px var(--pri-bg, #2563eb);
  }
  .sw.none.on:not(.cur) {
    box-shadow: inset 0 0 0 1.5px var(--muted), 0 0 0 2px var(--card), 0 0 0 3.5px var(--pri-bg, #2563eb);
  }
  .swh {
    max-width: 200px;
    color: var(--muted);
    font-size: 11px;
  }
</style>
