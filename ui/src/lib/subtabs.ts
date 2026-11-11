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
