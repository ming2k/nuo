use super::*;

#[test]
fn domain_content_is_quarantined_then_invalidated_by_change() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
    assert_eq!(store.snapshot(root).skills, WorkspaceTrustState::Absent);

    std::fs::create_dir_all(root.join(".nuo/skills/demo")).unwrap();
    std::fs::write(root.join(".nuo/skills/demo/SKILL.md"), "one").unwrap();
    assert_eq!(
        store.snapshot(root).skills,
        WorkspaceTrustState::Quarantined
    );
    assert!(store.trust_domain(root, TrustDomain::Skills).unwrap());
    assert_eq!(store.snapshot(root).skills, WorkspaceTrustState::Trusted);

    std::fs::write(root.join(".nuo/skills/demo/SKILL.md"), "two").unwrap();
    assert_eq!(store.snapshot(root).skills, WorkspaceTrustState::Changed);
}

#[test]
fn grant_then_refuse_leaves_each_domain_with_its_own_decision() {
    // Models the two driver branches (`TrustDecision::Grant` vs `Deny`) at
    // the store level, so the split is pinned where it is cheap to assert.
    //
    // Grant path: trust the selected, deny the rest (candidates \ selected).
    // Deny path: deny exactly what was named, touching nothing else.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));

    std::fs::write(root.join("AGENTS.md"), "agents").unwrap();
    std::fs::create_dir_all(root.join(".nuo/skills/demo")).unwrap();
    std::fs::write(root.join(".nuo/skills/demo/SKILL.md"), "skill").unwrap();
    std::fs::create_dir_all(root.join(".nuo/hooks")).unwrap();
    std::fs::write(root.join(".nuo/hooks/h.sh"), "#!/bin/sh\n").unwrap();

    // The gate offers all three; the user selects AGENTS.md and skills.
    let candidates = store.snapshot(root).domains_needing_review();
    assert_eq!(
        candidates,
        vec![
            TrustDomain::Skills,
            TrustDomain::Hooks,
            TrustDomain::Instructions,
        ]
    );
    let selected = vec![TrustDomain::Instructions, TrustDomain::Skills];
    store.trust_domains(root, &selected).unwrap();
    let unselected: Vec<TrustDomain> = candidates
        .into_iter()
        .filter(|d| !selected.contains(d))
        .collect();
    store.deny_domains(root, &unselected).unwrap();

    let snap = store.snapshot(root);
    assert_eq!(snap.instructions, WorkspaceTrustState::Trusted);
    assert_eq!(snap.skills, WorkspaceTrustState::Trusted);
    assert_eq!(snap.hooks, WorkspaceTrustState::Denied, "refused, settled");
    assert!(
        !snap.needs_review(),
        "a fully answered gate must leave nothing awaiting review"
    );

    // A refusal of an already-decided domain is scoped: it must not disturb
    // the grants beside it (that is what `/untrust` is for, and it is a
    // different verb on purpose).
    store
        .deny_domains(root, &[TrustDomain::Instructions])
        .unwrap();
    let snap = store.snapshot(root);
    assert_eq!(snap.instructions, WorkspaceTrustState::Denied);
    assert_eq!(
        snap.skills,
        WorkspaceTrustState::Trusted,
        "refusing one domain must not revoke another's grant"
    );
}

#[test]
fn every_project_rule_file_is_attested_by_the_instructions_domain() {
    // The regression this pins: the loader read `AGENTS.md`,
    // `.cursorrules`, and `.windsurfrules` into the system prompt, while the
    // Instructions digest covered only `AGENTS.md`. A trusted workspace could
    // therefore have arbitrary prompt text injected through the other two
    // files — from a clone, a vendored tree, or a dependency's fixture —
    // with no attestation at all. That is the prompt-injection-by-clone
    // hazard the trust gate exists to prevent, so the digest must cover
    // every file the loader reads.
    for rule_file in nuo_host::PROJECT_RULE_FILES {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));

        std::fs::write(root.join("AGENTS.md"), "agents").unwrap();
        std::fs::write(root.join(rule_file), "rules v1").unwrap();
        assert!(
            store.trust_domain(root, TrustDomain::Instructions).unwrap(),
            "{rule_file} must be present in the digest"
        );
        assert_eq!(
            store.snapshot(root).instructions,
            WorkspaceTrustState::Trusted
        );

        // Editing it must invalidate the grant, exactly like AGENTS.md.
        std::fs::write(root.join(rule_file), "rules v2").unwrap();
        assert_eq!(
            store.snapshot(root).instructions,
            WorkspaceTrustState::Changed,
            "editing {rule_file} must revoke trust: it reaches the system prompt"
        );
    }
}

