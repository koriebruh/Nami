//! Per-source ingest pipeline rules (Logs part 2): an ordered list of at
//! most [`MAX_RULES`] rules a source's events go through after normalization
//! and before the pattern and the row are built. Rules are validated and
//! compiled once per rule-set version ([`parse_rules`], [`compile`]) and the
//! compiled form is applied to each event ([`CompiledPipeline::apply`]).
//!
//! Kinds (R-P2-2, one flat object keyed by `type`): `drop` (the first match
//! ends processing), `remove_fields`, `redact` (by key name at any depth,
//! case-insensitive, and/or by regex over `message` and every string or
//! number value of `fields`, R-P2-5), `rename` (overwrites the target; a
//! missing source is a no-op) and `sample` (keeps the 1st, (n+1)th, … event
//! the rule sees, per rule and per engine instance).
//!
//! Regexes use the `regex` crate (linear time), at most
//! [`MAX_REGEX_CHARS`] characters, compiled with a size limit and a lazy-DFA
//! cache limit of 256 KiB each, at most [`MAX_REGEXES`] per pipeline
//! (R-P2-26). No error message and no `Debug` output ever shows a pattern: a
//! regex can carry the shape of a secret. Compiling and applying are CPU
//! work, so callers run them off the async workers (`spawn_blocking`).

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::normalize::{self, LogEvent};
use crate::logs::query::parse_path;

pub const MAX_RULES: usize = 50;
pub const MAX_REGEX_CHARS: usize = 256;
/// `RegexBuilder::size_limit` of every pipeline regex (R-P2-26).
pub const REGEX_SIZE_LIMIT: usize = 256 * 1024;
/// `RegexBuilder::dfa_size_limit` of every pipeline regex (R-P2-26).
pub const REGEX_DFA_SIZE_LIMIT: usize = 256 * 1024;
/// Regexes per pipeline, `message_regex` and `redact.regex` together (R-P2-26).
pub const MAX_REGEXES: usize = 20;
/// Most paths of a `remove_fields` rule and keys of a `redact` rule.
pub const MAX_RULE_ITEMS: usize = 50;
pub const MAX_KEY_CHARS: usize = 64;
pub const MAX_EQUALS_CHARS: usize = 1024;
pub const SAMPLE_RANGE: std::ops::RangeInclusive<u32> = 2..=10_000;
/// Most sample events one `POST .../pipeline/test` takes.
pub const MAX_TEST_EVENTS: usize = 100;
/// `POST .../pipeline/test` previews one user may have running; the next is
/// refused with 429. Each compiles regexes and runs up to
/// [`MAX_TEST_EVENTS`] events on a blocking thread.
pub const MAX_PIPELINE_PREVIEWS_PER_USER: usize = 2;
/// Previews running on one instance, every user together; past it, 429.
pub const MAX_PIPELINE_PREVIEWS_PER_INSTANCE: usize = 8;
pub const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Drop,
    RemoveFields,
    Redact,
    Rename,
    Sample,
}

impl RuleKind {
    fn name(self) -> &'static str {
        match self {
            RuleKind::Drop => "drop",
            RuleKind::RemoveFields => "remove_fields",
            RuleKind::Redact => "redact",
            RuleKind::Rename => "rename",
            RuleKind::Sample => "sample",
        }
    }

    /// The optional keys of a rule this kind uses (besides `type`).
    fn uses(self, key: &str) -> bool {
        let used: &[&str] = match self {
            RuleKind::Drop => &["match"],
            RuleKind::RemoveFields => &["paths"],
            RuleKind::Redact => &["keys", "regex"],
            RuleKind::Rename => &["from", "to"],
            RuleKind::Sample => &["n", "match"],
        };
        used.contains(&key)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_regex: Option<String>,
}

/// One rule as stored and sent (R-P2-2): a flat object keyed by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(rename = "type")]
    pub kind: RuleKind,
    #[serde(rename = "match", default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Match>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
}

/// A rule that cannot be stored: `rules[3]: regex must be at most 256
/// characters`. The message never contains a pattern.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("rules[{index}]: {message}")]
pub struct PipelineError {
    pub index: usize,
    pub message: String,
}

/// What a pipeline did with one event.
pub enum Outcome {
    Keep(LogEvent),
    Dropped,
    SampledOut,
}

