// Harness 3b builder (FR-24, FR-25). Pure: the daemon decides what is valid (`recipe.render`), these only move cards and
// chains around the way the screen allows. The recipe is the daemon's JSON (`recipe::to_json`), sent back as is.

export type BStep = {
  id: string; kind: "work" | "check"; name: string; need: string; instruction: string; agents: string[];
  model: string; models: Record<string, string>; effort: string; output: string; inputs: string[]; pass: string;
  timeout_secs: number; gate: boolean; human: boolean; shell: boolean; max_cost: number | null;
};
export type BRecipe = {
  name: string; fallback_on: string[]; token_saver: boolean; max_rounds: number; after_max: string; on_limit: string;
  total_secs: number; steps: BStep[];
};
export type Flags = { model: boolean; effort: boolean; cost: boolean };
export type LibAgent = { agent: string; installed: boolean; flags: Flags };
export type Diag = { code: string; args?: Record<string, string> };

/// SEC-30: a chain holds at most 3 agents
export const MAX_CHAIN = 3;

const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v));

/// a report-like work step is the only kind allowed after Ship (recipe.rs `ship_not_last`)
const afterShip = (s: BStep) => s.kind === "work" && !s.shell && !s.gate && !s.human;
export const shipAt = (steps: BStep[]) => steps.findIndex((s) => s.human);

/// where a dragged card may land: `to` is the index it ends up at. Ship never moves, and only a report-like step may sit after it.
export function canMove(steps: BStep[], from: number, to: number): boolean {
  if (from === to || from < 0 || to < 0 || from >= steps.length || to >= steps.length) return false;
  if (steps[from].human) return false;
  const moved = move(steps, from, to);
  const ship = shipAt(moved);
  return ship < 0 || moved.slice(ship + 1).every(afterShip);
}

function move(steps: BStep[], from: number, to: number): BStep[] {
  const out = steps.slice();
  const [s] = out.splice(from, 1);
  out.splice(to, 0, s);
  return out;
}

/// the steps after dragging card `from` to slot `to`; the same array when the screen does not allow it
export const moveStep = (steps: BStep[], from: number, to: number): BStep[] => (canMove(steps, from, to) ? move(steps, from, to) : steps);

/// which slot a pointer at `y` means, from the vertical middles of the cards (in screen order)
export function dropIndex(mids: number[], y: number): number {
  let i = 0;
  while (i < mids.length && y > mids[i]) i++;
  return i;
}

/// a card taken from the library goes in before Ship (before the report that follows it, so Ship stays after the work)
export function addStep(steps: BStep[], card: BStep): BStep[] {
  if (steps.some((s) => s.id === card.id)) return steps;
  const out = steps.slice();
  const ship = shipAt(out);
  out.splice(ship < 0 ? out.length : ship, 0, clone(card));
  return out;
}

export const removeStep = (steps: BStep[], id: string): BStep[] => steps.filter((s) => s.id !== id || s.human);

/// library cards already in the flow ("udah di alur")
export const used = (steps: BStep[], id: string) => steps.some((s) => s.id === id);

// ---- fallback chain (FR-25) ----

/// cannot add: Ship and test steps have no agent, a chain holds 3, the same agent never twice, only installed agents
export function canAddAgent(step: BStep, agent: string, lib: LibAgent[]): boolean {
  if (step.human || step.shell) return false;
  if (step.agents.length >= MAX_CHAIN || step.agents.includes(agent)) return false;
  return lib.some((a) => a.agent === agent && a.installed);
}

export function addAgent(step: BStep, agent: string, lib: LibAgent[]): BStep {
  return canAddAgent(step, agent, lib) ? { ...step, agents: [...step.agents, agent] } : step;
}

/// the card's `model` belongs to whoever is first (runner::step_flags), so changing the chain re-files every agent's model
function rechain(step: BStep, agents: string[]): BStep {
  const per = Object.fromEntries(step.agents.map((a) => [a, modelOf(step, a)]));
  const models: Record<string, string> = {};
  for (const a of agents.slice(1)) {
    const m = per[a] ?? step.models[a] ?? "";
    if (m) models[a] = m;
  }
  return { ...step, agents, model: per[agents[0]] ?? step.models[agents[0]] ?? "", models };
}

/// the primary agent picked from the card: swaps with the chain entry when it is already there, else replaces the first
export function setPrimary(step: BStep, agent: string): BStep {
  if (step.agents[0] === agent) return step;
  const j = step.agents.indexOf(agent);
  return j > 0 ? moveAgent(step, j, 0) : rechain(step, [agent, ...step.agents.slice(1)]);
}

