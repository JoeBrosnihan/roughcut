//! Parameters, however they arrived.
//!
//! A command line writes `--clip IMG_2527 --in 120`; an MCP client sends
//! `{"clip": "IMG_2527", "in": 120}`. Both become the same [`Args`], validated
//! against the same declaration, so a mistake is reported the same way and a
//! command never has to know which door it came through.

use crate::spec::{Cmd, Kind, Param};
use anyhow::{anyhow, bail, Context, Result};
use roughcut_core::model::{ClipId, Project};
use serde_json::{Map, Value};
use std::path::PathBuf;

#[derive(Debug)]
pub struct Args {
    cmd: &'static Cmd,
    values: Map<String, Value>,
}

impl Args {
    /// Check a set of supplied values against the command declaration and
    /// coerce each to the declared type.
    ///
    /// Coercion rather than rejection for numbers and booleans that arrive as
    /// text: the command line has no types at all, and MCP clients differ on
    /// whether they send `12` or `"12"`. Refusing one of those would make the
    /// same call work through one door and fail through the other.
    pub fn new(cmd: &'static Cmd, supplied: Map<String, Value>) -> Result<Self> {
        let mut values = Map::new();
        for (key, raw) in supplied {
            let Some(param) = cmd.param(&key) else {
                let known: Vec<&str> = cmd.params.iter().map(|p| p.name).collect();
                bail!(
                    "{} has no parameter --{key}. It takes: {}",
                    cmd.name,
                    if known.is_empty() { "nothing".to_string() } else { known.join(", ") }
                );
            };
            values.insert(key, coerce(cmd.name, param, raw)?);
        }
        for param in cmd.params {
            if param.required && !values.contains_key(param.name) {
                bail!("{} needs --{}: {}", cmd.name, param.name, param.help);
            }
        }
        Ok(Self { cmd, values })
    }

    fn get(&self, name: &str) -> Option<&Value> {
        debug_assert!(
            self.cmd.param(name).is_some(),
            "{} reads --{name}, which it does not declare",
            self.cmd.name
        );
        self.values.get(name)
    }

    pub fn opt_text(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(Value::as_str)
    }

    pub fn text(&self, name: &str) -> Result<&str> {
        self.opt_text(name)
            .ok_or_else(|| anyhow!("{} needs --{name}", self.cmd.name))
    }

    pub fn opt_int(&self, name: &str) -> Option<i64> {
        self.get(name).and_then(Value::as_i64)
    }

    pub fn int(&self, name: &str) -> Result<i64> {
        self.opt_int(name)
            .ok_or_else(|| anyhow!("{} needs --{name}", self.cmd.name))
    }

    /// A flag that was not given is false. Nothing in the table has a flag
    /// that defaults to true, because `--thing` meaning "turn it off" reads
    /// backwards to everyone including the person who wrote it.
    pub fn flag(&self, name: &str) -> bool {
        self.get(name).and_then(Value::as_bool).unwrap_or(false)
    }

    /// A three-state flag: given true, given false, or not given at all.
    pub fn opt_flag(&self, name: &str) -> Option<bool> {
        self.get(name).and_then(Value::as_bool)
    }

    pub fn opt_path(&self, name: &str) -> Option<PathBuf> {
        self.opt_text(name).map(PathBuf::from)
    }

    pub fn path(&self, name: &str) -> Result<PathBuf> {
        Ok(PathBuf::from(self.text(name)?))
    }

    /// Every value given for a repeatable parameter.
    pub fn paths(&self, name: &str) -> Vec<PathBuf> {
        match self.get(name) {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from)
                .collect(),
            Some(Value::String(s)) => vec![PathBuf::from(s)],
            _ => Vec::new(),
        }
    }

    pub fn choice(&self, name: &str, default: &'static str) -> &str {
        self.opt_text(name).unwrap_or(default)
    }

    /// Which project to work on.
    ///
    /// `ROUGHCUT_PROJECT` stands in when `--project` is absent, so a session
    /// spent editing one cut does not repeat the path on every call.
    pub fn project_path(&self) -> Result<PathBuf> {
        if let Some(path) = self.opt_path("project") {
            return Ok(path);
        }
        match std::env::var_os("ROUGHCUT_PROJECT") {
            Some(p) if !p.is_empty() => Ok(PathBuf::from(p)),
            _ => bail!(
                "{} needs --project, or ROUGHCUT_PROJECT set to a .roughcut file",
                self.cmd.name
            ),
        }
    }

    /// Resolve `--clip` against the bin.
    ///
    /// An id is exact. Anything else is matched against file names, because
    /// nobody — person or agent — reads a list of clips and then wants to
    /// quote a UUID back. An ambiguous name is an error that names the
    /// candidates rather than picking one.
    pub fn clip(&self, project: &Project, name: &str) -> Result<ClipId> {
        let text = self.text(name)?;
        resolve_clip(project, text)
    }

    pub fn opt_clip(&self, project: &Project, name: &str) -> Result<Option<ClipId>> {
        match self.opt_text(name) {
            Some(text) => resolve_clip(project, text).map(Some),
            None => Ok(None),
        }
    }
}

