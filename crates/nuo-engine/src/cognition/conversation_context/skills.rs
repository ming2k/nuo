//! Implicit skill context injected when the latest visible user text names a skill.

use std::collections::HashSet;

use nuo_tool::{Tag, parse_envelope};

use crate::{InjectionKind, Message, Role};

/// How many visible user messages the mention scan looks back over.
///
/// The mention grammar is explicit user intent, so it lives in recent input;
/// older mentions have already produced their `<skill>` envelope, which the
/// full-history `already_loaded` set below still honors.
const MENTION_SCAN_WINDOW: usize = 32;

pub(crate) fn inject_mentioned_skills(
    registry: &crate::cognition::skills::SkillRegistry,
    messages: &mut Vec<Message>,
) {
    // Fast pre-check before building any joined text: mentions have an
    // explicit grammar (`@name`, `@skill:name`, `skill://…`), and most turns
    // contain none of it. Scanning for the trigger characters first keeps the
    // common path O(recent user chars) without a full-history join — the old
    // unconditional join was O(total transcript chars) per call, and this
    // runs multiple times per ReAct turn (once per model_request/estimate).
    let recent_user_texts: Vec<&str> = messages
        .iter()
        .filter(|message| message.role == Role::User && !message.hidden)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    let window_start = recent_user_texts.len().saturating_sub(MENTION_SCAN_WINDOW);
    let scan_slice = &recent_user_texts[window_start..];
    let mentions_present = scan_slice
        .iter()
        .any(|t| t.contains('@') || t.contains("skill://"));
    if !mentions_present {
        return;
    }
    let text = scan_slice.join("\n");
    if text.is_empty() {
        return;
    }

    let already_loaded: HashSet<String> = messages
        .iter()
        .filter(|message| message.role == Role::User && message.hidden)
        .filter_map(|message| loaded_skill_name(&message.content))
        .collect();

    let mentioned: Vec<crate::cognition::skills::Skill> = {
        let registry = registry.lock();
        registry
            .resolve_mentions(&text)
            .into_iter()
            .filter(|skill| !already_loaded.contains(&skill.name))
            .collect()
    };

    for skill in mentioned {
        // Bodies are loaded lazily and cached on first use.
        let Some(Ok(content)) = registry.body_for(&skill.name) else {
            continue;
        };
        let formatted = crate::cognition::skills::render::render_skill_envelope(&skill, &content);
        messages.push(super::hidden_user_with_reason(
            InjectionKind::ImplicitSkill,
            &skill.name,
            formatted,
        ));
    }
}

/// The skill name carried by an already-injected skill envelope.
///
/// Reads the envelope's `name` attribute through the shared parser rather than
/// searching for a `<skill name="` literal: de-duplication must survive a change
/// to the envelope's spelling (ADR-0226).
///
/// There is deliberately no legacy `[Skill '…' loaded]` branch. The format was
/// replaced without an alias; a session file from before the change re-injects
/// its skill once and then de-duplicates normally, which is the correct trade
/// for not carrying two parsers forever.
pub(crate) fn loaded_skill_name(content: &str) -> Option<String> {
    let parsed = parse_envelope(content)?;
    if parsed.tag != Tag::Skill {
        return None;
    }
    parsed.attr("name").map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::message::hidden_user_with_reason;
    use crate::{Message, Role};

    fn registry_with_skill(name: &str) -> crate::cognition::skills::SkillRegistry {
        // Minimal registry whose single skill matches `@name` mentions:
        // build via serde defaults (all fields default sensibly, implicit
        // invocation allowed by the default policy), then `replace` it into
        // an empty registry — the same public surface the agent path uses.
        let mut skill: crate::cognition::skills::Skill =
            serde_json::from_value(serde_json::json!({ "name": name, "description": "d", "scope": "User", "source": "/nonexistent-skill.md", "root": ".", "content": "", "version": null, "policy": { "allow_implicit_invocation": true } }))
                .unwrap();
        skill.policy.allow_implicit_invocation = true;
        let registry = crate::cognition::skills::SkillRegistry::empty();
        registry.replace(vec![skill]);
        registry
    }

    fn injected_skill(name: &str) -> String {
        nuo_tool::Envelope::new(Tag::Skill)
            .attr("name", name)
            .attr("scope", "user")
            .body("body")
            .render()
    }

    /// A `@name` mention in the latest user message loads the skill.
    #[test]
    fn mention_in_recent_message_loads_skill() {
        let registry = registry_with_skill("rust-expert");
        let mut messages = vec![Message::new(Role::User, "please use @rust-expert here")];
        inject_mentioned_skills(&registry, &mut messages);
        let injected = messages
            .iter()
            .find(|m| m.hidden)
            .expect("a hidden skill message");
        assert_eq!(
            loaded_skill_name(&injected.content).as_deref(),
            Some("rust-expert")
        );
    }

    /// The reader understands the envelope the writer produces, and nothing
    /// else: a legacy marker is no longer recognized.
    #[test]
    fn the_reader_matches_the_writer_and_rejects_legacy_markers() {
        assert_eq!(
            loaded_skill_name(&injected_skill("rust-expert")),
            Some("rust-expert".to_string())
        );
        assert_eq!(loaded_skill_name("[Skill 'rust-expert' loaded]"), None);
        assert_eq!(loaded_skill_name("plain message"), None);
        // A different envelope tag is not a skill.
        assert_eq!(
            loaded_skill_name("<file ref=\"@file:a.rs\"/>"),
            None,
            "only a skill envelope names a loaded skill"
        );
    }

    /// Already loaded skills are not re-injected.
    #[test]
    fn already_loaded_skills_are_not_re_injected() {
        let registry = registry_with_skill("rust-expert");
        let mut messages = vec![
            hidden_user_with_reason(
                InjectionKind::ImplicitSkill,
                "rust-expert",
                injected_skill("rust-expert"),
            ),
            Message::new(Role::User, "please use @rust-expert again"),
        ];
        inject_mentioned_skills(&registry, &mut messages);
        assert_eq!(messages.len(), 2, "skill must not be duplicated");
    }

    /// Text with no mention grammar (`@`, `skill://`) exits before any
    /// matching — the common fast path.
    #[test]
    fn plain_history_loads_nothing() {
        let registry = registry_with_skill("rust-expert");
        let mut messages = vec![Message::new(Role::User, "just a normal prompt")];
        inject_mentioned_skills(&registry, &mut messages);
        assert_eq!(messages.len(), 1, "nothing injected");
    }
}
