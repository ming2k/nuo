use super::*;
use crate::model::document::NoticeSeverity;
use nuo_protocol::Role;

fn prose(text: &str) -> TranscriptMessage {
    TranscriptMessage::new(Role::Assistant, text)
}

fn user(text: &str) -> TranscriptMessage {
    TranscriptMessage::new(Role::User, text)
}

#[test]
fn ascii_and_unicode_case_folding_report_source_offsets() {
    assert_eq!(find_ci("Hello World", "world"), Some(6..11));
    assert_eq!(find_ci("STRAßE", "straße"), Some(0.."STRAßE".len()));
    assert_eq!(find_ci("CAFÉ au lait", "café"), Some(0.."CAFÉ".len()));
    assert_eq!(find_ci("abc", "abcd"), None);
    assert_eq!(find_ci("abc", ""), Some(0..0));
    // Folding is per-char (1:1). `İ` folds to *two* chars (`i` +
    // combining dot), so a one-char needle does not match it. That is the
    // documented limit of simple folding; what matters is that every
    // reported range is a source-byte range by construction.
    assert_eq!(find_ci("xİy", "i̇"), None);
    assert_eq!(find_ci("xİy", "y"), Some("xİ".len().."xİy".len()));
}

#[test]
fn facets_describe_entries_by_role_and_failure() {
    let reasoning = TranscriptMessage::reasoning("thinking about it");
    assert!(reasoning.facets().contains(Facets::THOUGHT));
    assert!(!reasoning.facets().contains(Facets::TEXT));

    let notice = TranscriptMessage::notice(NoticeSeverity::Error, "boom");
    assert!(notice.facets().contains(Facets::NOTICE));
    assert!(notice.facets().contains(Facets::ERROR));

    let info = TranscriptMessage::notice(NoticeSeverity::Info, "fyi");
    assert!(info.facets().contains(Facets::NOTICE));
    assert!(!info.facets().contains(Facets::ERROR));
}