/// the last agent cannot go: a work step without one is an error the user would only meet in review
export function removeAgent(step: BStep, agent: string): BStep {
  if (step.agents.length < 2 || !step.agents.includes(agent)) return step;
  return rechain(step, step.agents.filter((a) => a !== agent));
}

export function moveAgent(step: BStep, from: number, to: number): BStep {
  if (from === to || from < 0 || to < 0 || from >= step.agents.length || to >= step.agents.length) return step;
  const agents = step.agents.slice();
  const [a] = agents.splice(from, 1);
  agents.splice(to, 0, a);
  return rechain(step, agents);
}

/// model of one agent row. Row 1 (primary) edits the card's `model` (the recipe's one effort stays per step); the others `models[agent]`.
/// The card model only applies to the first agent (runner::step_flags), so a primary swap must carry it along.
export function setModel(step: BStep, agent: string, value: string): BStep {
  const v = value.trim();
  if (step.agents[0] === agent) return { ...step, model: v };
  const models = { ...step.models };
  if (v) models[agent] = v;
  else delete models[agent];
  return { ...step, models };
}

/// what a row shows as its model: "card" for the primary's own, the override or "" (agent default)
export const modelOf = (step: BStep, agent: string) => (step.agents[0] === agent ? step.model : (step.models[agent] ?? ""));

/// check steps: agents that also sit in the Execute chain, skipped when they did the work (BRD 8 rule 4)
export function skipped(steps: BStep[], i: number): string[] {
  const s = steps[i];
  const exec = steps.find((x) => x.id === "execute");
  if (!s || s.kind !== "check" || !exec) return [];
  return s.agents.filter((a) => exec.agents.includes(a));
}

/// the flow shows the agent chain as a one-line summary ("Claude Code › Gemini"), names from `name`
export const chainText = (agents: string[], name: (a: string) => string) => agents.map(name).join(" › ");

// ---- diagnostics from recipe.render: `at` = "step.<id>", "step.<id>.inputs", "step" (the recipe as a whole) ----

const stepOf = (d: Diag) => /^step\.([^.]+)/.exec(d.args?.at ?? "")?.[1];
export const diagsFor = (diags: Diag[], id: string) => diags.filter((d) => stepOf(d) === id);
export const diagsRecipe = (diags: Diag[]) => diags.filter((d) => !stepOf(d));

/// the builder's recipe is unchanged from what it loaded
export const same = (a: BRecipe, b: BRecipe) => JSON.stringify(a) === JSON.stringify(b);

/// a new recipe's id from a name: lowercase, letters/digits/-, free among `taken`
export function slugId(name: string, taken: string[]): string {
  const base = name.toLowerCase().normalize("NFKD").replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40) || "resep";
  if (!taken.includes(base)) return base;
  for (let n = 2; ; n++) if (!taken.includes(`${base}-${n}`)) return `${base}-${n}`;
}

// ---- custom stage (B4): the form of "Tahap kustom", filled by the formulating agent or by hand ----
export type StepForm = { name: string; kind: "work" | "check"; instruction: string; output: string; pass: string };
export const PASSES = ["reply", "file", "first_line_pass", "exit0"];
export const NAME_MAX = 24;
const FILE_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

/// the form fields that would be refused (same rules as `recipe::check_step_draft`; the daemon still checks on save)
export function formErrors(f: StepForm): string[] {
  const e: string[] = [];
  if (!f.name.trim() || [...f.name.trim()].length > NAME_MAX) e.push("name");
  if (!f.instruction.trim()) e.push("instruction");
  if (f.output && !FILE_RE.test(f.output)) e.push("output");
  if (!PASSES.includes(f.pass)) e.push("pass");
  else if (!f.output && (f.pass === "file" || f.pass === "first_line_pass")) e.push("pass");
  return e;
}

/// the card for the flow: defaults of "Kustom" (the formulating agent, its default model, medium effort, 15 minutes); `need` is kept
export function customStep(f: StepForm, need: string, agent: string, ids: string[]): BStep {
  return {
    id: slugId(`tahap ${f.name}`, ids), kind: f.kind, name: f.name.trim(), need: need.trim(), instruction: f.instruction.trim(), agents: [agent],
    model: "", models: {}, effort: "medium", output: f.output, inputs: [], pass: f.pass, timeout_secs: 900, gate: false, human: false, shell: false, max_cost: null,
  };
}