enum Target {
    Level,
    Message,
    Field(Vec<String>),
}

enum Condition {
    /// The target's text (a string itself, a number or boolean as its JSON
    /// text) equals `text`.
    Equals {
        target: Target,
        text: String,
    },
    MessageRegex(Regex),
}

enum Step {
    Drop(Condition),
    Remove(Vec<Vec<String>>),
    Redact {
        /// Lowercased.
        keys: HashSet<String>,
        regex: Option<Regex>,
    },
    Rename {
        from: Target,
        to: Target,
    },
    Sample {
        n: u64,
        when: Option<Condition>,
        /// Index into [`CompiledPipeline::counters`].
        counter: usize,
    },
}

/// A validated rule set, ready to apply. Sample counters live here, so they
/// restart whenever the rules are compiled again (after an edit).
pub struct CompiledPipeline {
    steps: Vec<Step>,
    counters: Vec<AtomicU64>,
}

impl fmt::Debug for CompiledPipeline {
    // Never the patterns: a regex can hold a secret's shape.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledPipeline")
            .field("rules", &self.steps.len())
            .finish()
    }
}

impl CompiledPipeline {
    pub fn empty() -> CompiledPipeline {
        CompiledPipeline {
            steps: Vec::new(),
            counters: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Runs the rules top to bottom on one event.
    pub fn apply(&self, mut event: LogEvent) -> Outcome {
        for step in &self.steps {
            match step {
                Step::Drop(condition) => {
                    if condition.matches(&event) {
                        return Outcome::Dropped;
                    }
                }
                Step::Sample { n, when, counter } => {
                    if when.as_ref().is_none_or(|c| c.matches(&event))
                        && !self.counters[*counter]
                            .fetch_add(1, Ordering::Relaxed)
                            .is_multiple_of(*n)
                    {
                        return Outcome::SampledOut;
                    }
                }
                Step::Remove(paths) => {
                    for path in paths {
                        remove_path(&mut event.fields, path);
                    }
                }
                Step::Redact { keys, regex } => {
                    if !keys.is_empty() {
                        redact_keys(&mut event.fields, keys);
                    }
                    if let Some(regex) = regex {
                        if let Some(replaced) = replace_matches(regex, &event.message) {
                            event.message = replaced;
                        }
                        for value in event.fields.values_mut() {
                            redact_value(value, regex);
                        }
                    }
                }
                Step::Rename { from, to } => {
                    if let Some(value) = take(&mut event, from) {
                        put(&mut event, to, value);
                    }
                }
            }
        }
        Outcome::Keep(event)
    }
}

impl Condition {
    fn matches(&self, event: &LogEvent) -> bool {
        match self {
            Condition::Equals { target, text } => match target {
                Target::Level => event.level == *text,
                Target::Message => event.message == *text,
                Target::Field(path) => lookup(&event.fields, path)
                    .and_then(value_text)
                    .is_some_and(|value| value == *text),
            },
            Condition::MessageRegex(regex) => regex.is_match(&event.message),
        }
    }
}

/// The comparison text of a value: a string itself, a number or boolean as
/// its JSON text (like search's `eq`); objects, arrays and null have none.
fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

fn lookup<'a>(fields: &'a Map<String, Value>, path: &[String]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut value = fields.get(first)?;
    for segment in rest {
        value = value.as_object()?.get(segment)?;
    }
    Some(value)
}

/// Removes the value at `path`; a missing or non-object step is a no-op.
fn remove_path(fields: &mut Map<String, Value>, path: &[String]) -> Option<Value> {
    let (last, parents) = path.split_last()?;
    let mut map = fields;
    for segment in parents {
        match map.get_mut(segment) {
            Some(Value::Object(next)) => map = next,
            _ => return None,
        }
    }
    map.remove(last)
}

/// Inserts `value` at `path`, creating objects on the way and replacing a
/// non-object step with `{}`.
fn insert_path(fields: &mut Map<String, Value>, path: &[String], value: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut map = fields;
    for segment in parents {
        let entry = map
            .entry(segment.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        let Value::Object(next) = entry else {
            return;
        };
        map = next;
    }
    map.insert(last.clone(), value);
}

fn take(event: &mut LogEvent, target: &Target) -> Option<Value> {
    match target {
        Target::Level => Some(Value::String(std::mem::replace(
            &mut event.level,
            normalize::DEFAULT_LEVEL.to_string(),
        ))),
        Target::Message => Some(Value::String(std::mem::take(&mut event.message))),
        Target::Field(path) => remove_path(&mut event.fields, path),
    }
}

fn put(event: &mut LogEvent, target: &Target, value: Value) {
    match target {
        // Names and numbers the decoders know; anything else is `info`.
        Target::Level => event.level = normalize::normalize_level(&value).to_string(),
        Target::Message => {
            event.message = match value {
                Value::String(text) => text,
                other => other.to_string(),
            }
        }
        Target::Field(path) => insert_path(&mut event.fields, path, value),
    }
}

/// Replaces the value of every key of `map`, and of the objects at any
/// depth below it (array elements included), whose lowercased name is in
/// `keys`. Recursion depth is bounded by the decoders (at most 128 levels).
fn redact_keys(map: &mut Map<String, Value>, keys: &HashSet<String>) {
    for (key, value) in map.iter_mut() {
        if keys.contains(&key.to_lowercase()) {
            *value = Value::String(REDACTED.to_string());
        } else {
            redact_keys_below(value, keys);
        }
    }
}

fn redact_keys_below(value: &mut Value, keys: &HashSet<String>) {
    match value {
        Value::Object(map) => redact_keys(map, keys),
        Value::Array(items) => items.iter_mut().for_each(|v| redact_keys_below(v, keys)),
        _ => {}
    }
}

/// Applies the regex to every string at any depth, and to the JSON text of
/// every number: a number with a match becomes the replaced text (R-P2-5).
/// Booleans, nulls and keys are left alone.
fn redact_value(value: &mut Value, regex: &Regex) {
    match value {
        Value::String(text) => {
            if let Some(replaced) = replace_matches(regex, text) {
                *text = replaced;
            }
        }
        Value::Number(number) => {
            if let Some(replaced) = replace_matches(regex, &number.to_string()) {
                *value = Value::String(replaced);
            }
        }
        Value::Object(map) => map.values_mut().for_each(|v| redact_value(v, regex)),
        Value::Array(items) => items.iter_mut().for_each(|v| redact_value(v, regex)),
        Value::Bool(_) | Value::Null => {}
    }
}

/// `text` with every non-empty match replaced by [`REDACTED`], or `None`
/// when nothing matched. Zero-width matches are skipped (defense in depth:
/// [`compile`] already refuses redact regexes that can match empty), so a
/// marker is never put between characters.
fn replace_matches(regex: &Regex, text: &str) -> Option<String> {
    let mut out = String::new();
    let mut last = 0;
    let mut replaced = false;
    for found in regex.find_iter(text) {
        if found.start() == found.end() {
            continue;
        }
        out.push_str(&text[last..found.start()]);
        out.push_str(REDACTED);
        last = found.end();
        replaced = true;
    }
    if !replaced {
        return None;
    }
    out.push_str(&text[last..]);
    Some(out)
}

/// `value` must be a JSON array of at most [`MAX_RULES`] rule objects. Each
/// element is read on its own, so an error names its index.
pub fn parse_rules(value: &Value) -> Result<Vec<Rule>, PipelineError> {
    let Value::Array(items) = value else {
        return Err(PipelineError {
            index: 0,
            message: "rules must be a JSON array".to_string(),
        });
    };
    if items.len() > MAX_RULES {
        return Err(PipelineError {
            index: MAX_RULES,
            message: format!("at most {MAX_RULES} rules"),
        });
    }
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            serde_json::from_value::<Rule>(item.clone()).map_err(|error| PipelineError {
                index,
                message: serde_message(&error.to_string()),
            })
        })
        .collect()
}

