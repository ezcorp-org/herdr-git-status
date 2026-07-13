//! Plugin config and env/state path resolution.
//!
//! - [`load_config`] parses `$HERDR_PLUGIN_CONFIG_DIR/config.toml` (flat
//!   `key = value` lines).
//! - The path helpers resolve the herdr-injected env (`HERDR_PLUGIN_*`) with the
//!   same `<tmpdir>/<id>` fallbacks the runtime uses.

use std::path::PathBuf;

/// Status-surfacing strategy (plugin `config.toml` `mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Stock herdr: a "git" pseudo-agent per space in the agents panel. This is
    /// the default — it works on any herdr build, no patch required.
    AgentsPanel,
    /// Opt-in for herdr builds patched with a spaces-card status row:
    /// display-only metadata rendered inside the spaces card.
    Sidebar,
}

/// Plugin user config from `$HERDR_PLUGIN_CONFIG_DIR/config.toml`.
#[derive(Debug, Clone)]
pub struct Config {
    pub mode: Mode,
    pub interval_seconds: u64,
    /// Surface `✓` for fully clean repos (nothing to commit, push, or pull)
    /// instead of clearing the status. `false` restores the pre-1.1 behaviour
    /// where a clean space shows nothing at all.
    pub clean_checkmark: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::AgentsPanel,
            interval_seconds: 5,
            clean_checkmark: true,
        }
    }
}

/// Default plugin id when herdr does not inject `HERDR_PLUGIN_ID`.
const DEFAULT_PLUGIN_ID: &str = "ez-corp.git-status";

/// Load the plugin's own `config.toml`, returning defaults if it is absent.
pub fn load_config() -> Config {
    match std::fs::read_to_string(config_dir().join("config.toml")) {
        Ok(text) => parse_config(&text),
        Err(_) => Config::default(), // no config file — defaults
    }
}

/// Plugin id (`HERDR_PLUGIN_ID`, else `ez-corp.git-status`).
pub fn plugin_id() -> String {
    non_empty_env("HERDR_PLUGIN_ID").unwrap_or_else(|| DEFAULT_PLUGIN_ID.to_string())
}

