//! FR-16, FR-17, FR-25, FR-26; SEC-30: the harness engine (TECH 12 and 15, plan H2). Pure: `Run::decide(now, event)`
//! returns the actions to perform and does no I/O, so every rule of BRD 8 can be tested without a PTY, a clock, or a
//! daemon. The H3 runner feeds it events from the daemon (pane status, exit, timer, verdicts of the pass checks) and
//! carries out the actions (open a pane, send through the message engine, run the test command, read an artifact).

use std::collections::HashMap;

use crate::recipe::{MAX_AGENTS, MAX_ROUNDS, MAX_TOTAL_SECS, Recipe};

/// BRD 8 rule 2: a pane that stays `unknown` this long triggers the fallback
pub const UNKNOWN_SECS: u64 = 120;
/// the "Perbaiki" turn is not a recipe step, so its limit lives here (BRD 8 defaults)
pub const FIX_TIMEOUT_SECS: u64 = 20 * 60;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    Working,
    Idle,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Start,
    /// status of the current stage's pane
    Status(Status),
    /// the stage's agent process ended, or (shell step) the test command finished with this code
    Exit(i32),
    /// result of `Action::Evaluate`: did the stage meet its pass condition
    Verdict(bool),
    /// the stage message reached the pane: the agent's turn starts here, not at the launch (B2: a folder-trust dialog
    /// can hold the message back)
    Delivered,
    /// timer; the runner sends it about once a second
    Tick,
    /// a hook reported a usage limit for the stage's agent (fed once the per-agent patterns are proven, TECH 12)
    #[allow(dead_code)]
    Limit,
    /// the agent stopped on its own cost cap (omp `--max-cost`): never moves the cost to another agent
    #[allow(dead_code)]
    CostLimit,
    /// the user approved the gate (UI channel only, SEC-20)
    GateOk,
    Stop,
    Takeover,
    Release,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Halt {
    Stopped,
    /// fix rounds used up and the recipe says `stop`
    MaxRounds,
    Total,
    Cost,
    /// no usable agent for the stage at all
    NoAgent(usize),
    /// every agent of a check stage is the one that did the work (BRD 8 rule 4)
    NoChecker(usize),
    /// the fallback chain ran out
    Exhausted(usize),
    /// a trigger that the recipe does not list in `fallback_on`
    Failed { step: usize, why: &'static str },
    /// a work stage ended without meeting its pass condition
    StepFailed(usize),
    Timeout(usize),
    FixFailed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Idle,
    Running,
    /// waiting for the user (a stage with `gate`, or Ship)
    Gate(usize),
    TakenOver,
    Done,
    Halted(Halt),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Gate(usize),
    Blocked(usize),
    Done,
    Halted,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// open the stage's pane with this agent and send the stage message (same message after a fallback)
    Send { step: usize, agent: String },
    /// ask the work agent to fix what check step `failed` found
    SendFix { step: usize, agent: String, failed: usize },
    /// run the workspace test command in a shell, no AI
    RunShell { step: usize },
    /// check the stage's pass condition and answer with `Event::Verdict`
    Evaluate { step: usize },
    /// release every driver lock (FR-17); panes stay open for reading
    Unlock,
    Notify(Notice),
}

/// one line of the event trail (FR-26, SEC-11): the runner writes these to the audit log
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub at: u64,
    pub code: &'static str,
    pub step: Option<usize>,
    pub agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Task {
    Work { step: usize, agent: String },
    Fix { exec: usize, agent: String },
    Shell { step: usize },
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    /// `seen`: the pane was seen working since the message went out, so a later `idle` means the turn ended
    Working { seen: bool },
    Blocked,
    Evaluating,
}

pub struct Run {
    pub recipe: Recipe,
    pub state: State,
    pub trace: Vec<Entry>,
    pub round: u32,
    task: Option<Task>,
    phase: Phase,
    held: Option<Phase>,
    now: u64,
    /// start of the current active interval; None while blocked, at a gate, or taken over
    since: Option<u64>,
    step_spent: u64,
    total_spent: u64,
    unknown_since: Option<u64>,
    /// agents that failed per step, never tried again in this run
    failed: HashMap<usize, Vec<String>>,
    /// agent of the latest work stage, and that stage: what "Execute" means for rules 4 and 5
    worker: Option<String>,
    exec: Option<usize>,
}

impl Run {
    /// `installed` = agent kinds found on this Mac. SEC-30 is enforced again here, not trusted from the recipe.
    pub fn new(mut recipe: Recipe, installed: &[String]) -> Run {
        recipe.max_rounds = recipe.max_rounds.min(MAX_ROUNDS as u32);
        recipe.total_secs = recipe.total_secs.min(MAX_TOTAL_SECS);
        for s in &mut recipe.steps {
            // the model was written for the recipe's first agent: when that one isn't installed, no agent gets it
            if s.agents.first().is_some_and(|a| !installed.contains(a)) {
                s.model.clear();
            }
            s.agents.retain(|a| installed.contains(a));
            s.agents.truncate(MAX_AGENTS);
        }
        Run {
            recipe,
            state: State::Idle,
            trace: Vec::new(),
            round: 0,
            task: None,
            phase: Phase::Working { seen: false },
            held: None,
            now: 0,
            since: None,
            step_spent: 0,
            total_spent: 0,
            unknown_since: None,
            failed: HashMap::new(),
            worker: None,
            exec: None,
        }
    }

    /// stage the UI should show as current, if any
    pub fn current(&self) -> Option<usize> {
        match (&self.state, &self.task) {
            (State::Gate(i), _) => Some(*i),
            (_, Some(Task::Work { step, .. } | Task::Shell { step })) => Some(*step),
            (_, Some(Task::Fix { exec, .. })) => Some(*exec),
            _ => None,
        }
    }

    fn live(&self) -> bool {
        matches!(self.state, State::Idle | State::Running | State::Gate(_) | State::TakenOver)
    }

    fn log(&mut self, code: &'static str, step: Option<usize>, agent: Option<&str>) {
        self.trace.push(Entry { at: self.now, code, step, agent: agent.map(String::from) });
    }

    pub fn decide(&mut self, now: u64, ev: Event) -> Vec<Action> {
        self.now = now;
        match (&self.state, ev) {
            (_, Event::Stop) if self.live() => self.halt(Halt::Stopped),
            (State::Idle, Event::Start) => {
                self.log("start", None, None);
                self.start_step(0)
            }
            (State::Gate(i), Event::GateOk) => {
                let i = *i;
                self.log("gate_ok", Some(i), None);
                self.next(i)
            }
            (State::Running, Event::Takeover) if !matches!(self.task, Some(Task::Shell { .. })) => {
                self.bank();
                self.since = None;
                self.held = Some(self.phase.clone());
                self.state = State::TakenOver;
                self.log("takeover", self.current(), None);
                vec![Action::Unlock]
            }
            (State::TakenOver, Event::Release) => {
                self.state = State::Running;
                self.since = Some(now);
                self.unknown_since = None;
                self.log("release", self.current(), None);
                self.held = None;
                // the user may have finished the stage by hand: judge what is there instead of resending
                match self.task.clone() {
                    Some(Task::Fix { exec, .. }) => self.restart_checks(exec),
                    Some(Task::Work { step, .. }) => {
                        self.phase = Phase::Evaluating;
                        vec![Action::Evaluate { step }]
                    }
                    _ => Vec::new(),
                }
            }
            (State::Running, ev) => self.running(ev),
            _ => Vec::new(),
        }
    }

    /// add the active interval to the stage and total clocks
    fn bank(&mut self) {
        if let Some(s) = self.since {
            let d = self.now.saturating_sub(s);
            self.step_spent += d;
            self.total_spent += d;
            self.since = Some(self.now);
        }
    }

    fn running(&mut self, ev: Event) -> Vec<Action> {
        self.bank();
        let Some(task) = self.task.clone() else { return Vec::new() };
        if self.phase == Phase::Evaluating {
            return match ev {
                Event::Verdict(pass) => self.verdict(task, pass),
                Event::CostLimit => self.halt(Halt::Cost),
                _ => Vec::new(),
            };
        }
        if let Task::Shell { step } = task {
            return match ev {
                Event::Exit(0) => {
                    self.log("shell_pass", Some(step), None);
                    self.advance(step)
                }
                Event::Exit(_) => {
                    self.log("shell_fail", Some(step), None);
                    self.begin_fix(step)
                }
                Event::Tick => self.check_clocks(&task),
                _ => Vec::new(),
            };
        }
        match ev {
            Event::Status(s) => self.status(&task, s),
            Event::Delivered => {
                // a dialog shown before the message (folder trust) was not part of the turn
                if self.phase == Phase::Blocked {
                    self.since = Some(self.now);
                }
                self.phase = Phase::Working { seen: false };
                Vec::new()
            }
            Event::Exit(_) => self.trigger("exit"),
            Event::Limit if self.recipe.fallback_on.iter().any(|t| t == "limit") => self.trigger("limit"),
            Event::CostLimit => self.halt(Halt::Cost),
            Event::Tick => self.check_clocks(&task),
            _ => Vec::new(),
        }
    }

    fn status(&mut self, task: &Task, s: Status) -> Vec<Action> {
        let blocked = self.phase == Phase::Blocked;
        let seen = blocked || self.phase == Phase::Working { seen: true };
        if s != Status::Unknown {
            self.unknown_since = None;
        }
        match s {
            Status::Unknown => {
                self.unknown_since.get_or_insert(self.now);
                Vec::new()
            }
            // BRD 8 rule 3: `blocked` is the user's dialog, so neither fallback nor the stage clock applies
            Status::Blocked => {
                if blocked {
                    return Vec::new();
                }
                self.since = None;
                self.phase = Phase::Blocked;
                self.log("blocked", self.current(), None);
                vec![Action::Notify(Notice::Blocked(self.current().unwrap_or(0)))]
            }
            Status::Working => {
                if blocked {
                    self.since = Some(self.now);
                }
                self.phase = Phase::Working { seen: true };
                Vec::new()
            }
            Status::Idle => {
                if !seen {
                    return Vec::new();
                }
                if blocked {
                    self.since = Some(self.now);
                }
                match task {
                    Task::Work { step, .. } => {
                        self.phase = Phase::Evaluating;
                        vec![Action::Evaluate { step: *step }]
                    }
                    Task::Fix { exec, .. } => {
                        let exec = *exec;
                        self.log("fix_done", Some(exec), None);
                        self.restart_checks(exec)
                    }
                    Task::Shell { .. } => Vec::new(),
                }
            }
        }
    }

    fn check_clocks(&mut self, task: &Task) -> Vec<Action> {
        if self.total_spent >= self.recipe.total_secs {
            return self.halt(Halt::Total);
        }
        let limit = match task {
            Task::Work { step, .. } | Task::Shell { step } => self.recipe.steps[*step].timeout_secs,
            Task::Fix { .. } => FIX_TIMEOUT_SECS,
        };
        if self.phase != Phase::Blocked {
            if limit > 0 && self.step_spent >= limit {
                return match task {
                    Task::Shell { step } => self.halt(Halt::Timeout(*step)),
                    _ => self.trigger("timeout"),
                };
            }
            if self.unknown_since.is_some_and(|u| self.now.saturating_sub(u) > UNKNOWN_SECS) {
                return self.trigger("unknown");
            }
        }
        Vec::new()
    }

    /// BRD 8 rule 2: an agent failed. Next agent of the chain, same message, or stop and call the user.
    fn trigger(&mut self, why: &'static str) -> Vec<Action> {
        match self.task.clone() {
            Some(Task::Work { step, agent }) => {
                if !self.recipe.fallback_on.iter().any(|t| t == why) {
                    return self.halt(Halt::Failed { step, why });
                }
                self.log(why, Some(step), Some(&agent));
                self.failed.entry(step).or_default().push(agent);
                self.start_step(step)
            }
            Some(Task::Fix { .. }) => self.halt(Halt::FixFailed),
            _ => Vec::new(),
        }
    }

    fn verdict(&mut self, task: Task, pass: bool) -> Vec<Action> {
        let Task::Work { step, agent } = task else { return Vec::new() };
        self.log(if pass { "pass" } else { "fail" }, Some(step), Some(&agent));
        match (pass, self.recipe.steps[step].kind == "check") {
            (true, _) => self.advance(step),
            (false, true) => self.begin_fix(step),
            // a work stage with nothing to show is not a fallback trigger: the user decides
            (false, false) => self.halt(Halt::StepFailed(step)),
        }
    }

    /// stage `i` passed: a gate waits for the user, otherwise go on
    fn advance(&mut self, i: usize) -> Vec<Action> {
        if self.recipe.steps[i].gate && !self.recipe.steps[i].human {
            self.state = State::Gate(i);
            self.task = None;
            self.since = None;
            self.log("gate", Some(i), None);
            return vec![Action::Notify(Notice::Gate(i))];
        }
        self.next(i)
    }

    fn next(&mut self, i: usize) -> Vec<Action> {
        if i + 1 >= self.recipe.steps.len() {
            self.state = State::Done;
            self.task = None;
            self.since = None;
            self.log("done", None, None);
            return vec![Action::Unlock, Action::Notify(Notice::Done)];
        }
        self.start_step(i + 1)
    }

    fn start_step(&mut self, i: usize) -> Vec<Action> {
        self.state = State::Running;
        self.step_spent = 0;
        self.unknown_since = None;
        self.phase = Phase::Working { seen: false };
        let s = self.recipe.steps[i].clone();
        // Ship: the harness never commits, pushes, or opens a PR itself (SEC-30)
        if s.human {
            self.state = State::Gate(i);
            self.task = None;
            self.since = None;
            self.log("ship", Some(i), None);
            return vec![Action::Notify(Notice::Gate(i))];
        }
        self.since = Some(self.now);
        if s.shell {
            self.task = Some(Task::Shell { step: i });
            self.log("shell", Some(i), None);
            return vec![Action::RunShell { step: i }];
        }
        let tried = self.failed.get(&i).cloned().unwrap_or_default();
        let mut skipped_worker = false;
        let mut pick = None;
        for a in &s.agents {
            if tried.contains(a) {
                continue;
            }
            // BRD 8 rule 4: the checker is never the agent that did the work
            if s.kind == "check" && self.worker.as_deref() == Some(a.as_str()) {
                skipped_worker = true;
                self.log("skip_worker", Some(i), Some(a));
                continue;
            }
            pick = Some(a.clone());
            break;
        }
        let Some(agent) = pick else {
            return self.halt(if skipped_worker {
                Halt::NoChecker(i)
            } else if tried.is_empty() {
                Halt::NoAgent(i)
            } else {
                Halt::Exhausted(i)
            });
        };
        if s.kind == "work" {
            self.worker = Some(agent.clone());
            self.exec = Some(i);
        }
        self.log("send", Some(i), Some(&agent));
        self.task = Some(Task::Work { step: i, agent: agent.clone() });
        vec![Action::Send { step: i, agent }]
    }

    /// BRD 8 rule 5: a check failed. Fix round with the work agent, or the end of the rounds.
    fn begin_fix(&mut self, failed: usize) -> Vec<Action> {
        let (Some(exec), Some(agent)) = (self.exec, self.worker.clone()) else { return self.halt(Halt::StepFailed(failed)) };
        if self.round >= self.recipe.max_rounds {
            if self.recipe.after_max == "escalate" {
                self.log("escalate", Some(exec), Some(&agent));
                self.failed.entry(exec).or_default().push(agent);
                self.round = 0;
                return self.start_step(exec);
            }
            return self.halt(Halt::MaxRounds);
        }
        self.round += 1;
        self.log("fix", Some(failed), Some(&agent));
        self.step_spent = 0;
        self.unknown_since = None;
        self.since = Some(self.now);
        self.phase = Phase::Working { seen: false };
        self.task = Some(Task::Fix { exec, agent: agent.clone() });
        vec![Action::SendFix { step: exec, agent, failed }]
    }

    /// every check after Execute runs again, from the first one
    fn restart_checks(&mut self, exec: usize) -> Vec<Action> {
        match self.recipe.steps.iter().enumerate().skip(exec + 1).find(|(_, s)| s.kind == "check") {
            Some((i, _)) => self.start_step(i),
            None => self.next(exec),
        }
    }

    fn halt(&mut self, h: Halt) -> Vec<Action> {
        self.log("halt", self.current(), None);
        self.state = State::Halted(h);
        self.task = None;
        self.since = None;
        vec![Action::Unlock, Action::Notify(Notice::Halted)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action::*;

    fn recipe(extra: &str, steps: &str) -> Recipe {
        let text = format!("{extra}\n{steps}");
        let p = crate::recipe::parse(&text, "t", None);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        p.recipe.unwrap()
    }

    fn all() -> Vec<String> {
        ["claude", "codex", "omp", "opencode"].map(String::from).to_vec()
    }

    const FEATURE: &str = r#"
[[step]]
id = "plan"
agents = ["claude"]
output = "plan.md"
gate = true
timeout = "10m"
[[step]]
id = "execute"
agents = ["claude", "codex", "omp"]
inputs = ["plan.md"]
timeout = "60s"
[[step]]
id = "test"
kind = "check"
shell = true
[[step]]
id = "scan"
kind = "check"
agents = ["claude", "codex"]
output = "scan.md"
[[step]]
id = "ship"
human = true
"#;

    fn feature(extra: &str) -> Run {
        Run::new(recipe(extra, FEATURE), &all())
    }

    fn send(step: usize, agent: &str) -> Action {
        Send { step, agent: agent.into() }
    }

    /// drive a stage's agent through one finished turn: working, then idle
    fn turn(r: &mut Run, t: u64) -> Vec<Action> {
        r.decide(t, Event::Status(Status::Working));
        r.decide(t + 1, Event::Status(Status::Idle))
    }

    /// run up to the Execute stage with claude, ready for its turn
    fn at_execute(r: &mut Run) {
        assert_eq!(r.decide(0, Event::Start), vec![send(0, "claude")]);
        assert_eq!(turn(r, 1), vec![Evaluate { step: 0 }]);
        assert_eq!(r.decide(3, Event::Verdict(true)), vec![Notify(Notice::Gate(0))]);
        assert_eq!(r.state, State::Gate(0));
        assert_eq!(r.decide(100, Event::GateOk), vec![send(1, "claude")]);
    }

    #[test]
    fn happy_path_ends_at_the_ship_gate_then_done() {
        let mut r = feature("");
        at_execute(&mut r);
        assert_eq!(turn(&mut r, 101), vec![Evaluate { step: 1 }]);
        assert_eq!(r.decide(103, Event::Verdict(true)), vec![RunShell { step: 2 }]);
        assert_eq!(r.decide(110, Event::Exit(0)), vec![send(3, "codex")]); // checker is not the worker
        assert_eq!(turn(&mut r, 111), vec![Evaluate { step: 3 }]);
        assert_eq!(r.decide(113, Event::Verdict(true)), vec![Notify(Notice::Gate(4))]);
        assert_eq!(r.state, State::Gate(4));
        assert_eq!(r.decide(500, Event::GateOk), vec![Unlock, Notify(Notice::Done)]);
        assert_eq!(r.state, State::Done);
        assert!(r.trace.iter().any(|e| e.code == "skip_worker" && e.agent.as_deref() == Some("claude")));
    }

    #[test]
    fn nothing_but_the_user_gets_past_a_gate() {
        let mut r = feature("");
        at_execute(&mut r);
        r.decide(101, Event::Status(Status::Working));
        r.decide(102, Event::Status(Status::Idle));
        r.decide(103, Event::Verdict(true));
        r.decide(104, Event::Exit(0));
        r.decide(105, Event::Status(Status::Working));
        r.decide(106, Event::Status(Status::Idle));
        r.decide(107, Event::Verdict(true));
        assert_eq!(r.state, State::Gate(4));
        for ev in [Event::Tick, Event::Verdict(true), Event::Exit(0), Event::Status(Status::Idle), Event::Release, Event::Start, Event::Takeover] {
            assert_eq!(r.decide(99999, ev), vec![]);
            assert_eq!(r.state, State::Gate(4));
        }
        // a gate ignores the clocks: 3 hours at the gate do not trip the 2 hour total
        assert_eq!(r.decide(99999 + 3 * 3600, Event::Tick), vec![]);
        assert_eq!(r.state, State::Gate(4));
    }

    #[test]
    fn killed_agent_falls_back_with_the_same_stage_and_never_returns() {
        let mut r = feature("");
        at_execute(&mut r);
        r.decide(101, Event::Status(Status::Working));
        assert_eq!(r.decide(110, Event::Exit(1)), vec![send(1, "codex")]);
        assert_eq!(r.decide(120, Event::Exit(1)), vec![send(1, "omp")]);
        assert_eq!(r.decide(130, Event::Exit(1)), vec![Unlock, Notify(Notice::Halted)]);
        assert_eq!(r.state, State::Halted(Halt::Exhausted(1)));
        assert_eq!(r.trace.iter().filter(|e| e.code == "exit").count(), 3);
    }

    #[test]
    fn a_trigger_missing_from_fallback_on_stops_instead() {
        let mut r = feature("fallback_on = [\"timeout\"]");
        at_execute(&mut r);
        r.decide(101, Event::Status(Status::Working));
        r.decide(110, Event::Exit(1));
        assert_eq!(r.state, State::Halted(Halt::Failed { step: 1, why: "exit" }));
    }

    #[test]
    fn blocked_never_falls_back_and_stops_the_stage_clock() {
        let mut r = feature(""); // execute timeout 60s
        at_execute(&mut r); // execute starts at t=100
        r.decide(110, Event::Status(Status::Working));
        assert_eq!(r.decide(130, Event::Status(Status::Blocked)), vec![Notify(Notice::Blocked(1))]);
        assert_eq!(r.decide(130, Event::Status(Status::Blocked)), vec![]);
        // hours at the dialog: no timeout, no fallback, no total-limit trip
        assert_eq!(r.decide(130 + 5 * 3600, Event::Tick), vec![]);
        assert_eq!(r.state, State::Running);
        assert_eq!(r.decide(130 + 5 * 3600, Event::Exit(0)), vec![send(1, "codex")]); // a real exit still counts
        // clock: 30s spent before the dialog (100..130); resume and the rest of the 60s is left
        let mut r = feature("");
        at_execute(&mut r);
        r.decide(110, Event::Status(Status::Working));
        r.decide(130, Event::Status(Status::Blocked));
        r.decide(1000, Event::Status(Status::Working));
        assert_eq!(r.decide(1029, Event::Tick), vec![]);
        assert_eq!(r.decide(1030, Event::Tick), vec![send(1, "codex")]);
        assert!(r.trace.iter().any(|e| e.code == "timeout"));
    }

    #[test]
    fn a_dialog_before_the_message_is_delivered_is_not_the_turn() {
        // B2: the agent's folder-trust dialog shows before the stage message can go in
        let mut r = feature("");
        at_execute(&mut r);
        assert_eq!(r.decide(105, Event::Status(Status::Blocked)), vec![Notify(Notice::Blocked(1))]);
        assert_eq!(r.decide(105 + 3600, Event::Tick), vec![]); // an hour at the dialog: no timeout
        assert_eq!(r.decide(3705, Event::Delivered), vec![]);
        // still idle right after delivery, before the agent picks the message up: not a finished turn
        assert_eq!(r.decide(3706, Event::Status(Status::Idle)), vec![]);
        assert_eq!(turn(&mut r, 3710), vec![Evaluate { step: 1 }]);
    }

    #[test]
    fn unknown_triggers_fallback_only_after_two_minutes() {
        let mut r = Run::new(recipe("", &FEATURE.replace("timeout = \"60s\"", "timeout = \"10m\"")), &all());
        at_execute(&mut r);
        r.decide(101, Event::Status(Status::Working));
        r.decide(102, Event::Status(Status::Unknown));
        assert_eq!(r.decide(102 + 120, Event::Tick), vec![]);
        assert_eq!(r.decide(102 + 121, Event::Tick), vec![send(1, "codex")]);
        assert!(r.trace.iter().any(|e| e.code == "unknown"));
        // any other status clears it
        let mut r = feature("");
        at_execute(&mut r);
        r.decide(102, Event::Status(Status::Unknown));
        r.decide(150, Event::Status(Status::Working));
        assert_eq!(r.decide(102 + 200, Event::Tick), vec![send(1, "codex")]); // 60s stage timeout, not unknown
        assert!(!r.trace.iter().any(|e| e.code == "unknown"));
    }

    #[test]
    fn limit_is_a_trigger_only_when_listed() {
        let mut r = feature("");
        at_execute(&mut r);
        assert_eq!(r.decide(101, Event::Limit), vec![]);
        let mut r = feature("fallback_on = [\"exit\", \"limit\"]");
        at_execute(&mut r);
        assert_eq!(r.decide(101, Event::Limit), vec![send(1, "codex")]);
    }

    #[test]
    fn cost_cap_stops_and_does_not_move_the_cost_to_another_agent() {
        let mut r = feature("");
        at_execute(&mut r);
        assert_eq!(r.decide(101, Event::CostLimit), vec![Unlock, Notify(Notice::Halted)]);
        assert_eq!(r.state, State::Halted(Halt::Cost));
    }

    /// a check that always fails stops after max_rounds fix rounds, never more
    #[test]
    fn failing_check_stops_at_round_n() {
        for (rounds, want) in [(0, 0), (1, 1), (2, 2), (3, 3), (9, 3)] {
            let mut r = feature(&format!("max_rounds = {rounds}"));
            at_execute(&mut r);
            turn(&mut r, 101);
            r.decide(103, Event::Verdict(true));
            let mut fixes = 0;
            let mut t = 110;
            // shell test fails every time
            let mut acts = r.decide(t, Event::Exit(1));
            while matches!(acts.first(), Some(SendFix { .. })) {
                fixes += 1;
                assert_eq!(acts, vec![SendFix { step: 1, agent: "claude".into(), failed: 2 }]);
                t += 10;
                // the fix turn ends, the checks restart from the first one after Execute
                assert_eq!(turn(&mut r, t), vec![RunShell { step: 2 }]);
                t += 10;
                acts = r.decide(t, Event::Exit(1));
            }
            assert_eq!(fixes, want, "max_rounds {rounds}");
            assert_eq!(r.state, State::Halted(Halt::MaxRounds));
            assert_eq!(acts, vec![Unlock, Notify(Notice::Halted)]);
        }
    }

    #[test]
    fn escalate_moves_execute_to_the_next_agent_and_resets_rounds() {
        let mut r = feature("max_rounds = 1\nafter_max = \"escalate\"");
        at_execute(&mut r);
        turn(&mut r, 101);
        r.decide(103, Event::Verdict(true));
        assert!(matches!(r.decide(110, Event::Exit(1)).first(), Some(SendFix { .. })));
        turn(&mut r, 120);
        assert_eq!(r.decide(130, Event::Exit(1)), vec![send(1, "codex")]);
        assert_eq!(r.round, 0);
        turn(&mut r, 140);
        r.decide(143, Event::Verdict(true));
        // checks now compare against the new worker (codex): scan picks claude
        assert_eq!(r.decide(150, Event::Exit(0)), vec![send(3, "claude")]);
    }

    #[test]
    fn a_check_stage_that_fails_goes_to_a_fix_and_the_checks_run_again() {
        let mut r = feature("");
        at_execute(&mut r);
        turn(&mut r, 101);
        r.decide(103, Event::Verdict(true));
        r.decide(110, Event::Exit(0));
        turn(&mut r, 111);
        assert_eq!(r.decide(113, Event::Verdict(false)), vec![SendFix { step: 1, agent: "claude".into(), failed: 3 }]);
        assert_eq!(turn(&mut r, 120), vec![RunShell { step: 2 }]);
        assert_eq!(r.round, 1);
    }

    #[test]
    fn checker_is_never_the_worker() {
        let only_worker = "[[step]]\nid = \"a\"\nagents = [\"claude\"]\n[[step]]\nid = \"c\"\nkind = \"check\"\nagents = [\"claude\"]\noutput = \"c.md\"\n";
        let mut r = Run::new(recipe("", only_worker), &all());
        r.decide(0, Event::Start);
        turn(&mut r, 1);
        let acts = r.decide(3, Event::Verdict(true));
        assert_eq!(acts, vec![Unlock, Notify(Notice::Halted)]);
        assert_eq!(r.state, State::Halted(Halt::NoChecker(1)));
    }

    #[test]
    fn work_stage_without_its_artifact_stops_for_the_user() {
        let mut r = feature("");
        r.decide(0, Event::Start);
        turn(&mut r, 1);
        r.decide(3, Event::Verdict(false));
        assert_eq!(r.state, State::Halted(Halt::StepFailed(0)));
    }

    #[test]
    fn stage_without_an_installed_agent_halts_at_start() {
        let mut r = Run::new(recipe("", FEATURE), &["codex".to_string()]); // plan wants claude only
        assert_eq!(r.decide(0, Event::Start), vec![Unlock, Notify(Notice::Halted)]);
        assert_eq!(r.state, State::Halted(Halt::NoAgent(0)));
    }

    #[test]
    fn total_duration_is_a_hard_stop() {
        let mut r = feature("total = \"1m\"");
        r.decide(0, Event::Start);
        r.decide(1, Event::Status(Status::Working));
        assert_eq!(r.decide(59, Event::Tick), vec![]);
        assert_eq!(r.decide(60, Event::Tick), vec![Unlock, Notify(Notice::Halted)]);
        assert_eq!(r.state, State::Halted(Halt::Total));
    }

    #[test]
    fn recipe_cannot_raise_sec30_limits_even_if_built_by_hand() {
        let mut rec = recipe("", FEATURE);
        rec.max_rounds = 99;
        rec.total_secs = 99 * 3600;
        rec.steps[1].agents = ["claude", "codex", "omp", "opencode"].map(String::from).to_vec();
        let r = Run::new(rec, &all());
        assert_eq!((r.recipe.max_rounds, r.recipe.total_secs, r.recipe.steps[1].agents.len()), (3, 7200, 3));
    }

    #[test]
    fn stop_halts_from_anywhere_and_ends_are_final() {
        for before in 0..3 {
            let mut r = feature("");
            if before > 0 {
                at_execute(&mut r);
            }
            if before > 1 {
                r.decide(101, Event::Status(Status::Blocked));
            }
            assert_eq!(r.decide(200, Event::Stop), vec![Unlock, Notify(Notice::Halted)]);
            assert_eq!(r.state, State::Halted(Halt::Stopped));
            for ev in [Event::Start, Event::GateOk, Event::Release, Event::Tick, Event::Verdict(true), Event::Exit(0)] {
                assert_eq!(r.decide(300, ev), vec![]);
            }
            assert_eq!(r.state, State::Halted(Halt::Stopped));
        }
    }

    #[test]
    fn takeover_holds_everything_and_release_judges_what_is_there() {
        let mut r = feature("");
        at_execute(&mut r);
        r.decide(101, Event::Status(Status::Working));
        assert_eq!(r.decide(110, Event::Takeover), vec![Unlock]);
        assert_eq!(r.state, State::TakenOver);
        for ev in [Event::Tick, Event::Exit(1), Event::Status(Status::Idle), Event::GateOk, Event::Verdict(true)] {
            assert_eq!(r.decide(99999, ev), vec![]);
        }
        assert_eq!(r.state, State::TakenOver);
        // no clock ran while the user drove the pane: the 60s stage timeout still has 50s
        assert_eq!(r.decide(100000, Event::Release), vec![Evaluate { step: 1 }]);
        assert_eq!(r.decide(100001, Event::Verdict(true)), vec![RunShell { step: 2 }]);
        // a shell stage cannot be taken over
        assert_eq!(r.decide(100002, Event::Takeover), vec![]);
        assert_eq!(r.state, State::Running);
    }

    #[test]
    fn stale_events_do_nothing() {
        let mut r = feature("");
        assert_eq!(r.decide(0, Event::Verdict(true)), vec![]); // before start
        r.decide(0, Event::Start);
        assert_eq!(r.decide(1, Event::Verdict(true)), vec![]); // not evaluating
        assert_eq!(r.decide(2, Event::Status(Status::Idle)), vec![]); // never seen working
        turn(&mut r, 3);
        assert_eq!(r.decide(5, Event::Status(Status::Idle)), vec![]); // already evaluating
    }

    #[test]
    fn shell_timeout_halts_and_shell_ignores_pane_status() {
        let text = "[[step]]\nid = \"a\"\nagents = [\"claude\"]\n[[step]]\nid = \"t\"\nkind = \"check\"\nshell = true\ntimeout = \"30s\"\n";
        let mut r = Run::new(recipe("", text), &all());
        r.decide(0, Event::Start);
        turn(&mut r, 1);
        assert_eq!(r.decide(3, Event::Verdict(true)), vec![RunShell { step: 1 }]);
        assert_eq!(r.decide(4, Event::Status(Status::Idle)), vec![]);
        assert_eq!(r.decide(40, Event::Tick), vec![Unlock, Notify(Notice::Halted)]);
        assert_eq!(r.state, State::Halted(Halt::Timeout(1)));
    }

    #[test]
    fn work_stage_after_ship_runs_after_the_gate() {
        let text = "[[step]]\nid = \"a\"\nagents = [\"claude\"]\n[[step]]\nid = \"s\"\nhuman = true\n[[step]]\nid = \"laporan\"\nagents = [\"codex\"]\noutput = \"laporan.md\"\n";
        let mut r = Run::new(recipe("", text), &all());
        r.decide(0, Event::Start);
        turn(&mut r, 1);
        assert_eq!(r.decide(3, Event::Verdict(true)), vec![Notify(Notice::Gate(1))]);
        assert_eq!(r.decide(9, Event::GateOk), vec![send(2, "codex")]);
        turn(&mut r, 10);
        assert_eq!(r.decide(12, Event::Verdict(true)), vec![Unlock, Notify(Notice::Done)]);
    }
}
