//! User automation: named shell commands bound to an event or an interval.
//!
//! One rule is one command line. Nothing is hidden: the exact string is what
//! the platform shell receives, in the instance folder, with the same
//! environment the game will get. Rules are plain JSON so they can be edited,
//! copied, or pasted without opening the launcher.

use crate::ephemeral_log::EphemeralLog;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

/// Upper bound on stored rules. A long list is a config file, not automation.
pub const MAX_RULES: usize = 32;
pub const MAX_NAME_CHARS: usize = 48;
pub const MAX_COMMAND_CHARS: usize = 2048;
pub const MIN_INTERVAL_SECS: u64 = 10;
pub const MAX_INTERVAL_SECS: u64 = 7 * 24 * 60 * 60;
pub const MIN_TIMEOUT_SECS: u64 = 1;
pub const MAX_TIMEOUT_SECS: u64 = 900;
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// Output kept per run. The rest is dropped so a chatty command cannot grow
/// the launcher's memory.
const MAX_OUTPUT_LINES: usize = 60;
const MAX_OUTPUT_LINE_CHARS: usize = 400;

#[derive(Debug, Error)]
pub enum AutomationError {
    #[error("a rule needs a command")]
    EmptyCommand,
    #[error("rule name is longer than {MAX_NAME_CHARS} characters")]
    NameTooLong,
    #[error("command is longer than {MAX_COMMAND_CHARS} characters")]
    CommandTooLong,
    #[error("intervals must be at least {MIN_INTERVAL_SECS} seconds")]
    IntervalTooShort,
    #[error("timeouts must be between {MIN_TIMEOUT_SECS} and {MAX_TIMEOUT_SECS} seconds")]
    TimeoutOutOfRange,
    #[error("too many rules (limit {MAX_RULES})")]
    TooManyRules,
    #[error("could not read automation: {0}")]
    Read(String),
    #[error("could not write automation: {0}")]
    Write(String),
}

/// When a rule fires. Values are stored as plain words so a hand-edited file
/// stays readable, and every spelling the launcher accepts is also accepted
/// from that file: `pre_launch`, `before launch`, and `every 15m` all work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// Only when the user presses Run.
    #[default]
    Manual,
    /// Once, when the launcher starts.
    Startup,
    /// Before Java starts for any instance.
    PreLaunch,
    /// After the game process exits.
    PostExit,
    /// Repeated while the launcher is open.
    Every { secs: u64 },
}

impl<'de> Deserialize<'de> for Trigger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Word(String),
            Every { every: Span },
            Secs { secs: u64 },
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Span {
            Fields { secs: u64 },
            Number(u64),
        }
        match Repr::deserialize(deserializer)? {
            Repr::Word(word) => Self::parse(&word).ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "unknown trigger \"{word}\"; use manual, startup, pre_launch, \
                     post_exit, or every 15m"
                ))
            }),
            Repr::Every { every } => Ok(Self::Every {
                secs: match every {
                    Span::Fields { secs } | Span::Number(secs) => secs,
                },
            }),
            Repr::Secs { secs } => Ok(Self::Every { secs }),
        }
    }
}

impl Trigger {
    /// Short, plain label used by the menu and by exported text.
    pub fn label(self) -> String {
        match self {
            Self::Manual => "On demand".to_owned(),
            Self::Startup => "At launcher start".to_owned(),
            Self::PreLaunch => "Before launch".to_owned(),
            Self::PostExit => "After game exit".to_owned(),
            Self::Every { secs } => format!("Every {}", span(secs)),
        }
    }