/// serde's message, without the offending value: "invalid type: string
/// \"…\", expected u32" would otherwise echo whatever the sender put in a
/// wrongly typed slot, a pattern included. Unknown keys and rule types get
/// fixed texts too, since their names are whatever the sender wrote.
fn serde_message(message: &str) -> String {
    if message.starts_with("unknown variant") {
        return "unknown rule type".to_string();
    }
    if message.starts_with("unknown field") {
        return "unknown key".to_string();
    }
    for prefix in ["invalid type", "invalid value", "invalid length"] {
        if message.starts_with(prefix) {
            return match message.rfind(", expected ") {
                Some(at) => format!("{prefix}, {}", &message[at + 2..]),
                None => prefix.to_string(),
            };
        }
    }
    message.to_string()
}

/// Validates `rules` and builds their compiled form. Errors name the rule
/// by its 0-based index and never echo a pattern.
pub fn compile(rules: &[Rule]) -> Result<CompiledPipeline, PipelineError> {
    let mut compiler = Compiler {
        regexes: 0,
        counters: 0,
    };
    let mut steps = Vec::with_capacity(rules.len());
    for (index, rule) in rules.iter().enumerate() {
        steps.push(
            compiler
                .rule(rule)
                .map_err(|message| PipelineError { index, message })?,
        );
    }
    let counters = (0..compiler.counters).map(|_| AtomicU64::new(0)).collect();
    Ok(CompiledPipeline { steps, counters })
}

