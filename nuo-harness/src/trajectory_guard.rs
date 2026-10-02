//! General-purpose trajectory loop guard (ADR-0247): a pre-dispatch detector that intercepts
//! tool calls matching an in-window repeating trajectory pattern before execution.
//!
//! # Architecture (ADR-0247)
//!
//! Extends the ADR-0148 deterministic signature bookkeeper into a dual-tier Trajectory Loop Guard:
//! 1. **L1 Heuristic Probe**: Watches normalized signatures across all covered tools within a sliding window.
//! 2. **L2 Cognitive Arbiter (Steward)**: When a signature trips the current threshold tier in cognitive mode,
//!    the Steward evaluates whether the repetition represents genuine non-converging paralysis or
//!    legitimate iterative progress (e.g. test-edit-test cycles, paging).
//! 3. **Escalating Backoff Ladder**: An acquittal advances the threshold (e.g. 4 -> 8 -> 12), preventing
//!    redundant reviews while preserving the safety ceiling.
//! 4. **Fail-Open Invariant (`[INV-LOOP-02]`)**: If the cognitive review times out or errors, it defaults to
//!    `NoLoop` and advances the ladder, avoiding deadlocks on infrastructure faults.

use std::collections::{HashMap, VecDeque};

use nuo_contracts::TrajectoryGuardConfig;
use serde_json::Value;

use crate::guard::GuardAction;

/// The tools this guard watches. Anything outside this set is passed through
/// untouched — MCP tools, `ask_user`, `use_skill`, `todo_*`, subagent, etc. are
/// either inherently unique or user-interactive, where a repeat is plausibly
/// legitimate and blocking would be hostile.
///
/// Kept as a sorted set so the [`covers`] lookup is O(log n).
const WATCHED_TOOLS: &[&str] = &[
    "edit_text",
    "execute_command",
    "find_files",
    "list_dir",
    "read",
    "read_image",
    "read_text",
    "read_url",
    "run_command",
    "search_text",
    "search_web",
    "write_file",
];

/// Whether a tool name is in the watched set. Case-sensitive.
pub(crate) fn covers(name: &str) -> bool {
    WATCHED_TOOLS.binary_search(&name).is_ok()
}

/// Canonical signature of a single watched tool call, normalized so that
/// semantically-identical calls share a key but genuinely-different calls do
/// not.
pub fn trajectory_signature(name: &str, args: &str) -> String {
    if !covers(name) {
        return format!("{name}|<unwatched>");
    }
    let value: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    if name == "find_files" {
        let path = value
            .get("path")
            .and_then(Value::as_str)
            .map(normalize_path_locator)
            .unwrap_or_else(|| ".".to_string());
        let patterns = {
            let s = normalized_string_array(&value, "patterns", false);
            if s.is_empty() { "*".to_string() } else { s }
        };
        return format!(
            "{name}|{path}|include={patterns}|exclude={}",
            normalized_string_array(&value, "exclude", false)
        );
    }
    if name == "search_text" {
        let query = value
            .get("query")
            .and_then(Value::as_str)
            .map(normalize_query_locator)
            .unwrap_or_default();
        let path = value
            .get("path")
            .and_then(Value::as_str)
            .map(normalize_path_locator)
            .unwrap_or_else(|| ".".to_string());
        return format!(
            "{name}|{query}|{path}|include={}|exclude={}|regex={}",
            normalized_string_array(&value, "include", false),
            normalized_string_array(&value, "exclude", false),
            value.get("regex").and_then(Value::as_bool).unwrap_or(false)
        );
    }
    if name == "read_text" || name == "read" {
        let path = value
            .get("path")
            .or_else(|| value.get("file_path"))
            .or_else(|| value.get("file"))
            .or_else(|| value.get("filename"))
            .and_then(Value::as_str)
            .map(normalize_path_locator)
            .unwrap_or_default();
        let offset = value
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1);
        let limit = value.get("limit").and_then(Value::as_u64).unwrap_or(0);
        return format!("{name}|{path}|offset={offset}|limit={limit}");
    }
    // Prefer the most specific locator present, in priority order.
    for key in ["command", "cmd"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            return format!("{name}|{}", normalize_command_locator(s));
        }
    }
    if let Some(s) = value.get("url").and_then(Value::as_str) {
        return format!("{name}|{}", normalize_query_locator(s));
    }
    for key in ["query", "pattern", "q"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            return format!("{name}|{}", normalize_query_locator(s));
        }
    }
    // Content-addressed mutations (ADR-0148): edits and writes key on path
    // plus a hash of the payload.
    if name == "edit_text" || name == "write_file" {
        let path = value
            .get("path")
            .or_else(|| value.get("file_path"))
            .or_else(|| value.get("file"))
            .and_then(Value::as_str)
            .map(normalize_path_locator)
            .unwrap_or_default();
        let mut payload = String::new();
        for key in ["old_string", "old", "new_string", "new", "content"] {
            if let Some(s) = value.get(key).and_then(Value::as_str) {
                payload.push_str(s);
                payload.push('\u{1f}');
            }
        }
        if !payload.is_empty() {
            let h = stable_hash(payload.as_bytes());
            return format!("{name}|{path}|h={h}");
        }
    }
    for key in ["path", "file_path", "file", "filename"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            return format!("{name}|{}", normalize_path_locator(s));
        }
    }
    format!("{name}|{}", args.trim())
}

