//! Messages the UI shows in its own language (TECH 20): a code plus named args, never prose. The UI renders
//! `err.<code>` (ui/src/lib/i18n), prefixes `at` and appends `err.then.<then>`; OS or TOML text rides in an arg as is.
use serde_json::{json, Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Diag {
    pub code: &'static str,
    pub args: Vec<(&'static str, String)>,
}

impl Diag {
    pub fn new(code: &'static str) -> Self {
        Diag { code, args: Vec::new() }
    }

    pub fn arg(mut self, k: &'static str, v: impl ToString) -> Self {
        self.args.push((k, v.to_string()));
        self
    }

    /// config key the problem is about, e.g. `profile[1].permission`
    pub fn at(self, at: impl ToString) -> Self {
        self.arg("at", at)
    }

    /// what loading does about it: `default`, `ignored`, `skip_profile`, `skip_account`, `skip_plugin`, …
    pub fn then(self, then: &'static str) -> Self {
        self.arg("then", then)
    }

    pub fn json(&self) -> Value {
        let args: Map<String, Value> = self.args.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
        json!({"code": self.code, "args": args})
    }

    /// reply of a UI method that failed: `{"error": code, "args": {…}}`
    pub fn reply(&self) -> Value {
        json!({"error": self.code, "args": self.json()["args"]})
    }
}

impl From<&'static str> for Diag {
    fn from(code: &'static str) -> Self {
        Diag::new(code)
    }
}

/// `code k=v …`, for logs and tests
impl std::fmt::Display for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.code)?;
        self.args.iter().try_for_each(|(k, v)| write!(f, " {k}={v}"))
    }
}
