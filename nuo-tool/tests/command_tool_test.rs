#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_tool::{CommandTool, RiskProfile, ShellKind, Tool, ToolContext, ToolError};
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn command_tool_contract_metadata() {
    let tool = CommandTool::new()
        .with_default_timeout(Duration::from_secs(30))
        .with_shell(ShellKind::Auto);

    assert_eq!(tool.name(), "execute_command");
    assert_eq!(tool.risk_profile(), RiskProfile::ArbitraryExecution);
    assert_eq!(tool.default_timeout(), Duration::from_secs(30));
    assert_eq!(tool.shell(), &ShellKind::Auto);

    let schema = tool.parameters_schema();
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["command"].is_object());
    assert!(schema["properties"]["working_dir"].is_object());
    assert!(schema["properties"]["shell"].is_object());
    assert!(schema["properties"]["timeout_secs"].is_object());
}

#[tokio::test]
async fn command_tool_executes_echo_successfully() {
    let tool = CommandTool::new();
    let ctx = ToolContext::default();

    let output = tool
        .execute(&ctx, json!({"command": "echo 'Hello Cross-Platform Nous'"}))
        .await
        .expect("echo must succeed");

    assert!(!output.is_error());
    assert!(output.content().contains("[Exit status: 0]"));
    assert!(output.content().contains("Hello Cross-Platform Nous"));
}

#[tokio::test]
async fn command_tool_honors_working_directory() {
    let temp_dir = std::env::temp_dir();
    let temp_dir_str = temp_dir.to_string_lossy().to_string();

    let tool = CommandTool::new();
    let ctx = ToolContext::default();

    // In unix pwd or in windows cd/Get-Location
    #[cfg(windows)]
    let check_cmd = "powershell -NoProfile -Command (Get-Location).Path";
    #[cfg(not(windows))]
    let check_cmd = "pwd -P";

    let output = tool
        .execute(
            &ctx,
            json!({
                "command": check_cmd,
                "working_dir": temp_dir_str
            }),
        )
        .await
        .expect("execution must succeed");

    assert!(!output.is_error());
}

#[tokio::test]
async fn command_tool_enforces_cancellation() {
    let tool = CommandTool::new();
    let cancel = CancellationToken::new();

    // Cancel before or during long sleep
    let ctx = ToolContext::default().with_cancel_token(cancel.clone());

    #[cfg(windows)]
    let sleep_cmd = "powershell -Command Start-Sleep -Seconds 5";
    #[cfg(not(windows))]
    let sleep_cmd = "sleep 5";

    let handle = tokio::spawn(async move {
        tool.execute(&ctx, json!({"command": sleep_cmd})).await
    });

    // Cancel after 50ms
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();

    let result = handle.await.expect("task join succeeded");
    match result {
        Err(ToolError::Cancelled(name)) => assert_eq!(name, "execute_command"),
        other => panic!("expected Cancelled error, got {other:?}"),
    }
}

#[tokio::test]
async fn command_tool_enforces_timeout() {
    let _tool = CommandTool::new().with_default_timeout(Duration::from_millis(100));
    let ctx = ToolContext::default();

    #[cfg(windows)]
    let sleep_cmd = "powershell -Command Start-Sleep -Seconds 2";
    #[cfg(not(windows))]
    let sleep_cmd = "sleep 2";

    let tool_with_short = CommandTool::new().with_default_timeout(Duration::from_millis(50));
    let err = tool_with_short
        .execute(&ctx, json!({"command": sleep_cmd}))
        .await
        .unwrap_err();

    match err {
        ToolError::Timeout(_) => {}
        other => panic!("expected Timeout error, got {other:?}"),
    }
}

#[tokio::test]
async fn command_tool_approval_trigger_for_high_risk() {
    let tool = CommandTool::new();
    let ctx = ToolContext::default();

    // Safe command: does not require approval
    assert!(!tool.requires_approval(&ctx, &json!({"command": "cargo --version"})));

    // Destructive Unix: requires approval
    assert!(tool.requires_approval(&ctx, &json!({"command": "rm -rf /var/log"})));

    // Destructive Windows / PowerShell: requires approval
    assert!(tool.requires_approval(
        &ctx,
        &json!({"command": "Remove-Item -Path C:\\Temp -Recurse -Force"})
    ));
    assert!(tool.requires_approval(&ctx, &json!({"command": "rd /s /q D:\\Test"})));

    // When require_all_approval is enabled, even safe command requires approval
    let strict_tool = tool.require_approval_for_all(true);
    assert!(strict_tool.requires_approval(&ctx, &json!({"command": "cargo --version"})));
}