    /// Accepts the labels above plus short forms: `manual`, `start`, `launch`,
    /// `exit`, `every 15m`, `15m`, `90s`, `2h`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        let text = text.replace(['-', '_'], " ");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        match text.as_str() {
            "manual" | "on demand" | "on demand only" | "run now" | "none" => Some(Self::Manual),
            "startup" | "start" | "at start" | "at launcher start" | "app start" => {
                Some(Self::Startup)
            }
            "prelaunch" | "pre launch" | "launch" | "before launch" | "before start" => {
                Some(Self::PreLaunch)
            }
            "postexit" | "post exit" | "exit" | "after exit" | "after game exit" | "on exit" => {
                Some(Self::PostExit)
            }
            other => {
                let rest = other
                    .strip_prefix("every ")
                    .or_else(|| other.strip_prefix("each "))
                    .unwrap_or(other);
                parse_span(rest).map(|secs| Self::Every { secs })
            }
        }
    }

    pub fn is_interval(self) -> bool {
        matches!(self, Self::Every { .. })
    }
}

/// Human duration in the fewest characters: `45s`, `15m`, `2h`, `1d`.
fn span(secs: u64) -> String {
    let days = secs / 86_400;
    if days > 0 && days * 86_400 == secs {
        return format!("{days}d");
    }
    let hours = secs / 3_600;
    if hours > 0 && hours * 3_600 == secs {
        return format!("{hours}h");
    }
    let minutes = secs / 60;
    if minutes > 0 && minutes * 60 == secs {
        return format!("{minutes}m");
    }
    format!("{secs}s")
}

fn parse_span(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (number, unit) = text.split_at(
        text.find(|character: char| !character.is_ascii_digit())
            .unwrap_or(text.len()),
    );
    let value = number.parse::<u64>().ok()?;
    if value == 0 {
        return None;
    }
    let multiplier = match unit.trim() {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
        "d" | "day" | "days" => 86_400,
        _ => return None,
    };
    value
        .checked_mul(multiplier)
        .filter(|secs| *secs <= MAX_INTERVAL_SECS)
}

/// One command and when it runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub trigger: Trigger,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Last interval start, in Unix seconds. Zero means "not run yet".
    #[serde(default)]
    pub last_run_unix: i64,
}

fn default_true() -> bool {
    true
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

impl Rule {
    pub fn new(id: &str, name: &str, command: &str, trigger: Trigger) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            command: command.to_owned(),
            trigger,
            enabled: true,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            last_run_unix: 0,
        }
    }

    pub fn validate(&self) -> Result<(), AutomationError> {
        if self.command.trim().is_empty() {
            return Err(AutomationError::EmptyCommand);
        }
        if self.command.chars().count() > MAX_COMMAND_CHARS {
            return Err(AutomationError::CommandTooLong);
        }
        if self.name.chars().count() > MAX_NAME_CHARS {
            return Err(AutomationError::NameTooLong);
        }
        if let Trigger::Every { secs } = self.trigger
            && !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&secs)
        {
            return Err(AutomationError::IntervalTooShort);
        }
        if !(MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(&self.timeout_secs) {
            return Err(AutomationError::TimeoutOutOfRange);
        }
        Ok(())
    }

    /// A name for menus: the typed name, or the command itself.
    pub fn title(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() {
            truncate(self.command.trim(), MAX_NAME_CHARS)
        } else {
            name.to_owned()
        }
    }

    pub fn is_due(&self, now_unix: i64) -> bool {
        let Trigger::Every { secs } = self.trigger else {
            return false;
        };
        if !self.enabled {
            return false;
        }
        now_unix.saturating_sub(self.last_run_unix) >= secs as i64
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut short = text
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    short.push('…');
    short
}

/// The rule list plus the file it came from.
#[derive(Debug, Clone)]
pub struct Automation {
    path: PathBuf,
    rules: Vec<Rule>,
}

impl Default for Automation {
    fn default() -> Self {
        Self {
            path: default_path(),
            rules: Vec::new(),
        }
    }
}

impl Automation {
    pub fn with_rules(rules: Vec<Rule>) -> Self {
        Self {
            path: default_path(),
            rules,
        }
    }

    /// Read the shared file. A missing or broken file yields an empty list and
    /// a line of plain text instead of an error, so one bad edit never stops
    /// the launcher from starting.
    pub fn load() -> (Self, Option<String>) {
        Self::load_from(default_path())
    }