/// Canonical signature normalizer for a shell command locator.
fn normalize_command_locator(raw: &str) -> String {
    let mut meaningful: Vec<String> = Vec::new();
    for segment in raw.split([';', '\n']) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let cleaned = strip_env_assignments(segment);
        if cleaned.is_empty() {
            continue;
        }
        let first = cleaned.split_whitespace().next().unwrap_or("");
        if is_noise_first_token(first) {
            continue;
        }
        let mut tokens: Vec<String> = cleaned.split_whitespace().map(str::to_string).collect();
        if let Some(first) = tokens.first_mut() {
            *first = first.to_lowercase();
        }
        meaningful.push(tokens.join(" "));
    }
    if meaningful.is_empty() {
        let first = strip_env_assignments(raw.trim())
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();
        return first;
    }
    meaningful.join("; ")
}

fn strip_env_assignments(segment: &str) -> String {
    let mut tokens: Vec<&str> = Vec::new();
    for tok in segment.split_whitespace() {
        if tokens.is_empty()
            && tok.contains('=')
            && tok.split_once('=').is_some_and(|(k, v)| {
                !k.is_empty()
                    && !v.is_empty()
                    && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        {
            continue;
        }
        tokens.push(tok);
    }
    tokens.join(" ")
}

fn is_noise_first_token(token: &str) -> bool {
    matches!(token.to_lowercase().as_str(), "sleep" | "true" | ":")
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn normalize_query_locator(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn normalize_path_locator(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    trimmed.strip_prefix("./").unwrap_or(trimmed).to_string()
}

fn normalized_string_array(value: &Value, key: &str, lowercase: bool) -> String {
    let mut strings = value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|item| {
            let item = item.trim();
            if lowercase {
                item.to_lowercase()
            } else {
                item.to_string()
            }
        })
        .collect::<Vec<_>>();
    strings.sort();
    strings.dedup();
    strings.join(",")
}

fn is_watched_sig(sig: &str) -> bool {
    !sig.ends_with("|<unwatched>")
}

/// The pre-dispatch trajectory loop detector (ADR-0247).
pub struct TrajectoryLoopGuard {
    config: TrajectoryGuardConfig,
    window: VecDeque<String>,
    /// Per-signature escalating backoff tier (ADR-0247).
    ladder_tiers: HashMap<String, usize>,
}

impl TrajectoryLoopGuard {
    /// Construct a guard tuned by `config`.
    pub fn new(config: TrajectoryGuardConfig) -> Self {
        Self {
            config,
            window: VecDeque::new(),
            ladder_tiers: HashMap::new(),
        }
    }

    /// The configured thresholds. Exposed for tests and diagnostics.
    pub fn config(&self) -> TrajectoryGuardConfig {
        self.config
    }

    /// Base occurrence tier for this guard.
    pub fn base_tier(&self) -> usize {
        self.config.threshold.max(2)
    }

    /// Current threshold tier for `signature`.
    pub fn current_tier(&self, signature: &str) -> usize {
        *self
            .ladder_tiers
            .get(signature)
            .unwrap_or(&self.base_tier())
    }

    /// Escalate the threshold tier along the backoff ladder (4 -> 8 -> 12) upon cognitive acquittal.
    pub fn acquit_and_backoff(&mut self, signature: &str) {
        let base = self.base_tier();
        let current = self.current_tier(signature);
        let next = (current + base).min(base * 3);
        self.ladder_tiers.insert(signature.to_string(), next);
    }

    /// Check if any incoming signature has tripped its current threshold tier under cognitive review mode.
    /// Returns `Some((signature, current_tier, recent_signatures))` if a candidate needs L2 arbitration.
    pub fn check_candidate(&self, signatures: &[String]) -> Option<(String, usize, Vec<String>)> {
        if !self.config.enabled || !self.config.cognitive_review {
            return None;
        }
        for sig in signatures.iter().filter(|s| is_watched_sig(s)) {
            let tier = self.current_tier(sig);
            let in_window = self.window.iter().filter(|w| *w == sig).count();
            if in_window + 1 >= tier {
                return Some((sig.clone(), tier, self.window.iter().cloned().collect()));
            }
        }
        None
    }

    /// Push signatures into the sliding window.
    pub fn push_all(&mut self, signatures: &[String]) {
        for sig in signatures.iter().filter(|s| is_watched_sig(s)) {
            self.push(sig.clone());
        }
    }

    /// Deterministic pre-dispatch check (or fallback when cognitive review is disabled).
    pub fn check_ahead(&mut self, signatures: &[String]) -> GuardAction {
        if !self.config.enabled {
            return GuardAction::Continue;
        }
        let repeated: Vec<String> = signatures
            .iter()
            .filter(|sig| is_watched_sig(sig))
            .filter(|sig| {
                let tier = self.current_tier(sig);
                self.window.iter().filter(|w| *w == *sig).count() + 1 >= tier
            })
            .cloned()
            .collect();

        for sig in signatures.iter().filter(|s| is_watched_sig(s)) {
            self.push(sig.clone());
        }

        if repeated.is_empty() {
            return GuardAction::Continue;
        }

        self.block_action(&repeated)
    }

    /// Generate a formatted [`GuardAction::Block`] for the specified signatures.
    pub fn block_action(&self, repeated: &[String]) -> GuardAction {
        let summary = repeated
            .iter()
            .map(|s| format!("- {}", humanize_sig(s)))
            .collect::<Vec<_>>()
            .join("\n");
        let message = format!(
            "You are repeating a tool call that already ran this round:\n{summary}\n\
             Re-running it cannot change the result you already have. This call is now \
             **blocked** for the rest of the turn — calling it again returns an error, \
             not a fresh result. Act on what you already have, try a *different* \
             command/file/query, or, if you genuinely cannot proceed, say so explicitly \
             or call `abort`."
        );
        GuardAction::Block {
            signatures: repeated.to_vec(),
            message,
        }
    }

    fn push(&mut self, signature: String) {
        self.window.push_back(signature);
        while self.window.len() > self.config.window {
            #[allow(clippy::expect_used)]
            self.window
                .pop_front()
                .expect("non-empty while over window");
        }
    }
}

/// Reduce a machine signature (`name|locator`) to a short human phrase for the
/// block message, e.g. `bash ls -la`, `read_text src/main.rs`.
pub fn humanize_sig(signature: &str) -> String {
    let parts: Vec<&str> = signature.split('|').collect();
    if (parts.first() == Some(&"read_text") || parts.first() == Some(&"read")) && parts.len() == 4 {
        let name = parts[0];
        let path = parts[1];
        let offset = parts[2].strip_prefix("offset=").unwrap_or("1");
        let limit = parts[3].strip_prefix("limit=").unwrap_or("0");
        if offset == "1" && limit == "0" {
            format!("{name} {path}")
        } else if limit == "0" {
            format!("{name} {path} :{offset},$")
        } else {
            format!("{name} {path} :{offset},limit={limit}")
        }
    } else {
        let mut parts = signature.splitn(2, '|');
        let name = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("");
        if rest.is_empty() || rest == "<unwatched>" {
            name.to_string()
        } else {
            format!("{name} {rest}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> TrajectoryGuardConfig {
        TrajectoryGuardConfig {
            enabled: true,
            ..TrajectoryGuardConfig::default()
        }
    }

    fn strict() -> TrajectoryGuardConfig {
        TrajectoryGuardConfig {
            enabled: true,
            threshold: 2,
            ..TrajectoryGuardConfig::default()
        }
    }

    #[test]
    fn covers_the_watched_set() {
        assert!(covers("execute_command"));
        assert!(covers("read_text"));
        assert!(covers("write_file"));
        assert!(!covers("use_skill"));
        assert!(!covers("ask_user"));
        assert!(!covers("mcp_tool"));
    }

    #[test]
    fn first_occurrence_is_allowed() {
        let mut g = TrajectoryLoopGuard::new(enabled());
        let action = g.check_ahead(&[trajectory_signature(
            "execute_command",
            r#"{"command":"ls"}"#,
        )]);
        assert_eq!(action, GuardAction::Continue);
    }

    #[test]
    fn occurrences_tolerated_until_threshold_blocks() {
        let mut g = TrajectoryLoopGuard::new(enabled());
        let s = trajectory_signature("execute_command", r#"{"command":"make test"}"#);
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
        let blocked = g.check_ahead(std::slice::from_ref(&s));
        match blocked {
            GuardAction::Block {
                signatures,
                message,
            } => {
                assert_eq!(signatures, vec![s]);
                assert!(message.contains("make test"));
                assert!(message.contains("**blocked** for the rest of the turn"));
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn backoff_ladder_escalates_on_acquittal() {
        let mut g = TrajectoryLoopGuard::new(TrajectoryGuardConfig::cognitive());
        assert_eq!(g.base_tier(), 4);
        let s = "execute_command|cargo test".to_string();

        assert_eq!(g.current_tier(&s), 4);
        g.acquit_and_backoff(&s);
        assert_eq!(g.current_tier(&s), 8);
        g.acquit_and_backoff(&s);
        assert_eq!(g.current_tier(&s), 12);
        // Capped at 12 (3 * base)
        g.acquit_and_backoff(&s);
        assert_eq!(g.current_tier(&s), 12);
    }

    #[test]
    fn strict_threshold_two_blocks_on_first_repeat() {
        let mut g = TrajectoryLoopGuard::new(strict());
        let s = trajectory_signature("execute_command", r#"{"command":"ls"}"#);
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
        match g.check_ahead(std::slice::from_ref(&s)) {
            GuardAction::Block { signatures, .. } => assert_eq!(signatures, vec![s]),
            other => panic!("expected Block on second call, got {other:?}"),
        }
    }

    #[test]
    fn disabled_guard_never_blocks() {
        let mut g = TrajectoryLoopGuard::new(TrajectoryGuardConfig::disabled());
        let s = trajectory_signature("execute_command", r#"{"command":"ls"}"#);
        for _ in 0..10 {
            assert_eq!(
                g.check_ahead(std::slice::from_ref(&s)),
                GuardAction::Continue
            );
        }
    }

    #[test]
    fn distinct_commands_do_not_collide() {
        let mut g = TrajectoryLoopGuard::new(enabled());
        let s1 = trajectory_signature("execute_command", r#"{"command":"cargo test"}"#);
        let s2 = trajectory_signature("execute_command", r#"{"command":"cargo build"}"#);
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s1)),
            GuardAction::Continue
        );
        assert_eq!(g.check_ahead(&[s2]), GuardAction::Continue);
        assert_eq!(g.check_ahead(&[s1]), GuardAction::Continue);
    }

    #[test]
    fn bare_sleep_variants_collide() {
        let s1 = trajectory_signature("execute_command", r#"{"command":"sleep 5"}"#);
        let s2 = trajectory_signature("execute_command", r#"{"command":"sleep 9"}"#);
        assert_eq!(s1, s2, "bare sleep noise variants must share a signature");
    }

    #[test]
    fn sleep_noise_variants_collide() {
        let s1 = trajectory_signature("execute_command", r#"{"command":"sleep 5; make test"}"#);
        let s2 = trajectory_signature("execute_command", r#"{"command":"sleep 9; make test"}"#);
        assert_eq!(
            s1, s2,
            "sleep noise prefix must be stripped so variants collide"
        );
    }

    #[test]
    fn env_assignment_prefixes_collide() {
        let s1 = trajectory_signature("execute_command", r#"{"command":"FOO=1 make test"}"#);
        let s2 = trajectory_signature("execute_command", r#"{"command":"make test"}"#);
        assert_eq!(s1, s2, "leading env assignment must be stripped");
    }

    #[test]
    fn program_casing_collides_but_arguments_do_not() {
        let s1 = trajectory_signature("execute_command", r#"{"command":"Bash -c ls"}"#);
        let s2 = trajectory_signature("execute_command", r#"{"command":"bash -c ls"}"#);
        assert_eq!(s1, s2, "program name casing must be normalized");

        let s3 = trajectory_signature("execute_command", r#"{"command":"bash -c LS"}"#);
        assert_ne!(s1, s3, "command arguments casing must be preserved");
    }

    #[test]
    fn read_same_range_collides_and_normalizes_defaults() {
        let s1 = trajectory_signature("read_text", r#"{"path":"foo.rs"}"#);
        let s2 = trajectory_signature("read_text", r#"{"path":"foo.rs","offset":1}"#);
        let s3 = trajectory_signature("read_text", r#"{"path":"foo.rs","offset":1,"limit":0}"#);
        assert_eq!(s1, s2);
        assert_eq!(s2, s3);
    }

    #[test]
    fn read_distinct_ranges_do_not_collide() {
        let s1 = trajectory_signature("read_text", r#"{"path":"foo.rs","offset":1,"limit":50}"#);
        let s2 = trajectory_signature("read_text", r#"{"path":"foo.rs","offset":51,"limit":50}"#);
        assert_ne!(s1, s2, "different read ranges must not collide");
    }

    #[test]
    fn exact_edit_thrash_collides_distinct_edits_do_not() {
        let e1 = trajectory_signature(
            "edit_text",
            r#"{"path":"foo.rs","old_string":"a","new_string":"b"}"#,
        );
        let e2 = trajectory_signature(
            "edit_text",
            r#"{"path":"foo.rs","old_string":"c","new_string":"d"}"#,
        );
        assert_ne!(e1, e2, "distinct edits to the same file must not collide");

        let e3 = trajectory_signature(
            "edit_text",
            r#"{"path":"foo.rs","old_string":"a","new_string":"b"}"#,
        );
        assert_eq!(e1, e3, "identical payload edit must collide");
    }

    #[test]
    fn write_content_hash_keys_the_payload() {
        let w1 = trajectory_signature("write_file", r#"{"path":"foo.rs","content":"hello"}"#);
        let w2 = trajectory_signature("write_file", r#"{"path":"foo.rs","content":"world"}"#);
        assert_ne!(w1, w2, "distinct writes must not collide");

        let w3 = trajectory_signature("write_file", r#"{"path":"foo.rs","content":"hello"}"#);
        assert_eq!(w1, w3, "identical write content must collide");
    }

    #[test]
    fn path_decorations_collide() {
        let s1 = trajectory_signature("read_text", r#"{"path":"./src/main.rs"}"#);
        let s2 = trajectory_signature("read_text", r#"{"path":"src/main.rs"}"#);
        let s3 = trajectory_signature("read_text", r#"{"path":"src/main.rs/"}"#);
        assert_eq!(s1, s2);
        assert_eq!(s2, s3);
    }

    #[test]
    fn query_casing_and_spacing_collide() {
        let s1 = trajectory_signature("search_text", r#"{"query":"fn foo","path":"src"}"#);
        let s2 = trajectory_signature("search_text", r#"{"query":"  FN   FOO  ","path":"./src/"}"#);
        assert_eq!(s1, s2);
    }

    #[test]
    fn file_pattern_order_does_not_change_search_intent() {
        let s1 = trajectory_signature("find_files", r#"{"patterns":["*.rs","*.toml"]}"#);
        let s2 = trajectory_signature("find_files", r#"{"patterns":["*.toml","*.rs"]}"#);
        assert_eq!(s1, s2);
    }

    #[test]
    fn unwatched_tool_signatures_do_not_block() {
        let mut g = TrajectoryLoopGuard::new(strict());
        let s = trajectory_signature("ask_user", r#"{"question":"ready?"}"#);
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
    }

    #[test]
    fn window_evicts_old_signatures() {
        let cfg = TrajectoryGuardConfig {
            enabled: true,
            window: 2,
            threshold: 2,
            cognitive_review: false,
        };
        let mut g = TrajectoryLoopGuard::new(cfg);
        let a = trajectory_signature("execute_command", r#"{"command":"cmd_a"}"#);
        let b = trajectory_signature("execute_command", r#"{"command":"cmd_b"}"#);
        let c = trajectory_signature("execute_command", r#"{"command":"cmd_c"}"#);

        assert_eq!(
            g.check_ahead(std::slice::from_ref(&a)),
            GuardAction::Continue
        );
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&b)),
            GuardAction::Continue
        );
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&c)),
            GuardAction::Continue
        );
        // `a` has aged out of window of size 2
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&a)),
            GuardAction::Continue
        );
    }

    #[test]
    fn humanize_sig_formats_locators() {
        assert_eq!(
            humanize_sig("execute_command|cargo test"),
            "execute_command cargo test"
        );
        assert_eq!(
            humanize_sig("read_text|src/main.rs|offset=1|limit=0"),
            "read_text src/main.rs"
        );
        assert_eq!(
            humanize_sig("read_text|src/main.rs|offset=20|limit=0"),
            "read_text src/main.rs :20,$"
        );
        assert_eq!(
            humanize_sig("read_text|src/main.rs|offset=20|limit=50"),
            "read_text src/main.rs :20,limit=50"
        );
    }
}