struct Compiler {
    regexes: usize,
    counters: usize,
}

impl Compiler {
    fn rule(&mut self, rule: &Rule) -> Result<Step, String> {
        let kind = rule.kind;
        let present = [
            ("match", rule.when.is_some()),
            ("paths", rule.paths.is_some()),
            ("keys", rule.keys.is_some()),
            ("regex", rule.regex.is_some()),
            ("from", rule.from.is_some()),
            ("to", rule.to.is_some()),
            ("n", rule.n.is_some()),
        ];
        if let Some((key, _)) = present
            .iter()
            .find(|(key, present)| *present && !kind.uses(key))
        {
            return Err(format!("{key} is not used by {} rules", kind.name()));
        }
        match kind {
            RuleKind::Drop => {
                let when = rule.when.as_ref().ok_or("drop needs a match")?;
                Ok(Step::Drop(self.condition(when)?))
            }
            RuleKind::RemoveFields => {
                let paths = rule.paths.as_deref().unwrap_or_default();
                if !(1..=MAX_RULE_ITEMS).contains(&paths.len()) {
                    return Err(format!("remove_fields needs 1 to {MAX_RULE_ITEMS} paths"));
                }
                let paths = paths
                    .iter()
                    .map(|path| {
                        if path == "level" || path == "message" {
                            Err("level and message cannot be removed; use rename or redact"
                                .to_string())
                        } else {
                            parse_path(path)
                        }
                    })
                    .collect::<Result<_, _>>()?;
                Ok(Step::Remove(paths))
            }
            RuleKind::Redact => {
                let keys = rule.keys.as_deref().unwrap_or_default();
                if keys.is_empty() && rule.regex.is_none() {
                    return Err("redact needs keys or regex".to_string());
                }
                if keys.len() > MAX_RULE_ITEMS {
                    return Err(format!("redact may list at most {MAX_RULE_ITEMS} keys"));
                }
                let keys = keys
                    .iter()
                    .map(|key| {
                        let len = key.chars().count();
                        if (1..=MAX_KEY_CHARS).contains(&len) && !key.chars().any(char::is_control)
                        {
                            Ok(key.to_lowercase())
                        } else {
                            Err(format!(
                                "redact key must be 1-{MAX_KEY_CHARS} characters without control characters"
                            ))
                        }
                    })
                    .collect::<Result<_, _>>()?;
                let regex = match &rule.regex {
                    Some(pattern) => {
                        let regex = self.regex(pattern)?;
                        if can_match_empty(pattern) {
                            return Err("redact regex must not match the empty string".to_string());
                        }
                        Some(regex)
                    }
                    None => None,
                };
                Ok(Step::Redact { keys, regex })
            }
            RuleKind::Rename => {
                let (Some(from), Some(to)) = (&rule.from, &rule.to) else {
                    return Err("rename needs from and to".to_string());
                };
                Ok(Step::Rename {
                    from: target(from)?,
                    to: target(to)?,
                })
            }
            RuleKind::Sample => {
                let n = rule.n.filter(|n| SAMPLE_RANGE.contains(n)).ok_or_else(|| {
                    format!(
                        "n must be between {} and {}",
                        SAMPLE_RANGE.start(),
                        SAMPLE_RANGE.end()
                    )
                })?;
                let when = rule
                    .when
                    .as_ref()
                    .map(|when| self.condition(when))
                    .transpose()?;
                let counter = self.counters;
                self.counters += 1;
                Ok(Step::Sample {
                    n: u64::from(n),
                    when,
                    counter,
                })
            }
        }
    }