    pub fn load_from(path: PathBuf) -> (Self, Option<String>) {
        let mut store = Self {
            path,
            rules: Vec::new(),
        };
        let text = match std::fs::read_to_string(&store.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (store, None),
            Err(error) => {
                return (
                    store,
                    Some(format!("Could not read automation rules: {error}")),
                );
            }
        };
        let decoded = match serde_json::from_str::<Vec<Rule>>(&text) {
            Ok(rules) => rules,
            Err(error) => {
                return (
                    store,
                    Some(format!("Automation rules are not valid JSON: {error}")),
                );
            }
        };
        let total = decoded.len();
        store.rules = decoded
            .into_iter()
            .filter(|rule| rule.validate().is_ok())
            .take(MAX_RULES)
            .collect();
        let dropped = total.saturating_sub(store.rules.len());
        let warning = (dropped > 0)
            .then(|| format!("Ignored {dropped} automation rule(s) that were empty or invalid."));
        (store, warning)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn rules_mut(&mut self) -> &mut Vec<Rule> {
        &mut self.rules
    }

    pub fn rule(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.id == id)
    }

    /// Add or replace one rule. Replacing keeps the original position so the
    /// list never reorders itself while it is being edited.
    pub fn upsert(&mut self, rule: Rule) -> Result<(), AutomationError> {
        rule.validate()?;
        match self.rules.iter_mut().find(|slot| slot.id == rule.id) {
            Some(slot) => *slot = rule,
            None => {
                if self.rules.len() >= MAX_RULES {
                    return Err(AutomationError::TooManyRules);
                }
                self.rules.push(rule);
            }
        }
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.rules.len();
        self.rules.retain(|rule| rule.id != id);
        self.rules.len() != before
    }

    /// Write the file atomically: a crash mid-save leaves the old rules intact.
    pub fn save(&self) -> Result<(), AutomationError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| AutomationError::Write(error.to_string()))?;
        }
        let text = serde_json::to_string_pretty(&self.rules)
            .map_err(|error| AutomationError::Write(error.to_string()))?;
        let temporary = self.path.with_extension("json.new");
        std::fs::write(&temporary, text.as_bytes())
            .map_err(|error| AutomationError::Write(error.to_string()))?;
        match std::fs::rename(&temporary, &self.path) {
            Ok(()) => Ok(()),
            Err(error) => {
                // Windows can refuse the rename when a reader holds the file.
                let _ = std::fs::remove_file(&temporary);
                Err(AutomationError::Write(error.to_string()))
            }
        }
    }

    /// Enabled rules for one event trigger, in list order.
    pub fn commands_for(&self, trigger: Trigger) -> Vec<&Rule> {
        self.rules
            .iter()
            .filter(|rule| rule.enabled && rule.trigger == trigger)
            .collect()
    }

    /// Interval rules that are due now, newest-run stamp included in the
    /// returned clones so the caller can persist the schedule.
    pub fn take_due(&mut self, now_unix: i64) -> Vec<Rule> {
        let mut due = Vec::new();
        for rule in self.rules.iter_mut() {
            if rule.is_due(now_unix) {
                rule.last_run_unix = now_unix;
                due.push(rule.clone());
            }
        }
        due
    }

    /// Join the enabled commands for a launch event into one shell line, kept
    /// inside the launch plan so ordering is guaranteed and no command can run
    /// after the game has already created its window.
    pub fn launch_chain(&self, trigger: Trigger) -> Option<String> {
        let commands = self
            .commands_for(trigger)
            .into_iter()
            .map(|rule| rule.command.trim())
            .filter(|command| !command.is_empty())
            .collect::<Vec<_>>();
        (!commands.is_empty()).then(|| commands.join(" && "))
    }
}

/// Where rules live: beside the instance metadata, override included.
pub fn default_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("KAELIS_DATA_DIR") {
        return PathBuf::from(dir).join("automation.json");
    }
    directories::ProjectDirs::from("gg", "kaelis", "Kaelis")
        .map(|dirs| dirs.config_dir().join("automation.json"))
        .unwrap_or_else(|| PathBuf::from("automation.json"))
}