// ---- recipe tabs (the builder as a sidebar tab, session only: never in layout.json) ----
export type RTab = { key: string; ws: string; rid: string; tpl: string; name: string; st: string };

/// one recipe = one tab: an existing recipe id is focused if it is already open; a new one (from a template) is always a new tab
export function openRecipe(tabs: RTab[], ws: string, rid: string, tpl: string, key: string): { tabs: RTab[]; key: string } {
  const have = rid ? tabs.find((x) => x.rid === rid) : undefined;
  return have ? { tabs, key: have.key } : { tabs: [...tabs, { key, ws, rid, tpl, name: "", st: tpl ? "unsaved" : "" }], key };
}

/// builder -> sidebar: name, saved id and state of a tab (a new recipe gets its id once it is saved, so opening it again finds the tab)
export const patchRecipe = (tabs: RTab[], key: string, p: Partial<RTab>): RTab[] =>
  tabs.map((x) => (x.key === key && Object.entries(p).some(([k, v]) => x[k as keyof RTab] !== v) ? { ...x, ...p } : x));

// ---- FR-99 P3: the recipe in sentences, one per stage ("Tinjau resep"), for people who cannot judge a TOML file ----
export type Tr = (key: string, p?: Record<string, string | number>) => string;
export type Sentence = { id: string; title: string; kind: "work" | "check" | "gate"; sentence: string; note: string };

/// "maks 15 menit" / "maks 2 jam"; no limit = ""
const spell = (secs: number, tr: Tr) => (secs % 3600 === 0 ? tr("hn.set.hours", { n: secs / 3600 }) : tr("hn.set.mins", { n: Math.max(1, Math.round(secs / 60)) }));
const limitOf = (s: BStep, tr: Tr) => (s.timeout_secs > 0 ? tr("hn.sum.limit", { t: spell(s.timeout_secs, tr) }) : "");

/// one sentence per stage; a stage id it does not know keeps the recipe's own name and a general sentence. Pure: the words
/// come from `tr` (the i18n keys `hn.sum.*`), the agent names from `name`.
export function describeRecipe(r: BRecipe, name: (agent: string) => string, tr: Tr): Sentence[] {
  const exec = r.steps.find((s) => s.id === "execute");
  return r.steps.map((s) => {
    const title = s.name || s.id;
    // a check never runs on the agent that did the work (BRD 8 rule 4), so that one is not in the chain the sentence tells
    const chain = s.kind === "check" && s.agents.some((a) => !exec?.agents.includes(a)) ? s.agents.filter((a) => !exec?.agents.includes(a)) : s.agents;
    const who = name(chain[0] ?? "");
    const lim = limitOf(s, tr);
    const notes: string[] = [];
    let sentence: string;
    let kind: Sentence["kind"] = s.kind;
    if (s.human) {
      kind = "gate";
      sentence = tr("hn.sum.ship");
      notes.push(tr("hn.sum.shipNote"));
    } else if (s.shell) {
      sentence = tr("hn.sum.shell");
    } else {
      sentence = s.kind === "check" ? tr("hn.sum.check", { who, limit: lim }) : tr(s.id === "plan" ? "hn.sum.plan" : s.id === "execute" ? "hn.sum.execute" : "hn.sum.other", { who, limit: lim });
      if (s.gate) notes.push(tr(s.id === "plan" ? "hn.sum.gatePlan" : "hn.sum.gate"));
      if (chain.length > 1) notes.push(tr("hn.sum.fallback", { first: name(chain[0]), rest: chain.slice(1).map(name).join(" › ") }));
    }
    if (s.kind === "check" && exec && r.max_rounds > 0) notes.push(tr("hn.sum.fix", { who: name(exec.agents[0] ?? ""), n: r.max_rounds }));
    return { id: s.id, title, kind, sentence: sentence.replace(/\s+([.,])/g, "$1"), note: notes.join(" ") };
  });
}

/// the line under the stages: fix rounds, total time, token saver
export const describeLimits = (r: BRecipe, tr: Tr): string =>
  [r.max_rounds > 0 ? tr("hn.sum.rounds", { n: r.max_rounds }) : "", r.total_secs > 0 ? tr("hn.sum.total", { t: spell(r.total_secs, tr) }) : "", r.token_saver ? tr("hn.sum.saver") : ""].filter(Boolean).join(" · ");