    fn condition(&mut self, when: &Match) -> Result<Condition, String> {
        match (&when.field, &when.equals, &when.message_regex) {
            (Some(field), Some(equals), None) => {
                let mut text = match equals {
                    Value::String(text) => text.clone(),
                    Value::Number(_) | Value::Bool(_) => equals.to_string(),
                    _ => return Err("equals must be a string, number or boolean".to_string()),
                };
                if text.chars().count() > MAX_EQUALS_CHARS {
                    return Err(format!(
                        "equals must be at most {MAX_EQUALS_CHARS} characters"
                    ));
                }
                let target = target(field)?;
                if let Target::Level = target {
                    // Stored levels are canonical: `DEBUG`, `dbg` or the
                    // pino number 20 all mean `debug`.
                    if let Some(level) = normalize::known_level(equals) {
                        text = level.to_string();
                    }
                }
                Ok(Condition::Equals { target, text })
            }
            (None, None, Some(pattern)) => Ok(Condition::MessageRegex(self.regex(pattern)?)),
            _ => Err("match needs either field and equals, or message_regex".to_string()),
        }
    }

    /// Counts the regex against [`MAX_REGEXES`] before spending CPU on it.
    fn regex(&mut self, pattern: &str) -> Result<Regex, String> {
        self.regexes += 1;
        if self.regexes > MAX_REGEXES {
            return Err(format!(
                "a pipeline may hold at most {MAX_REGEXES} regular expressions"
            ));
        }
        compile_regex(pattern)
    }
}

fn target(raw: &str) -> Result<Target, String> {
    Ok(match raw {
        "level" => Target::Level,
        "message" => Target::Message,
        _ => Target::Field(parse_path(raw)?),
    })
}

fn compile_regex(pattern: &str) -> Result<Regex, String> {
    if pattern.chars().count() > MAX_REGEX_CHARS {
        return Err(format!(
            "regex must be at most {MAX_REGEX_CHARS} characters"
        ));
    }
    // Neither message echoes the pattern: a regex can carry a secret's shape.
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .build()
        .map_err(|error| match error {
            regex::Error::CompiledTooBig(_) => {
                "regex is too complex (compiled size limit)".to_string()
            }
            _ => "regex is not a valid regular expression".to_string(),
        })
}