#[test]
fn the_attested_rule_list_is_the_loaded_rule_list() {
    // One constant, two consumers. The digest covers every rule file the
    // loader reads *and* the scoped instruction directory; a new path is
    // added once and both consumers pick it up.
    for rule_file in nuo_host::PROJECT_RULE_FILES {
        assert!(
            INSTRUCTION_PATHS.contains(rule_file),
            "{rule_file} is loaded but not attested"
        );
    }
    assert!(INSTRUCTION_PATHS.contains(&nuo_host::PROJECT_INSTRUCTIONS_DIR));
}

#[test]
fn scoped_project_instructions_are_attested() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
    std::fs::create_dir_all(root.join(".nuo/instructions")).unwrap();
    std::fs::write(root.join(".nuo/instructions/reviewer.md"), "review v1").unwrap();
    assert!(
        store.trust_domain(root, TrustDomain::Instructions).unwrap(),
        "the scoped instruction directory must be attested"
    );
    std::fs::write(root.join(".nuo/instructions/reviewer.md"), "review v2").unwrap();
    assert_eq!(
        store.snapshot(root).instructions,
        WorkspaceTrustState::Changed,
        "editing a scoped instruction must revoke trust"
    );
}

#[cfg(unix)]
#[test]
fn executable_mode_change_revokes_trust() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let hook = root.join(".nuo/hooks/check.sh");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&hook, permissions).unwrap();

    let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
    assert!(store.trust_domain(root, TrustDomain::Hooks).unwrap());
    let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&hook, permissions).unwrap();
    assert_eq!(store.snapshot(root).hooks, WorkspaceTrustState::Changed);
}

#[cfg(unix)]
#[test]
fn symlinked_domain_content_cannot_be_trusted() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let outside = root.join("outside-skill.md");
    std::fs::write(&outside, "mutable target").unwrap();
    std::fs::create_dir_all(root.join(".nuo/skills/demo")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(".nuo/skills/demo/SKILL.md")).unwrap();

    let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
    let error = store.trust_domain(root, TrustDomain::Skills).unwrap_err();
    assert!(error.contains("symlink"));
    assert_eq!(
        store.snapshot(root).skills,
        WorkspaceTrustState::Quarantined
    );
}

#[test]
fn domains_are_granted_persisted_and_revoked_independently() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let file = root.join("state/workspace_security.json");
    let store = WorkspaceSecurityStore::load_from(file.clone());

    std::fs::create_dir_all(root.join(".nuo/skills/demo")).unwrap();
    std::fs::write(root.join(".nuo/skills/demo/SKILL.md"), "skill body").unwrap();
    std::fs::write(root.join(".nuo/mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
    std::fs::write(
        root.join(".nuo/config.toml"),
        "[[hooks]]\nevent = \"SessionStart\"\ncommand = \"echo ready\"\n",
    )
    .unwrap();

    let snap = store.snapshot(root);
    assert_eq!(snap.mcp, WorkspaceTrustState::Quarantined);
    assert_eq!(snap.skills, WorkspaceTrustState::Quarantined);
    assert_eq!(snap.hooks, WorkspaceTrustState::Quarantined);
    assert_eq!(snap.instructions, WorkspaceTrustState::Absent);

    assert!(store.trust_domain(root, TrustDomain::Mcp).unwrap());
    let snap = store.snapshot(root);
    assert_eq!(snap.mcp, WorkspaceTrustState::Trusted);
    assert_eq!(snap.skills, WorkspaceTrustState::Quarantined);

    let granted = store.trust_domains(root, &TrustDomain::ALL).unwrap();
    assert_eq!(
        granted,
        vec![TrustDomain::Mcp, TrustDomain::Skills, TrustDomain::Hooks]
    );
    let reloaded = WorkspaceSecurityStore::load_from(file);
    let snap = reloaded.snapshot(root);
    assert_eq!(snap.aggregate(), WorkspaceTrustState::Trusted);
    assert_eq!(snap.mcp, WorkspaceTrustState::Trusted);
    assert_eq!(snap.skills, WorkspaceTrustState::Trusted);
    assert_eq!(snap.hooks, WorkspaceTrustState::Trusted);

    assert!(reloaded.revoke_workspace(root).unwrap());
    let snap = reloaded.snapshot(root);
    assert_eq!(snap.mcp, WorkspaceTrustState::Quarantined);
    assert_eq!(snap.skills, WorkspaceTrustState::Quarantined);
    assert_eq!(snap.hooks, WorkspaceTrustState::Quarantined);
}

#[test]
fn config_projections_do_not_cross_invalidate_domains() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join(".nuo")).unwrap();
    let config = root.join(".nuo/config.toml");
    std::fs::write(
        &config,
        "[mcp.demo]\ncommand = [\"demo\"]\n\n[[hooks]]\nevent = \"SessionStart\"\ncommand = \"echo one\"\n",
    )
    .unwrap();
    let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
    store
        .trust_domains(root, &[TrustDomain::Mcp, TrustDomain::Hooks])
        .unwrap();

    std::fs::write(
        &config,
        "[mcp.demo]\ncommand = [\"demo\"]\n\n[[hooks]]\nevent = \"SessionStart\"\ncommand = \"echo two\"\n",
    )
    .unwrap();
    let snap = store.snapshot(root);
    assert_eq!(snap.mcp, WorkspaceTrustState::Trusted);
    assert_eq!(snap.hooks, WorkspaceTrustState::Changed);
}