/// Durable state dir (`HERDR_PLUGIN_STATE_DIR`, else `<tmpdir>/<id>`).
pub fn state_dir() -> PathBuf {
    non_empty_env("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(plugin_id()))
}

/// User config dir (`HERDR_PLUGIN_CONFIG_DIR`, else `<tmpdir>/<id>-config`).
pub fn config_dir() -> PathBuf {
    non_empty_env("HERDR_PLUGIN_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("{}-config", plugin_id())))
}

/// Updater single-instance pid file (`<state_dir>/updater.pid`).
pub fn pid_file() -> PathBuf {
    state_dir().join("updater.pid")
}

// ---- env / path resolution --------------------------------------------------

/// Read `name` from the environment, treating unset AND empty as absent.
pub(crate) fn non_empty_env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// User home directory from `$HOME`, or an empty path when unset.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// XDG config base: `$XDG_CONFIG_HOME` if set (and non-empty), else `~/.config`.
pub(crate) fn config_home() -> PathBuf {
    non_empty_env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
}

// ---- pure parser (hand-rolled, no `toml` crate) -----------------------------

/// Parse the plugin's flat `config.toml` text into a [`Config`], starting from
/// the documented defaults.
///
/// Recognised keys: `mode` (`agents-panel` | `sidebar`), `interval_seconds`
/// (numeric `>= 1`), and `clean_checkmark` (`true` | `false`). Unknown keys are
/// ignored.
fn parse_config(text: &str) -> Config {
    let mut cfg = Config::default();
    for line in text.split('\n') {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = parse_kv_line(line) else {
            continue;
        };
        match key {
            "mode" if value == "sidebar" => cfg.mode = Mode::Sidebar,
            "mode" if value == "agents-panel" => cfg.mode = Mode::AgentsPanel,
            "clean_checkmark" if value == "true" => cfg.clean_checkmark = true,
            "clean_checkmark" if value == "false" => cfg.clean_checkmark = false,
            // Accept any numeric >= 1; the struct stores whole seconds, so a
            // fractional value is truncated (the daemon uses it as a coarse
            // cadence).
            "interval_seconds" => {
                if let Ok(n) = value.parse::<f64>() {
                    if n >= 1.0 {
                        cfg.interval_seconds = n as u64;
                    }
                }
            }
            _ => {}
        }
    }
    cfg
}

/// Split one flat `key = value` line into `(key, unquoted_value)`.
///
/// The key is one or more ASCII letters/underscores; the value is everything
/// after the FIRST `=` with surrounding whitespace trimmed (non-empty required)
/// and at most one leading and one trailing quote (`"` or `'`) removed. Inline
/// `#` comments are NOT stripped.
fn parse_kv_line(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_') {
        return None;
    }
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some((key, strip_quotes(value)))
}

/// Remove at most one leading and one trailing quote (`"` or `'`), independently.
fn strip_quotes(s: &str) -> &str {
    let is_quote = |c: char| c == '"' || c == '\'';
    let s = s.strip_prefix(is_quote).unwrap_or(s);
    s.strip_suffix(is_quote).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_empty_text_yields_documented_defaults() {
        let cfg = parse_config("");
        // agents-panel is the default: it works on stock herdr, no patch needed.
        assert_eq!(cfg.mode, Mode::AgentsPanel);
        assert_eq!(cfg.interval_seconds, 5);
        assert!(cfg.clean_checkmark);
    }

    #[test]
    fn config_clean_checkmark_only_accepts_booleans() {
        assert!(!parse_config("clean_checkmark = false").clean_checkmark);
        assert!(parse_config("clean_checkmark = true").clean_checkmark);
        assert!(parse_config("clean_checkmark = \"true\"").clean_checkmark);
        // Unknown value leaves the default (true) untouched.
        assert!(parse_config("clean_checkmark = maybe").clean_checkmark);
    }

    #[test]
    fn config_mode_only_accepts_known_values() {
        assert_eq!(parse_config("mode = sidebar").mode, Mode::Sidebar);
        assert_eq!(parse_config("mode = agents-panel").mode, Mode::AgentsPanel);
        // Unknown value leaves the default untouched.
        assert_eq!(parse_config("mode = bogus").mode, Mode::AgentsPanel);
    }

    #[test]
    fn config_quotes_are_stripped_from_values() {
        assert_eq!(
            parse_config("mode = \"agents-panel\"").mode,
            Mode::AgentsPanel
        );
        assert_eq!(
            parse_config("mode = 'agents-panel'").mode,
            Mode::AgentsPanel
        );
        // Mismatched leading/trailing quotes are stripped independently.
        assert_eq!(
            parse_config("mode = \"agents-panel'").mode,
            Mode::AgentsPanel
        );
    }

    #[test]
    fn config_interval_seconds_gates_on_ge_one() {
        assert_eq!(parse_config("interval_seconds = 12").interval_seconds, 12);
        assert_eq!(parse_config("interval_seconds = \"7\"").interval_seconds, 7);
        // Below 1, zero, non-numeric, and empty-after-quotes keep the default 5.
        assert_eq!(parse_config("interval_seconds = 0").interval_seconds, 5);
        assert_eq!(parse_config("interval_seconds = -3").interval_seconds, 5);
        assert_eq!(parse_config("interval_seconds = fast").interval_seconds, 5);
    }

    #[test]
    fn config_skips_comments_and_malformed_lines() {
        let text = "\
            # mode = sidebar\n\
            not a config line\n\
            mode2 = sidebar\n\
            interval_seconds = 9\n";
        let cfg = parse_config(text);
        // The commented and digit-keyed `sidebar` lines are ignored (mode stays
        // the agents-panel default); the valid line applies.
        assert_eq!(cfg.mode, Mode::AgentsPanel);
        assert_eq!(cfg.interval_seconds, 9);
    }

    #[test]
    fn strip_quotes_matches_expected_semantics() {
        assert_eq!(strip_quotes("\"foo\""), "foo");
        assert_eq!(strip_quotes("'foo'"), "foo");
        assert_eq!(strip_quotes("\"foo"), "foo"); // leading only
        assert_eq!(strip_quotes("foo\""), "foo"); // trailing only
        assert_eq!(strip_quotes("\"foo'"), "foo"); // mismatched
        assert_eq!(strip_quotes("\""), ""); // lone quote collapses to empty
        assert_eq!(strip_quotes("bare"), "bare");
    }

    #[test]
    fn parse_kv_line_rejects_bad_keys_and_empty_values() {
        assert_eq!(parse_kv_line("mode = sidebar"), Some(("mode", "sidebar")));
        assert_eq!(parse_kv_line("  spaced  =  v  "), Some(("spaced", "v")));
        assert_eq!(parse_kv_line("mode2 = x"), None); // digit in key
        assert_eq!(parse_kv_line("a b = x"), None); // space in key
        assert_eq!(parse_kv_line("noeq"), None); // no '='
        assert_eq!(parse_kv_line("mode =   "), None); // empty value
    }
}
