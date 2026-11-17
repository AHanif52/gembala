export type NestedTab = { id: string; parent?: string; helpersOpen?: boolean; restOpen?: boolean; grid?: string[]; gridRows?: boolean; gridRatio?: { x: number; y: number } };
export type SidebarRow<T> = { kind: "tab" | "summary" | "rest"; tab: T; count?: number; child?: boolean; rest?: T[]; open?: boolean };
export type HelperDot = "blocked" | "working" | "done" | "idle";

// FR-101 varian B (TECH 37): roots keep their tab order; a helper always sits below its owner. One helper = a thin row;
// two or more = one `summary` row that opens into thin rows, idle and seen-done helpers folding into one text `rest` row.
// `helpersOpen` has three states: true/false = the user's choice, undefined = automatic (open while something needs a look).
export function subtabRows<T extends NestedTab>(tabs: T[], quiet: (tab: T) => boolean, blocked: (tab: T) => boolean, selected?: string, unseen: (tab: T) => boolean = () => false): SidebarRow<T>[] {
  const rows: SidebarRow<T>[] = [];
  for (const tab of tabs.filter((t) => !t.parent)) {
    rows.push({ kind: "tab", tab });
    const children = tabs.filter((t) => t.parent === tab.id);
    if (!children.length) continue;
    if (children.length === 1) {
      rows.push({ kind: "tab", tab: children[0], child: true });
      continue;
    }
    rows.push({ kind: "summary", tab, count: children.length });
    const auto = children.some((t) => blocked(t) || unseen(t) || t.id === selected);
    if (!(tab.helpersOpen ?? auto)) continue;
    const folded = (t: T) => quiet(t) && t.id !== selected;
    const show = (t: T) => !folded(t) || !!tab.restOpen; // `restOpen`: the user clicked the rest row, quiet helpers get their rows too
    rows.push(...children.filter(show).map((t) => ({ kind: "tab" as const, tab: t, child: true })));
    const rest = children.filter(tab.restOpen ? quiet : folded); // open: the fold-back button stays even when only the selected one is quiet
    if (rest.length) rows.push({ kind: "rest", tab, count: rest.length, rest, open: !!tab.restOpen });
  }
  return rows;
}

/// Is the summary row open right now? Same rule as `subtabRows`, for the chevron.
export const helpersShown = <T extends NestedTab>(owner: T, children: T[], blocked: (t: T) => boolean, unseen: (t: T) => boolean, selected?: string) =>
  owner.helpersOpen ?? children.some((t) => blocked(t) || unseen(t) || t.id === selected);

/// Icons on the summary row: blocked first, then working, done not yet seen, the rest; max 4, `more` = the overflow count.
export function helperDots<T extends { id: string }>(children: T[], dot: (t: T) => HelperDot): { shown: { tab: T; dot: HelperDot }[]; more: number } {
  const rank: Record<HelperDot, number> = { blocked: 0, working: 1, done: 2, idle: 3 };
  const sorted = children.map((tab) => ({ tab, dot: dot(tab) })).sort((a, b) => rank[a.dot] - rank[b.dot]);
  return { shown: sorted.slice(0, 4), more: Math.max(0, sorted.length - 4) };
}

export const GRID_MAX = 4;

/// FR-101 varian B: the saved grid of an owner tab, minus ids that are gone, not its helpers, or duplicated; the owner is
/// always in it, at most 4 ids; fewer than 2 = no grid (`undefined`).
export function cleanGrid(owner: NestedTab, tabs: NestedTab[]): string[] | undefined {
  if (!Array.isArray(owner.grid)) return undefined;
  const ok = (id: unknown) => typeof id === "string" && (id === owner.id || tabs.some((t) => t.id === id && t.parent === owner.id));
  const ids = [...new Set(owner.grid.filter(ok))];
  if (!ids.includes(owner.id)) ids.unshift(owner.id);
  const out = ids.filter((id) => id !== owner.id).slice(0, GRID_MAX - 1);
  out.splice(Math.min(ids.indexOf(owner.id), GRID_MAX - 1), 0, owner.id);
  return out.length < 2 ? undefined : out;
}