#[test]
fn ex_workspace_domain_projection_tracks_workspace_table() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join(".nuo")).unwrap();
    let config = root.join(".nuo/config.toml");
    std::fs::write(&config, "[workspace]\nadditional_roots = [\"../optics\"]\n").unwrap();
    let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
    let snap = store.snapshot(root);
    assert_eq!(snap.ex_workspace, WorkspaceTrustState::Quarantined);

    store.trust_domain(root, TrustDomain::ExWorkspace).unwrap();
    let snap = store.snapshot(root);
    assert_eq!(snap.ex_workspace, WorkspaceTrustState::Trusted);

    std::fs::write(
        &config,
        "[workspace]\nadditional_roots = [\"../optics\", \"../backend\"]\n",
    )
    .unwrap();
    let snap = store.snapshot(root);
    assert_eq!(snap.ex_workspace, WorkspaceTrustState::Changed);
}

#[test]
fn denied_domain_lifecycle_and_invalidation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));

    std::fs::create_dir_all(root.join(".nuo/skills/demo")).unwrap();
    let skill_file = root.join(".nuo/skills/demo/SKILL.md");
    std::fs::write(&skill_file, "skill content v1").unwrap();

    // 1. Initially Quarantined
    assert_eq!(
        store.snapshot(root).skills,
        WorkspaceTrustState::Quarantined
    );

    // 2. Deny domain -> becomes Denied
    store.deny_domains(root, &[TrustDomain::Skills]).unwrap();
    let snap = store.snapshot(root);
    assert_eq!(snap.skills, WorkspaceTrustState::Denied);
    assert!(snap.skills.is_denied());
    // In aggregate, when only Denied is present, aggregate is Denied (does not gate)
    assert_eq!(snap.aggregate(), WorkspaceTrustState::Denied);

    // 3. Modifying file on disk invalidates Denied -> becomes Changed!
    std::fs::write(&skill_file, "skill content v2 (altered)").unwrap();
    let snap2 = store.snapshot(root);
    assert_eq!(snap2.skills, WorkspaceTrustState::Changed);
    assert_eq!(snap2.aggregate(), WorkspaceTrustState::Changed);

    // 4. User trusts it now -> becomes Trusted
    store.trust_domains(root, &[TrustDomain::Skills]).unwrap();
    assert_eq!(store.snapshot(root).skills, WorkspaceTrustState::Trusted);
}

#[test]
fn legacy_roots_and_rules_normalized_automatically() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));

    std::fs::write(root.join("AGENTS.md"), "test agents rules").unwrap();
    std::fs::create_dir_all(root.join(".nuo")).unwrap();
    std::fs::write(
        root.join(".nuo/config.toml"),
        "[workspace]\nadditional_roots = []\n",
    )
    .unwrap();

    let root_key = canonical_string(&workspace_identity(root));

    // Manually inject legacy state with "rules" and "roots"
    let mut digests = BTreeMap::new();
    let instructions_digest = domain_digest(root, TrustDomain::Instructions)
        .unwrap()
        .unwrap();
    let ex_workspace_digest = domain_digest(root, TrustDomain::ExWorkspace)
        .unwrap()
        .unwrap();
    digests.insert("rules".to_string(), instructions_digest);
    digests.insert("roots".to_string(), ex_workspace_digest);

    let mut workspaces = BTreeMap::new();
    workspaces.insert(
        root_key,
        WorkspaceRecord {
            domain_digests: digests,
            denied_digests: BTreeMap::new(),
            expires_at_s: BTreeMap::new(),
        },
    );

    let legacy_state = PersistedWorkspaceSecurity {
        version: CURRENT_VERSION,
        workspaces,
    };

    store.persist(&legacy_state).unwrap();

    // When reading snapshot, it should automatically normalize and recognize them as Trusted!
    let snap = store.snapshot(root);
    assert_eq!(snap.instructions, WorkspaceTrustState::Trusted);
    assert_eq!(snap.ex_workspace, WorkspaceTrustState::Trusted);

    // Verify that the persisted DB state now has the normalized keys "instructions" and "ex-workspace"
    let reloaded = store.read_state().unwrap();
    let record = reloaded.workspaces.values().next().unwrap();
    assert!(record.domain_digests.contains_key("instructions"));
    assert!(record.domain_digests.contains_key("ex-workspace"));
    assert!(!record.domain_digests.contains_key("rules"));
    assert!(!record.domain_digests.contains_key("roots"));
}
