use std::collections::HashMap;

/// A snapshot of the environment. Names are case-insensitive on Windows, as the OS treats them.
#[derive(Clone, Default, Debug)]
pub struct Env {
    vars: HashMap<String, String>,
}

fn norm(name: &str) -> String {
    if cfg!(windows) {
        name.to_ascii_uppercase()
    } else {
        name.to_string()
    }
}

impl Env {
    pub fn from_process() -> Self {
        let vars = std::env::vars_os().map(|(k, v)| (norm(&k.to_string_lossy()), v.to_string_lossy().into_owned())).collect();
        Env { vars }
    }

    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Env { vars: pairs.iter().map(|(k, v)| (norm(k), v.to_string())).collect() }
    }

    /// The value, set or not (an empty value counts as set).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(&norm(name)).map(|s| s.as_str())
    }

    /// The trimmed value, when it is set and not empty.
    pub fn non_empty(&self, name: &str) -> Option<&str> {
        self.get(name).map(str::trim).filter(|s| !s.is_empty())
    }

    /// The first of `names` that is set (even when empty), like `a ?? b ?? c`.
    pub fn first(&self, names: &[&str]) -> Option<&str> {
        names.iter().find_map(|n| self.get(n))
    }
}