#[test]
fn facet_choices_are_discovered_from_the_transcript() {
    let messages = vec![
        prose("hi"),
        TranscriptMessage::tool_step("1", "execute_command", "{}"),
        TranscriptMessage::tool_step("2", "read", "{}"),
        TranscriptMessage::tool_step("3", "execute_command", "{}"),
    ];
    let facets = choices(&messages);
    let tools: Vec<&str> = facets
        .iter()
        .filter_map(|facet| match facet {
            Facet::ToolNamed(name) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tools, vec!["execute_command", "read"]);
    assert_eq!(facets[0], Facet::Any);
}

#[test]
fn facet_only_query_lists_every_matching_entry() {
    let messages = vec![
        prose("one"),
        TranscriptMessage::reasoning("two"),
        prose("three"),
        TranscriptMessage::reasoning("four"),
    ];
    let hits = find(
        &messages,
        &Filter {
            facet: Facet::Thought,
            needle: String::new(),
        },
    );
    assert_eq!(
        hits.iter().map(|hit| hit.msg_idx).collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert!(hits.iter().all(|hit| hit.snippet.is_empty()));
}

#[test]
fn needle_and_facet_compose() {
    let messages = vec![
        user("deploy the thing"),
        prose("deployed successfully"),
        TranscriptMessage::reasoning("deploy needs care"),
    ];
    let every = find(
        &messages,
        &Filter {
            facet: Facet::Any,
            needle: "deploy".into(),
        },
    );
    assert_eq!(every.len(), 3);
    let only_user = find(
        &messages,
        &Filter {
            facet: Facet::User,
            needle: "deploy".into(),
        },
    );
    assert_eq!(only_user.len(), 1);
    assert_eq!(only_user[0].msg_idx, 0);
}

#[test]
fn hits_are_document_ordered_and_snippet_marks_the_match() {
    let messages = vec![
        prose("first"),
        prose("second has needle"),
        prose("needle again"),
    ];
    let hits = find(
        &messages,
        &Filter {
            facet: Facet::Any,
            needle: "needle".into(),
        },
    );
    assert_eq!(
        hits.iter().map(|hit| hit.msg_idx).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(hits[0].snippet.matched, "needle");
    assert_eq!(hits[0].snippet.lead, "second has ");
    assert_eq!(hits[0].snippet.tail, "");
    assert_eq!(hits[1].line, 1);
}

#[test]
fn snippet_reports_the_line_of_a_multi_line_match() {
    let messages = vec![prose("alpha\nbeta\ngamma needle here\n")];
    let hits = find(
        &messages,
        &Filter {
            facet: Facet::Any,
            needle: "needle".into(),
        },
    );
    assert_eq!(hits[0].line, 3);
    assert_eq!(hits[0].field, Field::Prose);
}

#[test]
fn a_long_line_is_clamped_with_ellipses() {
    let long = format!("{}needle{}", "a".repeat(80), "b".repeat(80));
    let messages = vec![prose(&long)];
    let hits = find(
        &messages,
        &Filter {
            facet: Facet::Any,
            needle: "needle".into(),
        },
    );
    let snippet = &hits[0].snippet;
    assert!(snippet.lead.starts_with('…'), "{:?}", snippet.lead);
    assert!(snippet.tail.ends_with('…'), "{:?}", snippet.tail);
    assert_eq!(snippet.lead.chars().count(), SNIPPET_CONTEXT_CHARS + 1);
    assert_eq!(snippet.tail.chars().count(), SNIPPET_CONTEXT_CHARS + 1);
}

#[test]
fn finder_cycles_facets_wrapping_both_ways() {
    let messages = vec![prose("a"), TranscriptMessage::reasoning("b")];
    let mut finder = Finder::new();
    finder.sync(&messages, 0);
    assert_eq!(finder.facet(), &Facet::Any);
    finder.cycle_facet(-1);
    let last = finder.choices().last().cloned().expect("choices");
    assert_eq!(finder.facet(), &last);
    finder.cycle_facet(1);
    assert_eq!(finder.facet(), &Facet::Any);
}

#[test]
fn finder_steps_hits_with_wraparound() {
    let messages = vec![prose("hit one"), prose("hit two")];
    let mut finder = Finder::new();
    finder.set_needle("hit");
    finder.sync(&messages, 0);
    assert_eq!(finder.counter(), (1, 2));
    assert_eq!(finder.step(1).map(|hit| hit.msg_idx), Some(1));
    assert_eq!(finder.counter(), (2, 2));
    assert_eq!(finder.step(1).map(|hit| hit.msg_idx), Some(0));
    assert_eq!(finder.step(-1).map(|hit| hit.msg_idx), Some(1));
}

#[test]
fn filter_posture_exposes_hit_indices_and_locate_does_not() {
    let messages = vec![prose("keep"), prose("drop"), prose("keep too")];
    let mut finder = Finder::new();
    finder.set_needle("keep");
    finder.sync(&messages, 0);
    assert_eq!(finder.visible(), None);
    assert_eq!(finder.toggle_posture(), Posture::Filter);
    assert_eq!(finder.visible(), Some(&[0usize, 2][..]));
}

#[test]
fn sync_is_a_no_op_until_the_transcript_or_query_moves() {
    let mut messages = vec![prose("alpha")];
    let mut finder = Finder::new();
    assert!(finder.sync(&messages, 0));
    assert!(!finder.sync(&messages, 0), "unchanged transcript re-scans");
    messages.push(prose("beta"));
    assert!(finder.sync(&messages, 0), "append must re-scan");
    messages[0].bump_rev();
    assert!(finder.sync(&messages, 0), "in-place mutation must re-scan");
    assert!(!finder.sync(&messages, 0));
    assert!(finder.sync(&messages, 1), "a new view must re-scan");
}

#[test]
fn an_empty_result_set_still_settles() {
    let messages = vec![prose("alpha")];
    let mut finder = Finder::new();
    finder.set_needle("nothing here");
    assert!(finder.sync(&messages, 0));
    assert_eq!(finder.counter(), (0, 0));
    assert!(
        !finder.sync(&messages, 0),
        "no hits is still a settled state"
    );
    assert_eq!(finder.step(1), None);
}

#[test]
fn cursor_clamps_when_the_result_set_shrinks() {
    let messages = vec![prose("hit 1"), prose("hit 2"), prose("hit 3")];
    let mut finder = Finder::new();
    finder.set_needle("hit");
    finder.sync(&messages, 0);
    finder.step(2);
    assert_eq!(finder.cursor(), 2);
    finder.set_needle("hit 1");
    finder.sync(&messages, 0);
    assert_eq!(finder.counter(), (1, 1));
    assert_eq!(finder.cursor(), 0);
}

#[test]
fn tool_output_is_searchable_where_the_match_lives() {
    let mut step = TranscriptMessage::tool_step("call", "execute_command", r#"{"command":"ls"}"#);
    assert!(step.finish_tool_step(
        "call",
        "total 0\nimportant.log\n",
        nuo_protocol::ToolOutput::Text("total 0\nimportant.log\n".into()),
        0,
    ));
    let messages = vec![step];
    let hits = find(
        &messages,
        &Filter {
            facet: Facet::Any,
            needle: "important".into(),
        },
    );
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].field, Field::ToolOutput);
    assert_eq!(hits[0].snippet.matched, "important");
    assert_eq!(hits[0].line, 2);
}

#[test]
fn a_failed_tool_step_is_selectable_as_an_error() {
    let mut step = TranscriptMessage::tool_step("call", "execute_command", "{}");
    assert!(step.finish_tool_step(
        "call",
        "boom",
        nuo_protocol::ToolOutput::Error {
            message: "boom".into(),
            detail: None,
        },
        0,
    ));
    let messages = vec![step];
    let hits = find(
        &messages,
        &Filter {
            facet: Facet::Error,
            needle: String::new(),
        },
    );
    assert_eq!(hits.len(), 1);
    // The same entry is still a tool step, so `Tool` selects it too.
    let tools = find(
        &messages,
        &Filter {
            facet: Facet::Tool,
            needle: String::new(),
        },
    );
    assert_eq!(tools.len(), 1);
}