/// Result of one command, small enough to keep in the UI log.
#[derive(Debug, Clone)]
pub struct RuleRun {
    pub id: String,
    pub name: String,
    pub ok: bool,
    /// One line for the status bar, e.g. `Backup finished in 0.4s`.
    pub summary: String,
    pub lines: Vec<String>,
}

/// Run one rule through the platform shell.
///
/// The process gets no console window on Windows, a hard timeout, and a fixed
/// working directory. Output is bounded. A failure never propagates as an
/// error: it is reported in `RuleRun` so a broken hook cannot break a launch.
pub async fn run_rule(
    rule: &Rule,
    cwd: &Path,
    env: &[(String, String)],
    log: &EphemeralLog,
) -> RuleRun {
    let started = std::time::Instant::now();
    let mut process = shell(rule.command.trim());
    process
        .current_dir(cwd)
        .envs(
            env.iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: automation must never flash a console over the UI.
        process.as_std_mut().creation_flags(0x0800_0000);
    }
    let timeout = Duration::from_secs(rule.timeout_secs.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS));
    log.push(format!("[automation] {}: starting", rule.title()));
    let output = tokio::time::timeout(timeout, process.output()).await;
    let elapsed = started.elapsed();
    let mut lines = Vec::new();
    let (ok, summary) = match output {
        Err(_) => {
            log.push(format!("[automation] {}: timed out", rule.title()));
            (
                false,
                format!("{} timed out after {}s", rule.title(), timeout.as_secs()),
            )
        }
        Ok(Err(error)) => {
            log.push(format!("[automation] {}: {error}", rule.title()));
            (false, format!("{} could not start: {error}", rule.title()))
        }
        Ok(Ok(output)) => {
            for stream in [&output.stdout, &output.stderr] {
                for line in String::from_utf8_lossy(stream).lines() {
                    if lines.len() >= MAX_OUTPUT_LINES {
                        break;
                    }
                    lines.push(truncate(line.trim_end(), MAX_OUTPUT_LINE_CHARS));
                }
            }
            for line in &lines {
                log.push(format!("[automation] {}: {line}", rule.title()));
            }
            let success = output.status.success();
            if !success {
                log.push(format!(
                    "[automation] {}: exited with {}",
                    rule.title(),
                    output.status
                ));
            }
            let summary = if success {
                format!("{} finished in {:.1}s", rule.title(), elapsed.as_secs_f32())
            } else {
                format!(
                    "{} exited with {} after {:.1}s",
                    rule.title(),
                    output.status,
                    elapsed.as_secs_f32()
                )
            };
            (success, summary)
        }
    };
    RuleRun {
        id: rule.id.clone(),
        name: rule.title(),
        ok,
        summary,
        lines,
    }
}

fn shell(command: &str) -> tokio::process::Command {
    if cfg!(windows) {
        let mut process = tokio::process::Command::new("cmd");
        process.args(["/D", "/S", "/C", command]);
        process
    } else {
        let mut process = tokio::process::Command::new("sh");
        process.args(["-c", command]);
        process
    }
}

