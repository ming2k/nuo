//! Implicit skill context injected when the latest visible user text names a skill.

use std::collections::HashSet;

use crate::{InjectionKind, Message, Role};

pub(crate) fn inject_mentioned_skills(
    registry: &crate::skills::SkillRegistry,
    messages: &mut Vec<Message>,
) {
    // Fast pre-check before building any joined text: mentions have an
    // explicit grammar (`@name`, `@skill:name`, `skill://…`), and most turns
    // contain none of it. Scanning for the trigger characters first keeps the
    // common path O(recent user chars) without a full-history join — the old
    // unconditional join was O(total transcript chars) per call, and this
    // runs multiple times per ReAct turn (once per model_request/estimate).
    //
    // The mention scan itself is windowed to the most recent
    // [`MENTION_SCAN_WINDOW`] *visible user* messages: the mention grammar
    // is explicit user intent, so it lives in recent input; older mentions
    // have already produced their `<skill name="…">` envelope, which the
    // full-history `already_loaded` set below still honors (hidden markers
    // are few, so that scan stays cheap).
    const MENTION_SCAN_WINDOW: usize = 32;
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
        .filter_map(|message| extract_loaded_skill_name(&message.content))
        .collect();

    let mentioned: Vec<crate::skills::Skill> = {
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
        let formatted = crate::skills::render::format_skill_injection(&skill, &content);
        messages.push(super::hidden_user_with_reason(
            InjectionKind::ImplicitSkill,
            &skill.name,
            formatted,
        ));
    }
}

/// Extract the skill name from a previously loaded skill message.
///
/// Recognizes both the structured XML envelope (`<skill name="...`) and the
/// legacy marker (`[Skill '...' loaded]`) so durable session history from prior
/// versions remains deduplicated.
pub(crate) fn extract_loaded_skill_name(content: &str) -> Option<String> {
    if let Some(start) = content.find("<skill name=\"") {
        let after = &content[start + "<skill name=\"".len()..];
        let end = after.find('"')?;
        return Some(after[..end].to_string());
    }
    let prefix = "[Skill '";
    let start = content.find(prefix)? + prefix.len();
    let end = content[start..].find("' loaded]")?;
    Some(content[start..start + end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_context::hidden_user_with_reason;
    use crate::{Message, Role};

    fn registry_with_skill(name: &str) -> crate::skills::SkillRegistry {
        // Minimal registry whose single skill matches `@name` mentions:
        // build via serde defaults (all fields default sensibly, implicit
        // invocation allowed by the default policy), then `replace` it into
        // an empty registry — the same public surface the agent path uses.
        let mut skill: crate::skills::Skill =
            serde_json::from_value(serde_json::json!({ "name": name, "description": "d", "scope": "User", "source": "/nonexistent-skill.md", "root": ".", "content": "", "version": null, "policy": { "allow_implicit_invocation": true } }))
                .unwrap();
        skill.policy.allow_implicit_invocation = true;
        let registry = crate::skills::SkillRegistry::empty();
        registry.replace(vec![skill]);
        registry
    }

    /// A `@name` mention in the latest user message loads the skill.
    #[test]
    fn mention_in_recent_message_loads_skill() {
        let registry = registry_with_skill("rust-expert");
        let mut messages = vec![Message::new(Role::User, "please use @rust-expert here")];
        inject_mentioned_skills(&registry, &mut messages);
        assert!(
            messages.iter().any(|m| m.hidden
                && m.content.contains("<skill name=\"rust-expert\"")
                && m.content.contains("<system_guidance>")),
            "mention must inject the skill envelope with system guidance"
        );
    }

    /// Extractor correctly identifies both modern XML and legacy markers.
    #[test]
    fn extracts_skill_name_from_modern_and_legacy_envelopes() {
        let modern = "<skill name=\"rust-expert\" scope=\"repo\">\n<system_guidance>...</system_guidance>\n<instructions>...</instructions>\n</skill>";
        assert_eq!(
            extract_loaded_skill_name(modern),
            Some("rust-expert".to_string())
        );

        let legacy = "[Skill 'rust-expert' loaded]\nbody\n[/Skill]";
        assert_eq!(
            extract_loaded_skill_name(legacy),
            Some("rust-expert".to_string())
        );

        assert_eq!(extract_loaded_skill_name("plain message"), None);
    }

    /// Already loaded skills (both modern and legacy) are not re-injected.
    #[test]
    fn already_loaded_skills_are_not_re_injected() {
        let registry = registry_with_skill("rust-expert");
        let mut messages = vec![
            hidden_user_with_reason(
                InjectionKind::ImplicitSkill,
                "rust-expert",
                "<skill name=\"rust-expert\" scope=\"user\"><instructions></instructions></skill>",
            ),
            Message::new(Role::User, "please use @rust-expert again"),
        ];
        inject_mentioned_skills(&registry, &mut messages);
        assert_eq!(messages.len(), 2, "skill must not be duplicated");

        // Legacy format deduplication check
        let mut messages_legacy = vec![
            hidden_user_with_reason(
                InjectionKind::ImplicitSkill,
                "rust-expert",
                "[Skill 'rust-expert' loaded]\nbody\n[/Skill]",
            ),
            Message::new(Role::User, "please use @rust-expert again"),
        ];
        inject_mentioned_skills(&registry, &mut messages_legacy);
        assert_eq!(
            messages_legacy.len(),
            2,
            "legacy skill must not be duplicated"
        );
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