/// Find the one clip `text` names. Shared so `search --clip` and `--clip`
/// proper cannot disagree about what counts as a match.
pub fn resolve_clip(project: &Project, text: &str) -> Result<ClipId> {
    if let Some(clip) = project.clips.iter().find(|c| c.id.to_string() == text) {
        return Ok(clip.id);
    }
    let needle = text.to_lowercase();
    let hits: Vec<&roughcut_core::model::SourceClip> = project
        .clips
        .iter()
        .filter(|c| c.file_name().to_lowercase().contains(&needle))
        .collect();
    match hits.len() {
        1 => Ok(hits[0].id),
        0 => bail!("no clip in the bin is called {text}"),
        _ => {
            let names: Vec<String> = hits.iter().take(8).map(|c| c.file_name()).collect();
            bail!(
                "{text} matches {} clips ({}{}). Use more of the name, or the id.",
                hits.len(),
                names.join(", "),
                if hits.len() > names.len() { ", ..." } else { "" }
            )
        }
    }
}

/// Turn one supplied value into the declared type, or explain why it will not.
fn coerce(cmd: &str, param: &Param, raw: Value) -> Result<Value> {
    if param.many {
        let items = match raw {
            Value::Array(items) => items,
            other => vec![other],
        };
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            out.push(coerce_one(cmd, param, item)?);
        }
        return Ok(Value::Array(out));
    }
    coerce_one(cmd, param, raw)
}

fn coerce_one(cmd: &str, param: &Param, raw: Value) -> Result<Value> {
    let name = param.name;
    match param.kind {
        Kind::Text | Kind::Path => match raw {
            Value::String(s) => Ok(Value::String(s)),
            Value::Number(n) => Ok(Value::String(n.to_string())),
            Value::Bool(b) => Ok(Value::String(b.to_string())),
            other => bail!("{cmd} --{name} wants text, not {}", shape(&other)),
        },
        Kind::Int => match raw {
            Value::Number(n) => n
                .as_i64()
                .map(Value::from)
                .ok_or_else(|| anyhow!("{cmd} --{name} wants a whole number, not {n}")),
            Value::String(s) => s
                .trim()
                .parse::<i64>()
                .map(Value::from)
                .with_context(|| format!("{cmd} --{name} wants a whole number of frames, not {s:?}")),
            other => bail!("{cmd} --{name} wants a whole number, not {}", shape(&other)),
        },
        Kind::Bool => match raw {
            Value::Bool(b) => Ok(Value::Bool(b)),
            Value::String(s) => match s.trim().to_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => Ok(Value::Bool(true)),
                "false" | "no" | "off" | "0" => Ok(Value::Bool(false)),
                other => bail!("{cmd} --{name} wants true or false, not {other:?}"),
            },
            other => bail!("{cmd} --{name} wants true or false, not {}", shape(&other)),
        },
        Kind::Choice(all) => {
            let text = match raw {
                Value::String(s) => s,
                other => bail!("{cmd} --{name} wants one of {}, not {}", all.join(", "), shape(&other)),
            };
            if all.contains(&text.as_str()) {
                Ok(Value::String(text))
            } else {
                bail!("{cmd} --{name} wants one of {}, not {text:?}", all.join(", "))
            }
        }
    }
}

