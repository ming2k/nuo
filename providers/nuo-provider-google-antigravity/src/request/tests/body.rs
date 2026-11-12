//! Body assembly: instructions, images, tools, and thinking config.

use super::*;

#[test]
fn declared_text_only_route_projects_inline_data_away() {
    // Google's `inline_data` used to be emitted unconditionally, so a route
    // without image input failed the whole turn.
    let body = body_with_capabilities(
        "gemini-test",
        vec![image_message("look")],
        test_body_input(None, false, None),
        &caps_with_vision(Some(false)),
    );

    let parts = body["contents"][0]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["text"], "look");
    assert!(body["contents"][0]["parts"][0].get("inline_data").is_none());
}

#[test]
fn undeclared_route_keeps_inline_data() {
    let body = body_with_capabilities(
        "gemini-test",
        vec![image_message("look")],
        test_body_input(None, false, None),
        &caps_with_vision(None),
    );

    let parts = body["contents"][0]["parts"].as_array().unwrap();
    assert_eq!(parts[1]["inline_data"]["mime_type"], "image/png");
    assert_eq!(parts[1]["inline_data"]["data"], "aGk=");
}

#[test]
fn preserves_system_harness_context() {
    let body = body(
        vec![
            Message::new(Role::System, "pursuit and tools"),
            Message::new(Role::User, "continue"),
        ],
        test_body_input(None, false, None),
    );

    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"],
        "pursuit and tools"
    );
    assert_eq!(body["contents"][0]["role"], "user");
}

#[test]
fn fallback_tool_results_are_user_context() {
    let body = body(
        vec![
            Message::new(Role::Assistant, "{\"tool\":\"read_text\"}"),
            Message::new(Role::Tool, "file contents"),
            Message::new(Role::User, "next"),
        ],
        test_body_input(None, false, None),
    );

    assert_eq!(body["contents"][1]["role"], "user");
    assert_eq!(body["contents"][1]["parts"][0]["text"], "next");
}

#[test]
fn declares_google_function_tools() {
    let body = body(
        vec![Message::new(Role::User, "list files")],
        test_body_input(
            Some(&[nuo_provider::capability::ToolSpec {
                name: "find_files".to_string(),
                description: "List files".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }),
            }]),
            false,
            None,
        ),
    );

    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["name"],
        "find_files"
    );
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["parameters"]["properties"]["path"]["type"],
        "string"
    );
}

#[test]
fn include_thoughts_stamps_thinking_config() {
    let body = body(
        vec![Message::new(Role::User, "why is the sky blue?")],
        test_body_input(None, true, None),
    );
    // Reasoning text is only surfaced when explicitly requested, and the
    // flag lives under generationConfig.thinkingConfig.
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["includeThoughts"],
        true
    );
}

#[test]
fn include_thoughts_off_omits_thinking_config() {
    let body = body(
        vec![Message::new(Role::User, "hi")],
        test_body_input(None, false, None),
    );
    assert!(body.get("generationConfig").is_none());
}
