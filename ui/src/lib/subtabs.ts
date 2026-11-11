export type NestedTab = { id: string; parent?: string; helpersOpen?: boolean; doneOpen?: boolean };
export type SidebarRow<T> = { kind: "tab" | "summary" | "fold"; tab: T; count?: number; child?: boolean };

// FR-101: roots keep their tab order; a helper always sits below its owner.
export function subtabRows<T extends NestedTab>(tabs: T[], quiet: (tab: T) => boolean, blocked: (tab: T) => boolean, selected?: string): SidebarRow<T>[] {
  const rows: SidebarRow<T>[] = [];
  for (const tab of tabs.filter((t) => !t.parent)) {
    rows.push({ kind: "tab", tab });
    const children = tabs.filter((t) => t.parent === tab.id);
    if (!children.length) continue;
    rows.push({ kind: "summary", tab, count: children.length });
    if (!tab.helpersOpen && !children.some(blocked) && !children.some((t) => t.id === selected)) continue;
    const done = children.filter(quiet);
    rows.push(...children.filter((t) => !quiet(t)).map((t) => ({ kind: "tab" as const, tab: t, child: true })));
    if (done.length) {
      rows.push({ kind: "fold", tab, count: done.length });
      rows.push(...done.filter((t) => tab.doneOpen || t.id === selected).map((t) => ({ kind: "tab" as const, tab: t, child: true })));
    }
  }
  return rows;
}

/// FR-101 item 1 (TECH 29, Orca pattern): a tab split by hand lists its agents as rows under the title. Shells running
/// something (a command, a password prompt) keep a row too; idle shells fold into "+N shell". `null` = the tab stays one
/// row as before (a single pane, or only shells).
export function splitRows(panes: number[], agent: (p: number) => boolean, busy: (p: number) => boolean): { rows: number[]; folded: number[] } | null {
  if (panes.length < 2 || !panes.some(agent)) return null;
  const rows = panes.filter((p) => agent(p) || busy(p));
  return { rows, folded: panes.filter((p) => !rows.includes(p)) };
}