/// Whether the pattern can match an empty string anywhere. `is_match("")`
/// is not enough: `\b` does not match `""` but matches between words. A
/// pattern that never matches (`minimum_len` of `None`) is not empty.
fn can_match_empty(pattern: &str) -> bool {
    match regex_syntax::Parser::new().parse(pattern) {
        Ok(hir) => hir.properties().minimum_len() == Some(0),
        // `compile_regex` accepted it, so this does not happen; refuse anyway.
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use chrono::Utc;
    use serde_json::{Value, json};

    use super::*;
    use crate::logs::ingest::normalize::from_json;

    fn event(value: Value) -> LogEvent {
        from_json(value.as_object().unwrap().clone(), Utc::now())
    }

    fn rules(value: Value) -> CompiledPipeline {
        compile(&parse_rules(&value).unwrap()).unwrap()
    }

    fn kept(outcome: Outcome) -> LogEvent {
        match outcome {
            Outcome::Keep(event) => event,
            Outcome::Dropped => panic!("dropped"),
            Outcome::SampledOut => panic!("sampled out"),
        }
    }

    fn rejected(value: Value) -> PipelineError {
        match parse_rules(&value) {
            Err(error) => error,
            Ok(parsed) => compile(&parsed).expect_err("must be rejected"),
        }
    }

    #[test]
    fn drop_matches_on_level_message_and_fields_and_ends_processing() {
        let p = rules(json!([
            {"type":"drop","match":{"field":"level","equals":"debug"}},
            {"type":"drop","match":{"field":"http.status","equals":200}},
            {"type":"drop","match":{"message_regex":"^healthz"}},
            {"type":"rename","from":"a","to":"b"}
        ]));
        assert!(matches!(
            p.apply(event(json!({"level":"DEBUG","message":"x"}))),
            Outcome::Dropped
        ));
        assert!(matches!(
            p.apply(event(json!({"message":"x","http":{"status":200}}))),
            Outcome::Dropped
        ));
        assert!(
            matches!(
                p.apply(event(json!({"message":"x","http":{"status":"200"}}))),
                Outcome::Dropped
            ),
            "a number compares by its JSON text, like search's eq"
        );
        assert!(matches!(
            p.apply(event(json!({"message":"healthz ok"}))),
            Outcome::Dropped
        ));
        let e = kept(p.apply(event(json!({"message":"real","a":1}))));
        assert_eq!(e.fields.get("b"), Some(&json!(1)));
        assert!(e.fields.get("a").is_none());
    }

    #[test]
    fn remove_fields_deletes_nested_keys_and_ignores_missing_ones() {
        let p = rules(
            json!([{"type":"remove_fields","paths":["user.email","password","nope.deeper"]}]),
        );
        let e = kept(p.apply(event(
            json!({"message":"m","password":"x","user":{"email":"a@b","id":7}}),
        )));
        assert_eq!(Value::Object(e.fields), json!({"user":{"id":7}}));
    }

    #[test]
    fn redact_by_key_name_is_case_insensitive_at_any_depth() {
        let p = rules(json!([{"type":"redact","keys":["password","Authorization"]}]));
        let e = kept(p.apply(event(json!({
            "message":"login",
            "PassWord":"hunter2",
            "req":{"headers":{"authorization":"Bearer abc"},"list":[{"password":"x"}]}
        }))));
        assert_eq!(
            Value::Object(e.fields),
            json!({
                "PassWord":"[REDACTED]",
                "req":{"headers":{"authorization":"[REDACTED]"},"list":[{"password":"[REDACTED]"}]}
            })
        );
    }

    #[test]
    fn redact_by_regex_rewrites_the_message_and_every_string_value() {
        let p = rules(json!([{"type":"redact","regex":"\\b\\d{4}-\\d{4}-\\d{4}-\\d{4}\\b"}]));
        let e = kept(p.apply(event(json!({
            "message":"card 4111-1111-1111-1111 declined",
            "card":{"number":"4111-1111-1111-1111","n":[ "4111-1111-1111-1111" ]},
            "amount": 41111111,
            "ok": true
        }))));
        assert_eq!(e.message, "card [REDACTED] declined");
        assert_eq!(
            e.fields["card"],
            json!({"number":"[REDACTED]","n":["[REDACTED]"]})
        );
        assert_eq!(
            e.fields["amount"],
            json!(41111111),
            "a number without a match is left alone"
        );
        assert_eq!(e.fields["ok"], json!(true));

        // R-P2-5: a number is matched on its JSON text; one with a match becomes a string.
        let p = rules(json!([{"type":"redact","regex":"\\d{16}"}]));
        let e = kept(p.apply(event(json!({"message":"m","card":4111111111111111_u64,"deep":[{"pan":5500000000000004_u64}]}))));
        assert_eq!(e.fields["card"], json!("[REDACTED]"));
        assert_eq!(e.fields["deep"], json!([{"pan":"[REDACTED]"}]));
    }

    #[test]
    fn rename_moves_values_overwrites_targets_and_handles_level_and_message() {
        let p = rules(json!([
            {"type":"rename","from":"msg2","to":"message"},
            {"type":"rename","from":"sev","to":"level"},
            {"type":"rename","from":"a.b","to":"c.d"},
            {"type":"rename","from":"missing","to":"x"}
        ]));
        let e = kept(p.apply(event(
            json!({"message":"old","msg2":"new","sev":"WARNING","a":{"b":1},"c":{"d":0,"e":2}}),
        )));
        assert_eq!(e.message, "new");
        assert_eq!(e.level, "warn");
        assert_eq!(Value::Object(e.fields), json!({"a":{},"c":{"d":1,"e":2}}));

        let p = rules(json!([{"type":"rename","from":"message","to":"original"}]));
        let e = kept(p.apply(event(json!({"message":"keep me"}))));
        assert_eq!(e.message, "");
        assert_eq!(e.fields["original"], json!("keep me"));
    }

    #[test]
    fn sample_keeps_the_first_of_every_n_matching_events() {
        let p = rules(json!([{"type":"sample","n":3,"match":{"field":"level","equals":"info"}}]));
        let outcomes: Vec<bool> = (0..7)
            .map(|_| matches!(p.apply(event(json!({"message":"m"}))), Outcome::Keep(_)))
            .collect();
        assert_eq!(outcomes, [true, false, false, true, false, false, true]);
        assert!(
            matches!(
                p.apply(event(json!({"message":"m","level":"error"}))),
                Outcome::Keep(_)
            ),
            "events the match leaves out are not sampled"
        );
        let all = rules(json!([{"type":"sample","n":2}]));
        assert!(matches!(
            all.apply(event(json!({"message":"a"}))),
            Outcome::Keep(_)
        ));
        assert!(matches!(
            all.apply(event(json!({"message":"b"}))),
            Outcome::SampledOut
        ));
    }

    #[test]
    fn invalid_rules_are_rejected_with_their_index() {
        let long = "a".repeat(257);
        for (value, index, needle) in [
            (json!([{"type":"drop"}]), 0, "match"),
            (json!([{"type":"drop","match":{"field":"a"}}]), 0, "equals"),
            (
                json!([{"type":"drop","match":{"field":"a","equals":1,"message_regex":"x"}}]),
                0,
                "either",
            ),
            (
                json!([{"type":"rename","from":"a","to":"b"},{"type":"redact"}]),
                1,
                "keys or regex",
            ),
            (json!([{"type":"redact","regex":"("}]), 0, "regex"),
            (json!([{"type":"redact","regex":long}]), 0, "256"),
            (
                json!([{"type":"drop","match":{"message_regex":long}}]),
                0,
                "256",
            ),
            (json!([{"type":"sample","n":1}]), 0, "2"),
            (json!([{"type":"sample","n":10001}]), 0, "10000"),
            (
                json!([{"type":"remove_fields","paths":["level"]}]),
                0,
                "level",
            ),
            (json!([{"type":"remove_fields","paths":[]}]), 0, "paths"),
            (
                json!([{"type":"remove_fields","paths":["a..b"]}]),
                0,
                "path",
            ),
            (json!([{"type":"rename","from":"a","to":"b","n":2}]), 0, "n"),
            (
                json!([{"type":"drop","match":{"field":"a","equals":1},"extra":1}]),
                0,
                "unknown key",
            ),
            (json!([{"type":"explode"}]), 0, "unknown rule type"),
            (json!([{"type":"redact","keys":[""]}]), 0, "key"),
        ] {
            let error = rejected(value.clone());
            assert_eq!(error.index, index, "{value}: {error}");
            assert!(
                error.to_string().to_lowercase().contains(needle),
                "{value}: {error}"
            );
        }
        let too_many = Value::Array(vec![json!({"type":"sample","n":2}); 51]);
        assert_eq!(rejected(too_many).index, 50);
        assert!(
            parse_rules(&json!({"type":"drop"})).is_err(),
            "not an array"
        );
    }

    #[test]
    fn a_regex_that_matches_the_empty_string_is_rejected() {
        for pattern in ["a*", "\\b", "(?:)", "x?", "^", "$", "a|", "(?i)a*"] {
            let error = rejected(json!([{"type":"redact","regex":pattern}]));
            assert!(error.message.contains("empty"), "{pattern}: {error}");
        }
    }

    #[test]
    fn a_pipeline_holds_at_most_twenty_regexes() {
        let at_limit: Vec<Value> = (0..MAX_REGEXES)
            .map(|n| json!({"type":"redact","regex":format!("secret{n}")}))
            .collect();
        assert!(compile(&parse_rules(&Value::Array(at_limit.clone())).unwrap()).is_ok());
        let mut over = at_limit;
        over.push(json!({"type":"drop","match":{"message_regex":"x"}}));
        let error = rejected(Value::Array(over));
        assert_eq!(error.index, MAX_REGEXES, "{error}");
        assert!(error.message.contains("20"), "{error}");
        // A pattern whose compiled program passes the size limit is refused, without echoing it.
        let huge = rejected(json!([{"type":"redact","regex":"\\w{200}\\w{50}"}]));
        assert!(
            huge.message.contains("too complex") && !huge.message.contains("\\w"),
            "{huge}"
        );
    }

    #[test]
    fn catastrophic_patterns_run_in_linear_time() {
        // 64 KiB exactly: `from_json` truncates anything longer before the pipeline sees it.
        let message = format!("{}!", "a".repeat(64 * 1024 - 1));
        let p = rules(json!([
            {"type":"drop","match":{"message_regex":"(a+)+$"}},
            {"type":"redact","regex":"(a|aa)+b"}
        ]));
        let started = Instant::now();
        let e = kept(p.apply(event(
            json!({ "message": message, "f": "a".repeat(64 * 1024) }),
        )));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert!(e.message.ends_with('!'));
    }

    #[test]
    fn rules_round_trip_through_json() {
        let value = json!([
            {"type":"drop","match":{"field":"level","equals":"debug"}},
            {"type":"remove_fields","paths":["a.b"]},
            {"type":"redact","keys":["token"],"regex":"secret-\\d+"},
            {"type":"rename","from":"a","to":"b"},
            {"type":"sample","n":10}
        ]);
        let parsed = parse_rules(&value).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
        assert!(compile(&[]).unwrap().is_empty());
    }

    #[test]
    fn no_error_and_no_debug_output_echoes_a_pattern() {
        let secret = "tok_SECRETSHAPE";
        for value in [
            json!([{"type":"redact","regex":format!("{secret}(")}]),
            json!([{"type":"redact","regex":format!("{secret}\\w{{200}}\\w{{50}}")}]),
            json!([{"type":"redact","regex":format!("(?:{secret})?")}]),
            json!([{"type":"redact","regex":format!("{secret}{}", "a".repeat(300))}]),
            json!([{"type":"drop","match":{"message_regex":format!("{secret}[")}}]),
            json!([{"type":"redact","keys":secret}]),
            json!([{"type":"sample","n":secret}]),
            json!([{"type":"drop","match":secret}]),
        ] {
            let error = rejected(value.clone());
            assert!(
                !error.to_string().contains("SECRETSHAPE"),
                "{value}: {error}"
            );
        }
        let p = rules(
            json!([{"type":"redact","regex":secret},{"type":"drop","match":{"message_regex":secret}}]),
        );
        assert_eq!(format!("{p:?}"), "CompiledPipeline { rules: 2 }");
    }

    #[test]
    fn zero_width_matches_never_insert_markers() {
        // compile refuses such redact regexes; a rule built around that check
        // still never puts markers between characters.
        let regex = Regex::new(r"\b|x*").unwrap();
        assert_eq!(replace_matches(&regex, "ab cd"), None);
        let regex = Regex::new(r"x*").unwrap();
        assert_eq!(
            replace_matches(&regex, "axxb").as_deref(),
            Some("a[REDACTED]b")
        );
    }

    #[test]
    fn a_level_condition_accepts_a_level_number() {
        let p = rules(json!([{"type":"drop","match":{"field":"level","equals":50}}]));
        assert!(matches!(
            p.apply(event(json!({"message":"m","level":"error"}))),
            Outcome::Dropped
        ));
        assert!(matches!(
            p.apply(event(json!({"message":"m","level":"warn"}))),
            Outcome::Keep(_)
        ));
    }

    #[test]
    fn the_regex_count_is_checked_before_compiling() {
        let mut over: Vec<Value> = (0..MAX_REGEXES)
            .map(|n| json!({"type":"redact","regex":format!("secret{n}")}))
            .collect();
        over.push(json!({"type":"drop","match":{"message_regex":"("}}));
        let error = rejected(Value::Array(over));
        assert_eq!(error.index, MAX_REGEXES, "{error}");
        assert!(
            error.message.contains("at most 20 regular expressions"),
            "{error}"
        );
    }

    #[test]
    fn a_level_condition_accepts_any_spelling_of_a_level() {
        let p = rules(json!([{"type":"drop","match":{"field":"level","equals":"WARNING"}}]));
        assert!(matches!(
            p.apply(event(json!({"message":"m","level":"warn"}))),
            Outcome::Dropped
        ));
        assert!(matches!(
            p.apply(event(json!({"message":"m"}))),
            Outcome::Keep(_)
        ));
    }
}