/// Environment every rule receives, so a command can name what it is acting on
/// without parsing the launcher's own state.
pub fn rule_environment(instance: Option<&crate::instance::Instance>) -> Vec<(String, String)> {
    let mut env = vec![(
        "KAELIS_LAUNCHER".to_owned(),
        crate::brand::LAUNCHER_NAME.to_owned(),
    )];
    if let Some(instance) = instance {
        env.push(("KAELIS_INSTANCE_ID".to_owned(), instance.id.clone()));
        env.push(("KAELIS_INSTANCE_NAME".to_owned(), instance.name.clone()));
        env.push(("KAELIS_MC_VERSION".to_owned(), instance.mc_version.clone()));
        env.push(("KAELIS_LOADER".to_owned(), instance.loader.clone()));
        env.push((
            "KAELIS_INSTANCE_DIR".to_owned(),
            instance.root.display().to_string(),
        ));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "kaelis-automation-{name}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        path
    }

    #[test]
    fn triggers_parse_from_plain_words_and_short_spans() {
        assert_eq!(Trigger::parse("before launch"), Some(Trigger::PreLaunch));
        assert_eq!(Trigger::parse("AFTER_EXIT"), Some(Trigger::PostExit));
        assert_eq!(
            Trigger::parse("Every 15m"),
            Some(Trigger::Every { secs: 900 })
        );
        assert_eq!(Trigger::parse("2h"), Some(Trigger::Every { secs: 7_200 }));
        assert_eq!(Trigger::parse("90"), Some(Trigger::Every { secs: 90 }));
        assert_eq!(Trigger::parse("lots"), None);
    }

    #[test]
    fn labels_round_trip_through_parse() {
        for trigger in [
            Trigger::Manual,
            Trigger::Startup,
            Trigger::PreLaunch,
            Trigger::PostExit,
            Trigger::Every { secs: 90 },
            Trigger::Every { secs: 900 },
            Trigger::Every { secs: 7_200 },
        ] {
            assert_eq!(Trigger::parse(&trigger.label()), Some(trigger));
        }
    }

    #[test]
    fn chains_skip_disabled_rules_and_keep_list_order() {
        let mut automation = Automation::with_rules(vec![
            Rule::new("a", "First", "echo one", Trigger::PreLaunch),
            Rule::new("b", "Second", "echo two", Trigger::PreLaunch),
            Rule::new("c", "Other", "echo three", Trigger::PostExit),
        ]);
        automation.rules[1].enabled = false;
        assert_eq!(
            automation.launch_chain(Trigger::PreLaunch).as_deref(),
            Some("echo one")
        );
        automation.rules[1].enabled = true;
        assert_eq!(
            automation.launch_chain(Trigger::PreLaunch).as_deref(),
            Some("echo one && echo two")
        );
        assert_eq!(automation.launch_chain(Trigger::Startup), None);
    }

    #[test]
    fn intervals_are_due_once_per_window() {
        let mut automation = Automation::with_rules(vec![
            Rule::new("a", "Backup", "echo backup", Trigger::Every { secs: 60 }),
            Rule::new("b", "Off", "echo off", Trigger::Every { secs: 60 }),
            Rule::new("c", "Manual", "echo manual", Trigger::Manual),
        ]);
        automation.rules[1].enabled = false;
        let due = automation.take_due(1_000);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, "a");
        assert!(automation.take_due(1_030).is_empty());
        assert_eq!(automation.take_due(1_060).len(), 1);
    }

    #[test]
    fn validation_rejects_unsafe_values_and_keeps_the_rest() {
        let mut short = Rule::new("a", "Short", "echo hi", Trigger::Every { secs: 1 });
        assert!(matches!(
            short.validate(),
            Err(AutomationError::IntervalTooShort)
        ));
        short.trigger = Trigger::Every { secs: 60 };
        short.command = "   ".to_owned();
        assert!(matches!(
            short.validate(),
            Err(AutomationError::EmptyCommand)
        ));
        short.command = "echo hi".to_owned();
        short.timeout_secs = 10_000;
        assert!(matches!(
            short.validate(),
            Err(AutomationError::TimeoutOutOfRange)
        ));
    }

    #[test]
    fn round_trip_keeps_every_field_and_ignores_broken_rows() {
        let path = temp_path("roundtrip");
        let mut automation = Automation::load_from(path.clone()).0;
        let mut rule = Rule::new(
            "nightly",
            "Nightly backup",
            "echo backup",
            Trigger::Every { secs: 900 },
        );
        rule.last_run_unix = 42;
        automation.upsert(rule.clone()).unwrap();
        automation.save().unwrap();

        let (loaded, warning) = Automation::load_from(path.clone());
        assert!(warning.is_none());
        assert_eq!(loaded.rules().len(), 1);
        let stored = &loaded.rules()[0];
        assert_eq!(stored.command, rule.command);
        assert_eq!(stored.trigger, Trigger::Every { secs: 900 });
        assert_eq!(stored.last_run_unix, 42);
        assert!(stored.enabled);
        let _ = std::fs::remove_file(path);

        let broken = temp_path("broken");
        std::fs::write(&broken, "[{\"id\":\"x\",\"name\":\"\",\"command\":\"\"}]").unwrap();
        let (loaded, warning) = Automation::load_from(broken.clone());
        assert!(loaded.rules().is_empty());
        assert!(warning.is_some());
        let _ = std::fs::remove_file(broken);

        let garbage = temp_path("garbage");
        std::fs::write(&garbage, "{not json").unwrap();
        let (loaded, warning) = Automation::load_from(garbage.clone());
        assert!(loaded.rules().is_empty());
        assert!(warning.unwrap().contains("not valid JSON"));
        let _ = std::fs::remove_file(garbage);
    }

    #[test]
    fn hand_written_files_accept_plain_words_and_both_interval_shapes() {
        let rules: Vec<Rule> = serde_json::from_str(
            r#"[
                {"id":"a","name":"","command":"echo a","trigger":"before_launch"},
                {"id":"b","name":"","command":"echo b","trigger":"every 15m"},
                {"id":"c","name":"","command":"echo c","trigger":{"every":{"secs":60}}},
                {"id":"d","name":"","command":"echo d","trigger":{"every":90}}
            ]"#,
        )
        .expect("parse");
        assert_eq!(rules[0].trigger, Trigger::PreLaunch);
        assert_eq!(rules[1].trigger, Trigger::Every { secs: 900 });
        assert_eq!(rules[2].trigger, Trigger::Every { secs: 60 });
        assert_eq!(rules[3].trigger, Trigger::Every { secs: 90 });
        // Saving writes the canonical spelling back out.
        let encoded = serde_json::to_string(&rules[0]).expect("encode");
        assert!(encoded.contains("\"pre_launch\""));
        assert!(
            serde_json::from_str::<Rule>(
                "{\"id\":\"x\",\"name\":\"\",\"command\":\"a\",\"trigger\":\"whenever\"}"
            )
            .is_err()
        );
    }

    #[test]
    fn the_rule_list_has_a_hard_ceiling() {
        let mut automation = Automation::with_rules(Vec::new());
        for index in 0..MAX_RULES {
            let id = format!("rule-{index}");
            automation
                .upsert(Rule::new(&id, "Rule", "echo hi", Trigger::Manual))
                .unwrap();
        }
        assert!(matches!(
            automation.upsert(Rule::new("one-more", "Rule", "echo hi", Trigger::Manual)),
            Err(AutomationError::TooManyRules)
        ));
        // Replacing an existing rule is always allowed.
        automation
            .upsert(Rule::new("rule-0", "Renamed", "echo bye", Trigger::Manual))
            .unwrap();
        assert_eq!(automation.rules()[0].name, "Renamed");
    }

    #[tokio::test]
    async fn a_rule_runs_through_the_shell_and_reports_output() {
        let log = EphemeralLog::new(200);
        let rule = Rule::new("echo", "Echo", "echo kaelis-automation", Trigger::Manual);
        let run = run_rule(&rule, &std::env::temp_dir(), &[], &log).await;
        assert!(run.ok, "summary: {}", run.summary);
        assert!(
            run.lines
                .iter()
                .any(|line| line.contains("kaelis-automation"))
        );
        assert!(
            log.snapshot()
                .iter()
                .any(|line| line.contains("[automation]"))
        );
    }

    #[tokio::test]
    async fn a_failing_command_is_reported_not_propagated() {
        let log = EphemeralLog::new(200);
        let mut rule = Rule::new("fail", "Fail", "exit 3", Trigger::Manual);
        rule.timeout_secs = 5;
        let run = run_rule(&rule, &std::env::temp_dir(), &[], &log).await;
        assert!(!run.ok);
        assert!(run.summary.contains("exited"));
    }
}