export type GridRect = { x: number; y: number; w: number; h: number };
export type GridLine = { axis: "x" | "y"; x: number; y: number; len: number }; // fractions of the grid box
const clampRatio = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? Math.min(0.8, Math.max(0.2, v)) : 0.5);
export const gridRatio = (r?: { x?: number; y?: number }) => ({ x: clampRatio(r?.x), y: clampRatio(r?.y) });

/// One rect per slot. 2 = side by side (or stacked when `rows`); 3 = slot 0 takes the full first side, the other two share
/// the second; 4 = 2x2. `x` is the vertical divider, `y` the horizontal one; with 3 panes the main divider is `x` in
/// columns and `y` in rows, the other axis splits the two small panes.
export function gridRects(n: number, rows = false, ratio?: { x?: number; y?: number }): GridRect[] {
  const { x, y } = gridRatio(ratio);
  if (n <= 1) return [{ x: 0, y: 0, w: 1, h: 1 }];
  if (n === 2) return rows ? [{ x: 0, y: 0, w: 1, h: y }, { x: 0, y, w: 1, h: 1 - y }] : [{ x: 0, y: 0, w: x, h: 1 }, { x, y: 0, w: 1 - x, h: 1 }];
  if (n === 3)
    return rows
      ? [{ x: 0, y: 0, w: 1, h: y }, { x: 0, y, w: x, h: 1 - y }, { x, y, w: 1 - x, h: 1 - y }]
      : [{ x: 0, y: 0, w: x, h: 1 }, { x, y: 0, w: 1 - x, h: y }, { x, y, w: 1 - x, h: 1 - y }];
  return [{ x: 0, y: 0, w: x, h: y }, { x, y: 0, w: 1 - x, h: y }, { x: 0, y, w: x, h: 1 - y }, { x, y, w: 1 - x, h: 1 - y }];
}

/// Draggable dividers of the same layout (the secondary divider of 3 panes only spans the second side).
export function gridLines(n: number, rows = false, ratio?: { x?: number; y?: number }): GridLine[] {
  const { x, y } = gridRatio(ratio);
  if (n <= 1) return [];
  if (n === 2) return [rows ? { axis: "y", x: 0, y, len: 1 } : { axis: "x", x, y: 0, len: 1 }];
  if (n === 3)
    return rows
      ? [{ axis: "y", x: 0, y, len: 1 }, { axis: "x", x, y, len: 1 - y }]
      : [{ axis: "x", x, y: 0, len: 1 }, { axis: "y", x, y, len: 1 - x }];
  return [{ axis: "x", x, y: 0, len: 1 }, { axis: "y", x: 0, y, len: 1 }];
}

/// Drop result of dragging helper `id` into slot `at` (0-based); null = refused (full, or not this owner's helper).
export function gridInsert(owner: NestedTab, tabs: NestedTab[], id: string, at: number): string[] | null {
  if (id !== owner.id && !tabs.some((t) => t.id === id && t.parent === owner.id)) return null;
  const cur = cleanGrid(owner, tabs) ?? [owner.id];
  if (cur.includes(id)) return cur;
  if (cur.length >= GRID_MAX) return null;
  const next = [...cur];
  next.splice(Math.min(Math.max(at, 0), next.length), 0, id);
  return next;
}

/// Drag a slot onto another: swap (kind "swap") or move to that position (chip strip reorder).
export function gridMove(grid: string[], from: number, to: number, swap: boolean): string[] {
  if (from === to || from < 0 || to < 0 || from >= grid.length || to >= grid.length) return grid;
  const next = [...grid];
  if (swap) [next[from], next[to]] = [next[to], next[from]];
  else next.splice(to, 0, next.splice(from, 1)[0]);
  return next;
}

/// FR-101 item 1 (TECH 29, Orca pattern): a tab split by hand lists its agents as rows under the title. Shells running
/// something (a command, a password prompt) keep a row too; idle shells fold into "+N shell". `null` = the tab stays one
/// row as before (a single pane, or only shells).
export function splitRows(panes: number[], agent: (p: number) => boolean, busy: (p: number) => boolean): { rows: number[]; folded: number[] } | null {
  if (panes.length < 2 || !panes.some(agent)) return null;
  const rows = panes.filter((p) => agent(p) || busy(p));
  return { rows, folded: panes.filter((p) => !rows.includes(p)) };
}
