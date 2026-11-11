// FR-99 P2 ("Butuh lu" strip and the stacked cards): the pure part. What waits for a person, in which order, and which
// card sits in front. Text is a key + params; the page renders it through i18n. Words of a kind come from `notice.ts`
// (FR-87), so the strip, the bar, the bell, the banner and Telegram keep saying the same thing.

import { type NoticeKind, wordKey } from "./notice.ts";

/// one shape per thing that waits; `permission`/`answer`/`password`/`trust` are a blocked pane, `message`/`open` are the
/// message engine's confirm cards, `gate`/`ship` are a harness run stopped at a step that needs the user.
export type NeedKind = "permission" | "answer" | "password" | "trust" | "message" | "open" | "gate" | "ship";

/// why the order is what it is: group 1 stops the agent completely, 2 holds the next step, 3 holds a run
export type NeedGroup = 1 | 2 | 3;

export const GROUP: Record<NeedKind, NeedGroup> = {
  permission: 1,
  answer: 1,
  password: 1,
  trust: 1,
  message: 2,
  open: 2,
  gate: 3,
  ship: 3,
};

/// the kind's word (FR-87): a trust prompt is still "needs permission", and the message cards + harness gates all read
/// "waiting for your OK"
const WORD_OF: Record<NeedKind, NoticeKind> = {
  permission: "permission",
  answer: "answer",
  password: "password",
  trust: "permission",
  message: "approve",
  open: "approve",
  gate: "approve",
  ship: "approve",
};
export const word = (k: NeedKind): string => wordKey(WORD_OF[k]);

export type NeedRow = {
  key: string;
  kind: NeedKind;
  group: NeedGroup;
  at: number; // ms; the longest wait goes first inside a group
  pane: number | null; // where a click lands; null = the harness flow of `ws`
  ws: number;
  /// i18n key of the one sentence under the name, with its params
  what: string;
  params: Record<string, string>;
  /// label of the hover button: jawab / buka / lihat / baca rencana
  act: "answer" | "open" | "see" | "plan";
};

/// a blocked pane; `ask`/`question` come from the screen (`pane.dialogs`), empty when the dialog wasn't read
export type PaneNeed = { pane: number; kind: "permission" | "answer" | "password" | "trust"; at: number; ask?: string; question?: string };
/// a card held by the message engine (FR-12 confirm, FR-75 open-and-send); `pane` = where its card sits, when that pane lives
export type MsgNeed = { id: string; at: number; from: string; to: string; pane: number | null };
/// FR-75: an agent asking for a helper that isn't open; `to` = the agent it wants
export type OpenNeed = { id: string; at: number; from: string; to: string; pane: number | null };
export type GateNeed = { run: string; ws: number; at: number; ship: boolean; name: string };
export type NeedInput = { panes: PaneNeed[]; messages: MsgNeed[]; opens: OpenNeed[]; gates: GateNeed[]; simple: boolean };

const clean = (s: string, n = 90) => {
  const one = s.split("\n").map((x) => x.trim()).filter(Boolean).join(" · ");
  return one.length > n ? `${one.slice(0, n - 1)}…` : one;
};

/// rows in the order the strip shows them: group 1 first (the agent stopped), then 2, then 3; oldest first inside a group
export function needRows(i: NeedInput): NeedRow[] {
  const rows: NeedRow[] = [];
  for (const p of i.panes) {
    const ask = clean(p.ask ?? "");
    const q = clean(p.question ?? "");
    const what =
      p.kind === "password"
        ? "need.password"
        : p.kind === "trust"
          ? "need.trust"
          : p.kind === "answer"
            ? q
              ? "need.answer"
              : "need.answer.none"
            : ask
              ? "need.permission"
              : "need.permission.none";
    rows.push({
      key: `${p.kind}:${p.pane}`,
      kind: p.kind,
      group: GROUP[p.kind],
      at: p.at,
      pane: p.pane,
      ws: 0,
      what,
      params: p.kind === "answer" && q ? { q } : ask ? { what: ask } : {},
      // the bar can only answer a dialog it read; password and trust are answered in the pane (never typed by Gembala)
      act: p.kind === "permission" && ask ? "answer" : "open",
    });
  }
  for (const m of i.messages)
    rows.push({
      key: `message:${m.id}`,
      kind: "message",
      group: GROUP.message,
      at: m.at,
      pane: m.pane,
      ws: 0,
      // Sederhana: the sender and what it wants (leaner); Lengkap: who → who, the fact only
      what: i.simple ? "need.message" : "need.messageFull",
      params: { from: m.from, to: m.to },
      act: "see",
    });
  for (const o of i.opens)
    rows.push({
      key: `open:${o.id}`,
      kind: "open",
      group: GROUP.open,
      at: o.at,
      pane: o.pane,
      ws: 0,
      what: i.simple ? "need.open" : "need.openFull",
      params: { from: o.from, agent: o.to },
      act: "see",
    });
  for (const g of i.gates)
    rows.push({
      key: `${g.ship ? "ship" : "gate"}:${g.run}`,
      kind: g.ship ? "ship" : "gate",
      group: GROUP[g.ship ? "ship" : "gate"],
      at: g.at,
      pane: null,
      ws: g.ws,
      what: g.ship ? "need.ship" : "need.gate",
      params: { name: g.name },
      act: "plan",
    });
  return rows.sort((a, b) => a.group - b.group || a.at - b.at);
}

/// panes the shortcut walks through, in strip order, once each (a card that lives in a pane is that pane's turn)
export function needPanes(rows: NeedRow[]): number[] {
  const out: number[] = [];
  for (const r of rows) if (r.pane != null && !out.includes(r.pane)) out.push(r.pane);
  return out;
}

/// several cards in one pane: the front one is whole, the rest fold to a line. Front = the one the user picked ("Lihat"),
/// else the longest waiting; everything stays oldest first.
export function frontCard<T extends { id: string; at: number }>(cards: T[], pick?: string | null): { front: T | null; rest: T[] } {
  if (!cards.length) return { front: null, rest: [] };
  const sorted = [...cards].sort((a, b) => a.at - b.at);
  const front = (pick && sorted.find((c) => c.id === pick)) || sorted[0];
  return { front, rest: sorted.filter((c) => c !== front) };
}
