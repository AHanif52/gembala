// FR-87 (TECH 27): the one list of notification kinds. Banner, bell, "Butuh lu" strip and Telegram use the same words.
// Pure functions only: `plan` takes the clock as an argument so the replace/repeat/merge/withdraw rules are tested without the app.

export type NoticeKind = "permission" | "answer" | "password" | "limit" | "approve" | "failed" | "done";

export type NoticeNotify = Partial<Record<"on_blocked" | "on_done" | "on_permission" | "on_answer" | "on_password" | "on_limit" | "on_approve" | "on_failed" | "sound" | "sound_done", boolean>>;

type Spec = {
  bounce: boolean; // Dock jumps (needs you now)
  loud: boolean; // plays the sound when [notify].sound is on; done is the exception (`sound_done`)
  waits: boolean; // the pane stays blocked until answered: the banner is withdrawn once it isn't
  act: "answer" | "open"; // the action line of the banner
};

// urgency (BRD FR-87 6): permission/answer/password = sound + bounce; limit/failed (and approve) = sound + badge; done = badge only
export const KINDS: Record<NoticeKind, Spec> = {
  permission: { bounce: true, loud: true, waits: true, act: "answer" },
  answer: { bounce: true, loud: true, waits: true, act: "answer" },
  password: { bounce: true, loud: true, waits: true, act: "answer" },
  approve: { bounce: false, loud: true, waits: true, act: "open" },
  limit: { bounce: false, loud: true, waits: false, act: "open" },
  failed: { bounce: false, loud: true, waits: false, act: "open" },
  done: { bounce: false, loud: false, waits: false, act: "open" },
};

/// i18n key of the word for a kind (`notice.permission` …)
export const wordKey = (k: NoticeKind) => `notice.${k}` as const;
export const actKey = (k: NoticeKind) => `notice.act.${KINDS[k].act}` as const;

/// `blocked` status → kind: only "waiting for an answer" (FR-84) is its own kind; every other reason is a permission-type ask
export const blockedKind = (reasonKey: string | undefined): NoticeKind => (reasonKey === "reason.input" ? "answer" : "permission");

/// [notify] switch: permission/answer/password sit under `on_blocked` (older configs only had that one); the rest have their own.
/// A missing key (older daemon) counts as on.
export function enabled(k: NoticeKind, n: NoticeNotify): boolean {
  if (k === "done") return n.on_done !== false;
  const own = n[`on_${k}` as keyof NoticeNotify] !== false;
  return k === "permission" || k === "answer" || k === "password" ? n.on_blocked !== false && own : own;
}

export const soundOn = (k: NoticeKind, n: NoticeNotify) => (KINDS[k].loud ? n.sound !== false : n.sound_done === true);

/// where a banner says it happened: `Workspace › Tab`; the tab only when a person named it (never the task label, FR-69/SEC-33)
export const where = (ws: string, tab: string) => [ws, tab].filter(Boolean).join(" › ");

export type Banner = { title: string; body: string; pane: number; sound: boolean; bounce: boolean };
export type Plan = { show?: Banner; remove: string[] };
type Shown = { kind: NoticeKind; at: number; title: string };
export type NoticeState = { by: Map<number, Shown>; batch: { panes: number[]; at: number } | null };
export const newState = (): NoticeState => ({ by: new Map(), batch: null });

export const REPEAT_MS = 60_000; // the same kind on the same pane inside this window doesn't ring again
export const MERGE_MS = 5_000; // done on several panes inside this window = one banner

export type Event = {
  pane: number;
  kind: NoticeKind;
  now: number;
  title: string; // "name · agent"
  body: string; // "event · Workspace › Tab\naction"
  sound: boolean;
  merged: (n: number) => { title: string; body: string }; // "3 panes done"; body of the newest pane's workspace
};

/// What to do for one event: nothing, a banner (with the titles it replaces to withdraw), or a merged "N done" banner.
export function plan(st: NoticeState, e: Event): Plan {
  const prev = st.by.get(e.pane);
  if (prev && prev.kind === e.kind && e.now - prev.at < REPEAT_MS) return { remove: [] };
  const remove: string[] = prev ? [prev.title] : [];
  const spec = KINDS[e.kind];
  if (e.kind === "done" && st.batch && e.now - st.batch.at < MERGE_MS) {
    // a later done joins the batch: earlier single banners and the previous merged one are replaced by one count
    const panes = [...st.batch.panes.filter((p) => p !== e.pane), e.pane];
    const m = e.merged(panes.length);
    for (const p of panes) {
      const t = st.by.get(p)?.title;
      if (t && !remove.includes(t)) remove.push(t);
    }
    st.batch = { panes, at: e.now };
    for (const p of panes) st.by.set(p, { kind: "done", at: p === e.pane ? e.now : (st.by.get(p)?.at ?? e.now), title: m.title });
    return { remove, show: { ...m, pane: e.pane, sound: e.sound, bounce: false } };
  }
  if (e.kind === "done") st.batch = { panes: [e.pane], at: e.now };
  st.by.set(e.pane, { kind: e.kind, at: e.now, title: e.title });
  return { remove, show: { title: e.title, body: e.body, pane: e.pane, sound: e.sound, bounce: spec.bounce } };
}

/// The pane stopped waiting (answered, closed, or the card is gone): titles to withdraw, and the next event of that kind rings again.
export function settle(st: NoticeState, pane: number): string[] {
  const prev = st.by.get(pane);
  if (!prev || !KINDS[prev.kind].waits) return [];
  st.by.delete(pane);
  return [prev.title];
}

/// The window is being looked at: every banner of the app goes, and the next events start fresh.
export function clear(st: NoticeState): string[] {
  const titles = [...new Set([...st.by.values()].map((s) => s.title))];
  st.by.clear();
  st.batch = null;
  return titles;
}