fn shape(v: &Value) -> &'static str {
    match v {
        Value::Null => "nothing",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// Parse `--name value` pairs from a command line into the same shape MCP
/// sends.
///
/// `--flag` on its own is true. A repeatable parameter given twice becomes a
/// list. `--name=value` works too, because that is how half of everything else
/// spells it and being wrong about it costs a confusing error.
pub fn parse_argv(cmd: &'static Cmd, argv: &[String]) -> Result<Args> {
    let mut values: Map<String, Value> = Map::new();
    let mut i = 0;
    while i < argv.len() {
        let token = &argv[i];
        let Some(rest) = token.strip_prefix("--") else {
            bail!(
                "{}: unexpected {token:?}. Everything is a --name value pair; \
                 run `roughcut-cli help {}` to see them.",
                cmd.name,
                cmd.name
            );
        };
        let (name, inline) = match rest.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (rest.to_string(), None),
        };
        let Some(param) = cmd.param(&name) else {
            let known: Vec<&str> = cmd.params.iter().map(|p| p.name).collect();
            bail!(
                "{} has no parameter --{name}. It takes: {}",
                cmd.name,
                if known.is_empty() { "nothing".to_string() } else { known.join(", ") }
            );
        };

        let text = match inline {
            Some(v) => v,
            None => {
                // A flag with nothing after it, or with another parameter
                // after it, means true. Anything else consumes the next token.
                let next = argv.get(i + 1);
                let takes_next = match param.kind {
                    Kind::Bool => next.is_some_and(|n| !n.starts_with("--")),
                    _ => true,
                };
                if takes_next {
                    i += 1;
                    next.ok_or_else(|| {
                        anyhow!("{} --{name} needs a value: {}", cmd.name, param.help)
                    })?
                    .clone()
                } else {
                    "true".to_string()
                }
            }
        };

        let value = Value::String(text);
        if param.many {
            match values.entry(name).or_insert_with(|| Value::Array(Vec::new())) {
                Value::Array(items) => items.push(value),
                slot => *slot = Value::Array(vec![slot.clone(), value]),
            }
        } else if values.insert(name.clone(), value).is_some() {
            bail!("{} --{name} was given twice", cmd.name);
        }
        i += 1;
    }
    Args::new(cmd, values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec;

    fn cmd(name: &str) -> &'static Cmd {
        spec::find(name).expect("command should exist")
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_command_line_and_an_mcp_call_produce_the_same_arguments() {
        let from_line = parse_argv(cmd("mark"), &argv(&["--clip", "IMG_1", "--in", "120"])).unwrap();
        let from_json = Args::new(
            cmd("mark"),
            serde_json::from_str(r#"{"clip":"IMG_1","in":120}"#).unwrap(),
        )
        .unwrap();
        assert_eq!(from_line.opt_text("clip"), from_json.opt_text("clip"));
        assert_eq!(from_line.opt_int("in"), Some(120));
        assert_eq!(from_json.opt_int("in"), Some(120));
    }

    /// Some MCP clients send every number as text. Rejecting that would make
    /// the same edit work from the terminal and fail from the agent.
    #[test]
    fn a_number_sent_as_text_is_still_a_number() {
        let a = Args::new(cmd("split"), serde_json::from_str(r#"{"at":"90"}"#).unwrap()).unwrap();
        assert_eq!(a.int("at").unwrap(), 90);
        let bad = Args::new(cmd("split"), serde_json::from_str(r#"{"at":"soon"}"#).unwrap());
        assert!(bad.unwrap_err().to_string().contains("frames"));
    }

    #[test]
    fn a_flag_on_its_own_is_true() {
        let a = parse_argv(cmd("flag"), &argv(&["--clip", "x", "--off"])).unwrap();
        assert!(a.flag("off"));
        let b = parse_argv(cmd("flag"), &argv(&["--off", "--clip", "x"])).unwrap();
        assert!(b.flag("off"), "a flag before another parameter is still a flag");
        let c = parse_argv(cmd("flag"), &argv(&["--clip", "x", "--off", "false"])).unwrap();
        assert!(!c.flag("off"), "and can still be said explicitly");
        let d = parse_argv(cmd("flag"), &argv(&["--clip", "x"])).unwrap();
        assert!(!d.flag("off"));
        assert_eq!(d.opt_flag("off"), None, "not given is distinguishable");
    }

    #[test]
    fn a_repeatable_parameter_collects() {
        let a = parse_argv(cmd("import"), &argv(&["--path", "a.mp4", "--path", "b.mp4"])).unwrap();
        assert_eq!(a.paths("path"), vec![PathBuf::from("a.mp4"), PathBuf::from("b.mp4")]);
        // And one value is still a list of one, not a special case.
        let b = parse_argv(cmd("import"), &argv(&["--path", "a.mp4"])).unwrap();
        assert_eq!(b.paths("path").len(), 1);
    }

    #[test]
    fn name_equals_value_works_too() {
        let a = parse_argv(cmd("split"), &argv(&["--at=42"])).unwrap();
        assert_eq!(a.int("at").unwrap(), 42);
    }

    #[test]
    fn a_missing_required_parameter_says_what_it_is_for() {
        let e = parse_argv(cmd("split"), &argv(&[])).unwrap_err().to_string();
        assert!(e.contains("--at"), "{e}");
        assert!(e.contains("frame"), "{e}");
    }

    #[test]
    fn an_unknown_parameter_lists_the_real_ones() {
        let e = parse_argv(cmd("split"), &argv(&["--frame", "3"])).unwrap_err().to_string();
        assert!(e.contains("--frame"), "{e}");
        assert!(e.contains("at"), "{e}");
    }

    #[test]
    fn a_choice_is_held_to_its_list() {
        assert!(parse_argv(cmd("trim"), &argv(&["--index", "0", "--edge", "middle", "--by", "1"]))
            .unwrap_err()
            .to_string()
            .contains("head"));
        let ok = parse_argv(cmd("trim"), &argv(&["--index", "0", "--edge", "tail", "--by", "-5"]))
            .unwrap();
        assert_eq!(ok.choice("edge", "head"), "tail");
        assert_eq!(ok.int("by").unwrap(), -5, "a negative frame count is not a flag");
    }

    #[test]
    fn a_value_given_twice_is_refused_rather_than_silently_dropped() {
        let e = parse_argv(cmd("split"), &argv(&["--at", "1", "--at", "2"]))
            .unwrap_err()
            .to_string();
        assert!(e.contains("twice"), "{e}");
    }

    #[test]
    fn a_bare_word_is_refused_with_a_pointer_to_the_help() {
        let e = parse_argv(cmd("split"), &argv(&["42"])).unwrap_err().to_string();
        assert!(e.contains("help split"), "{e}");
    }
}
