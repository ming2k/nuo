use super::*;

fn snapshot(
    mcp: WorkspaceTrustState,
    skills: WorkspaceTrustState,
    hooks: WorkspaceTrustState,
    instructions: WorkspaceTrustState,
) -> WorkspaceSecuritySnapshot {
    WorkspaceSecuritySnapshot {
        root: "/tmp/proj".to_string(),
        mcp,
        skills,
        hooks,
        instructions,
        ex_workspace: WorkspaceTrustState::Absent,
        user_assets: WorkspaceTrustState::Absent,
    }
}

#[test]
fn absent_workspace_gates_nothing() {
    assert!(
        gate_request(&snapshot(
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Absent
        ))
        .is_none()
    );
}

#[test]
fn trusted_and_denied_do_not_gate() {
    assert!(
        gate_request(&snapshot(
            WorkspaceTrustState::Trusted,
            WorkspaceTrustState::Trusted,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Trusted
        ))
        .is_none()
    );
    // Explicitly denied domains do not gate again (zero-nagging).
    assert!(
        gate_request(&snapshot(
            WorkspaceTrustState::Denied,
            WorkspaceTrustState::Trusted,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Denied
        ))
        .is_none()
    );
}

#[test]
fn every_reviewable_state_has_a_status_and_every_settled_state_has_none() {
    // Total over the state enum: a seventh state cannot compile until its
    // gate status — and therefore its position in the vocabulary — is chosen.
    for state in [
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Quarantined,
        WorkspaceTrustState::Trusted,
        WorkspaceTrustState::Denied,
        WorkspaceTrustState::Changed,
        WorkspaceTrustState::Expired,
    ] {
        let status = TrustStatus::of(state);
        assert_eq!(
            status.is_some(),
            state.needs_review(),
            "{} must{} have a gate status",
            state.as_str(),
            if state.needs_review() { "" } else { " not" }
        );
    }
}

#[test]
fn changed_domains_gate_exactly_like_never_trusted_domains() {
    // The regression this pins. `Changed` means "the content you approved
    // is not the content on disk" — the strongest reason to ask a human,
    // and previously the only one that did *not* gate. It escalated to a
    // transient banner instead, which re-fired on every attach and could be
    // scrolled past, so the highest-risk state had the weakest affordance.
    let changed = gate_request(&snapshot(
        WorkspaceTrustState::Changed,
        WorkspaceTrustState::Changed,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Changed,
    ))
    .expect("changed content must gate: the earlier approval was for other bytes");

    let quarantined = gate_request(&snapshot(
        WorkspaceTrustState::Quarantined,
        WorkspaceTrustState::Quarantined,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Quarantined,
    ))
    .expect("never-trusted content must gate");

    // Same gate, same assets, same options — only the status differs. The
    // option label is the asset name in both cases, so a re-review is told
    // apart by the status column, never by syntax grafted onto the label.
    let labels = |r: &UserQuestionRequest| {
        r.questions
            .first()
            .expect("the gate asks one question")
            .options
            .iter()
            .map(|o| o.label.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(labels(&changed), labels(&quarantined));
    assert!(
        changed
            .questions
            .first()
            .expect("one question")
            .options
            .iter()
            .all(|o| !o.label.contains('[') && !o.label.contains('|')),
        "an option label carries no status syntax: {:?}",
        labels(&changed)
    );
}

#[test]
fn the_label_is_the_plain_asset_name() {
    let req = gate_request(&snapshot(
        WorkspaceTrustState::Quarantined,
        WorkspaceTrustState::Quarantined,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Quarantined,
    ))
    .expect("quarantined workspace must gate");
    let labels: Vec<&str> = req
        .questions
        .first()
        .expect("one question")
        .options
        .iter()
        .map(|o| o.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec![".nuo/mcp.json", ".nuo/skills/", "Project instructions"],
        "the label names the asset and nothing else"
    );
    // The row's status is a value on the view, not a substring to parse.
    let rows = domain_rows(&snapshot(
        WorkspaceTrustState::Quarantined,
        WorkspaceTrustState::Quarantined,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Quarantined,
    ));
    assert_eq!(
        rows.iter().map(|r| r.status.token()).collect::<Vec<_>>(),
        vec!["new", "new", "new"]
    );
}

#[test]
fn status_tokens_are_lowercase_unbracketed_words() {
    // The state-token rule from the visual language reference. A bracketed
    // token here is indistinguishable from the `[ROLE]` badge and from a
    // `[N queued]` count, which are the two other things brackets mean.
    for status in [TrustStatus::New, TrustStatus::Changed, TrustStatus::Expired] {
        let token = status.token();
        assert_eq!(token, token.to_lowercase(), "{token} must be lowercase");
        assert!(
            !token.contains(['[', ']', '(', ')', '·', '|']),
            "{token} must be one bare word"
        );
        assert!(!token.contains(' '), "{token} must be one word");
    }
}

#[test]
fn a_re_review_is_the_loudest_status_and_routine_ones_are_quiet() {
    // The severity ladder, in the palette's own tones. `changed` is the one
    // status that means the approval no longer describes the bytes, so it
    // takes the warning tone; a lapsed lease and a first look must not
    // compete with it.
    let theme = Theme::default();
    assert_eq!(TrustStatus::Changed.tone(&theme), theme.warn());
    assert_eq!(TrustStatus::New.tone(&theme), theme.muted());
    assert_eq!(TrustStatus::Expired.tone(&theme), theme.dim());
    assert_ne!(
        TrustStatus::Changed.tone(&theme),
        TrustStatus::New.tone(&theme),
        "a re-review must not look like a first contact"
    );
}

#[test]
fn expired_domains_gate_for_routine_re_attestation() {
    let rows = domain_rows(&snapshot(
        WorkspaceTrustState::Expired,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Absent,
    ));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, TrustStatus::Expired);
}

#[test]
fn the_headline_names_the_reason_and_not_the_assets() {
    // A first contact and a re-review are different questions with the same
    // assets; the headline states which one is being asked. The assets are
    // the rows' job — naming them here was a second copy of the same list,
    // and the copy that had to break words to fit.
    let changed = gate_request(&snapshot(
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Changed,
    ))
    .expect("changed instructions must gate");
    let question = &changed.questions.first().expect("one question").question;
    assert!(
        question.contains("changed on disk"),
        "a re-review must say the bytes moved: {question}"
    );
    assert!(
        !question.contains("has not been reviewed"),
        "a re-review is not a first contact: {question}"
    );
    assert!(
        !question.contains("Project instructions"),
        "the asset is named on its row, not in the headline: {question}"
    );

    let fresh = gate_request(&snapshot(
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Quarantined,
    ))
    .expect("never-trusted instructions must gate");
    let question = &fresh.questions.first().expect("one question").question;
    assert!(
        question.contains("has not been reviewed"),
        "a first contact must say so: {question}"
    );
    assert!(
        !question.contains("changed on disk"),
        "a first contact is not a re-review: {question}"
    );
}

#[test]
fn a_mixed_gate_states_every_reason_that_applies() {
    // `Changed` and `New` are different questions; a snapshot carrying both
    // must not be described as only one of them.
    let mixed = gate_request(&snapshot(
        WorkspaceTrustState::Trusted,
        WorkspaceTrustState::Quarantined,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Changed,
    ))
    .expect("mixed review must gate");
    let question = &mixed.questions.first().expect("one question").question;
    assert!(question.contains("changed on disk"), "{question}");
    assert!(question.contains("has not been reviewed"), "{question}");
}

#[test]
fn the_consequence_is_kept_out_of_the_question_body() {
    // The surface paints it in its own (secondary) tone beneath the rows.
    // Folding it into the question string forced it to share the body's
    // full-strength tone, which made the least important sentence the
    // loudest thing on a security prompt.
    let req = gate_request(&snapshot(
        WorkspaceTrustState::Quarantined,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Absent,
        WorkspaceTrustState::Absent,
    ))
    .expect("quarantined workspace must gate");
    assert!(
        !req.questions
            .first()
            .expect("one question")
            .question
            .contains(GATE_CONSEQUENCE),
        "the consequence is painted by the surface, not appended to the question"
    );
    assert!(GATE_CONSEQUENCE.contains("until it changes"));
}

#[test]
fn answers_map_to_domains_through_the_offered_set() {
    let offered = [
        TrustDomain::Mcp,
        TrustDomain::Skills,
        TrustDomain::Instructions,
    ];
    let domains = answer_to_domains(
        &[vec![
            "Project instructions".to_string(),
            ".nuo/mcp.json".to_string(),
        ]],
        &offered,
    );
    assert_eq!(domains, vec![TrustDomain::Mcp, TrustDomain::Instructions]);
}

#[test]
fn an_answer_naming_an_unoffered_asset_grants_nothing() {
    // The inversion this closes: a label the gate never showed cannot
    // become authority. The old map matched path strings against a hardcoded
    // table, so any label spelling one of those paths resolved — whether or
    // not that domain was on screen.
    let offered = [TrustDomain::Skills];
    let domains = answer_to_domains(&[vec![".nuo/mcp.json".to_string()]], &offered);
    assert!(
        domains.is_empty(),
        "an unoffered asset must not be granted: {domains:?}"
    );
}

#[test]
fn every_offered_asset_round_trips_through_its_label() {
    let offered = TrustDomain::ALL;
    let labels: Vec<Vec<String>> = vec![
        offered
            .iter()
            .map(|domain| domain.label().to_string())
            .collect(),
    ];
    assert_eq!(answer_to_domains(&labels, &offered), offered.to_vec());
    // And every label is distinct, which is what makes the mapping total.
    let mut unique: Vec<&str> = offered.iter().map(|d| d.label()).collect();
    let total = unique.len();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), total, "labels must be injective");
}

#[test]
fn an_empty_answer_selects_nothing() {
    assert!(answer_to_domains(&[], &TrustDomain::ALL).is_empty());
    assert!(answer_to_domains(&[Vec::new()], &TrustDomain::ALL).is_empty());
}

#[test]
fn user_assets_quarantine_triggers_gate_in_workspace_free() {
    let mut snap = WorkspaceSecuritySnapshot::new("workspace-free");
    snap.user_assets = WorkspaceTrustState::Quarantined;
    let req = gate_request(&snap).expect("untrusted user assets must trigger gate");
    let option = req
        .questions
        .first()
        .expect("one question")
        .options
        .first()
        .expect("one option");
    assert_eq!(option.label, "~/.config/nuo/config.toml");
}
