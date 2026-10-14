use nuo_wire::{
    AgentRequest, AgentResponse, PermissionDecision, PermissionRequest, RoundEvent,
    UserQuestionRequest,
};
use nuo_client::{self as client, AttachAction, Handshake};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

fn send_intent(
    tx: &tokio::sync::mpsc::UnboundedSender<AgentRequest>,
    request: AgentRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    tx.send(request)
        .map_err(|error| format!("server link lost before intent delivery: {error}").into())
}

#[allow(clippy::too_many_arguments)]
pub async fn run_headless(
    prompt: String,
    json: bool,
    project_override: Option<PathBuf>,
    unattended: bool,
    confined: bool,
    role: Option<String>,
    resume: bool,
    remote: Option<String>,
    token: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Two mutually exclusive transports, resolved once:
    //
    // - `--remote <addr>` names an explicit server endpoint (ADR-0105's
    //   LAN shape): connect to it directly — no discovery read, no local
    //   spawn. A remote run that silently fell back to the local instance
    //   would be the worst kind of lie: it would *appear* to work while
    //   driving the wrong server.
    // - Otherwise the local instance: discover, or spawn on demand.
    //
    // The version pre-check applies only to the local shape — it compares
    // local discovery state (pid, /proc image), which does not exist for
    // a remote server; there, the handshake carries the version
    // negotiation.
    enum Transport {
        Remote(client::RemoteServer),
        Local(client::ServerInfo),
    }
    let transport = match remote {
        Some(addr) => Transport::Remote(client::RemoteServer::parse(&addr, token)?),
        None => {
            let project_root = project_override
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let info = client::ensure_server(&project_root).await?;
            if !client::versions_compatible(&info) {
                return Err(client::incompatibility_error(&info).into());
            }
            Transport::Local(info)
        }
    };

    // ADR-0141: declare the human-channel posture before the attach. A
    // headless run has no interactive UI by definition — `is_tty` below only
    // distinguishes "can print a question and read a line" from a pipe. A
    // TTY headless run declares Interactive (the operator is on the other
    // end of stderr/stdin); a piped run declares Autonomous so the session's
    // posture gate settles questions by labeled policy instead of letting
    // this client fabricate answers below (the old `options.first()` bug).
    {
        client::set_posture(if io::stderr().is_terminal() {
            nuo_wire::human_request::HumanChannelPosture::Interactive
        } else {
            nuo_wire::human_request::HumanChannelPosture::Autonomous
        });
    }
    let init_options = nuo_client::SessionInitOptions::new(unattended, confined)
        .with_role(role)
        .with_resume(resume);
    let action = AttachAction::New(if init_options.is_default() {
        None
    } else {
        Some(init_options)
    });
    let handshake = match &transport {
        Transport::Remote(server) => server.connect(action.clone()).await?,
        Transport::Local(info) => client::connect(info, action).await?,
    };
    let (tx, mut rx, session_id, _round_counter, _history, provider, model) = match handshake {
        Handshake::Attached {
            req_tx,
            resp_rx,
            session_id,
            round_counter,
            history,
            round_interrupts: _,
            retry_resolutions: _,
            provider,
            model,
            command_catalog: _,
        } => (
            req_tx,
            resp_rx,
            session_id,
            round_counter,
            history,
            provider,
            model,
        ),
        Handshake::Pick(_) => {
            return Err("unexpected session pick list when creating fresh headless session".into());
        }
    };

    if json {
        let init_event = serde_json::json!({
            "type": "session_init",
            "session_id": session_id,
            "provider": provider,
            "model": model,
        });
        println!("{}", serde_json::to_string(&init_event)?);
        io::stdout().flush()?;
    }

    // Dispatch the prompt
    send_intent(
        &tx,
        AgentRequest::Prompt {
            text: prompt,
            images: Vec::new(),
            sent_at_ms: None,
        },
    )?;

    let is_tty = io::stderr().is_terminal();
    let mut accumulated_text = String::new();

    while let Some(resp) = rx.recv().await {
        match resp {
            AgentResponse::Round {
                session_id: _,
                event,
            } => match event {
                RoundEvent::Text(delta) => {
                    if json {
                        accumulated_text.push_str(&delta);
                        let event_obj = serde_json::json!({
                            "type": "text_delta",
                            "delta": delta,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                        io::stdout().flush()?;
                    } else {
                        accumulated_text.push_str(&delta);
                        print!("{delta}");
                        io::stdout().flush()?;
                    }
                }
                RoundEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    if json {
                        let event_obj = serde_json::json!({
                            "type": "tool_call",
                            "id": id,
                            "name": name,
                            "arguments": arguments,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                        io::stdout().flush()?;
                    } else {
                        if is_tty {
                            eprintln!("\n\x1b[36m[Tool Call]\x1b[0m {name}({arguments})");
                        } else {
                            eprintln!("\n[Tool Call] {name}({arguments})");
                        }
                        io::stderr().flush()?;
                    }
                }
                RoundEvent::ToolStream { id, stream } => {
                    if !json {
                        match stream {
                            nuo_wire::ToolStream::Stdout(text) => {
                                let _ = io::stderr().write_all(text.as_bytes());
                                let _ = io::stderr().flush();
                            }
                            nuo_wire::ToolStream::Stderr(text) => {
                                let _ = io::stderr().write_all(text.as_bytes());
                                let _ = io::stderr().flush();
                            }
                        }
                    } else {
                        let stream_obj = serde_json::json!({
                            "type": "tool_stream",
                            "id": id,
                        });
                        println!("{}", serde_json::to_string(&stream_obj)?);
                        io::stdout().flush()?;
                    }
                }
                RoundEvent::ToolResult {
                    id,
                    name,
                    output: _,
                    structured,
                    duration_ms,
                } => {
                    if json {
                        let event_obj = serde_json::json!({
                            "type": "tool_result",
                            "id": id,
                            "name": name,
                            "duration_ms": duration_ms,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                        io::stdout().flush()?;
                    } else {
                        let is_error = matches!(
                            structured,
                            nuo_wire::ToolOutput::Error { .. }
                                | nuo_wire::ToolOutput::PermissionDenied { .. }
                        ) || match &structured {
                            nuo_wire::ToolOutput::Shell { exit, .. } => {
                                exit.is_some_and(|code| code != 0)
                            }
                            _ => false,
                        };
                        let status_label = if !is_error {
                            if is_tty {
                                "\x1b[32mcompleted\x1b[0m"
                            } else {
                                "completed"
                            }
                        } else {
                            if is_tty {
                                "\x1b[31mfailed\x1b[0m"
                            } else {
                                "failed"
                            }
                        };
                        eprintln!("[Tool {status_label}] {name} ({duration_ms}ms)");
                        io::stderr().flush()?;
                    }
                }
                RoundEvent::PermissionRequest(req) => {
                    handle_permission_request(&tx, req, unattended, is_tty).await?;
                }
                RoundEvent::UserQuestionRequest(req) => {
                    handle_user_question_request(&tx, req, is_tty).await?;
                }
                RoundEvent::RoundCompleted(summary) => {
                    if json {
                        let event_obj = serde_json::json!({
                            "type": "round_completed",
                            "output_tokens": summary.output_tokens,
                            "duration_ms": summary.duration_ms,
                            "generation_ms": summary.generation_ms,
                            "response": accumulated_text,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                        io::stdout().flush()?;
                    } else {
                        if !accumulated_text.ends_with('\n') {
                            println!();
                        }
                        io::stdout().flush()?;
                    }
                    declare_session_end(&tx, &mut rx).await;
                    return Ok(());
                }
                RoundEvent::StreamDelta(delta) => {
                    // Streaming providers (the common case) deliver the
                    // assistant text as deltas; the terminal `RoundEvent::Text`
                    // backstop only fires for non-streamed providers. Without
                    // this arm a streamed round completes with an empty
                    // `response`.
                    if json {
                        accumulated_text.push_str(&delta);
                        let event_obj = serde_json::json!({
                            "type": "text_delta",
                            "delta": delta,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                        io::stdout().flush()?;
                    } else {
                        accumulated_text.push_str(&delta);
                        print!("{delta}");
                        io::stdout().flush()?;
                    }
                }
                RoundEvent::RoundInterrupted(record) => {
                    // C11: the round stopped before completing. Machine
                    // readers get the typed reason + timestamp; humans get a
                    // one-line notice.
                    if json {
                        let event_obj = serde_json::json!({
                            "type": "round_interrupted",
                            "reason": record.reason,
                            "reason_label": record.label(),
                            "at_ms": record.at_ms,
                            "round": record.round,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                        io::stdout().flush()?;
                    } else if is_tty {
                        eprintln!("\n\x1b[33m[Interrupted — {}]\x1b[0m", record.label());
                        io::stderr().flush()?;
                    } else {
                        eprintln!("\n[Interrupted — {}]", record.label());
                        io::stderr().flush()?;
                    }
                }
                RoundEvent::Error(err) => {
                    if json {
                        let event_obj = serde_json::json!({
                            "type": "error",
                            "error": err,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                    } else {
                        eprintln!("\nmuta: error: {err}");
                    }
                    declare_session_end(&tx, &mut rx).await;
                    return Err(err.into());
                }
                RoundEvent::BackgroundJobStarted(_)
                | RoundEvent::BackgroundJobProgress { .. }
                | RoundEvent::BackgroundJobCompleted(_) => {}
                _ => {}
            },
            AgentResponse::Error(err) => {
                if json {
                    let event_obj = serde_json::json!({
                        "type": "error",
                        "error": err,
                    });
                    println!("{}", serde_json::to_string(&event_obj)?);
                } else {
                    eprintln!("\nmuta: error: {err}");
                }
                declare_session_end(&tx, &mut rx).await;
                return Err(err.into());
            }
            AgentResponse::Exit => {
                return Ok(());
            }
            _ => {}
        }
    }

    Ok(())
}

/// Tell the server the headless run's session is over (ADR-0112) and wait
/// for it to acknowledge with the terminal `AgentResponse::Exit`. Headless
/// runs are ephemeral by design — the operator asked one question and is
/// gone — so leaving the session hosted (the detach semantics a TUI get)
/// would only litter the dashboard with dead rows waiting for an idle
/// reaper that never applies (a session with real content is never reaped).
/// Bounded: a server that never answers does not hang the CLI.
async fn declare_session_end(
    tx: &tokio::sync::mpsc::UnboundedSender<AgentRequest>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentResponse>,
) {
    if tx.send(AgentRequest::EndSession).is_err() {
        return; // Connection already gone; the server sees the socket close.
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(AgentResponse::Exit)) | Ok(None) => return,
            Ok(Some(_)) => continue,
            Err(_) => return, // Timed out; teardown proceeds server-side.
        }
    }
}

async fn handle_permission_request(
    tx: &tokio::sync::mpsc::UnboundedSender<AgentRequest>,
    req: PermissionRequest,
    unattended: bool,
    is_tty: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if unattended {
        send_intent(
            tx,
            AgentRequest::PermissionReply {
                request_id: req.id,
                decision: PermissionDecision::Once,
                parent_call_id: None,
            },
        )?;
        return Ok(());
    }

    if !is_tty {
        eprintln!(
            "nuo: authority is missing for tool '{}' in non-interactive mode; rejecting. Configure workspace authority or a narrow persistent permission first.",
            req.tool
        );
        send_intent(
            tx,
            AgentRequest::PermissionReply {
                request_id: req.id,
                decision: PermissionDecision::Reject,
                parent_call_id: None,
            },
        )?;
        return Ok(());
    }

    eprintln!(
        "\n\x1b[33m[Permission Request]\x1b[0m Tool: {} ({})\nDetails: {}",
        req.tool, req.scope, req.description
    );
    eprint!("Allow execution? [y]es, [a]lways, [n]o: ");
    io::stderr().flush()?;

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let answer = line.trim().to_lowercase();

    let decision = match answer.as_str() {
        "y" | "yes" => PermissionDecision::Once,
        "s" | "session" => PermissionDecision::Session,
        "a" | "always" => PermissionDecision::Always,
        _ => PermissionDecision::Reject,
    };

    send_intent(
        tx,
        AgentRequest::PermissionReply {
            request_id: req.id,
            decision,
            parent_call_id: None,
        },
    )?;
    Ok(())
}

async fn handle_user_question_request(
    tx: &tokio::sync::mpsc::UnboundedSender<AgentRequest>,
    req: UserQuestionRequest,
    is_tty: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut all_answers = Vec::new();

    // ADR-0141: the non-TTY fabrication branch is gone. A piped run
    // declares the Autonomous posture at attach, so the session's posture
    // gate settles `ask_user` by labeled policy (fail-closed or
    // recommended-labeled per `[agent] ask_user_fallback`) *inside* the
    // harness — this handler is only reached when an interactive human is
    // on the other end of stdin/stderr. If one still slips through (e.g. a
    // legacy server), fail closed rather than inventing an answer.
    if !is_tty {
        eprintln!(
            "nuo: agent asked a question but stdin is not a TTY and no \
             human channel is attached; cancelling the question. Run with a \
             terminal, or configure `[agent] ask_user_fallback` for \
             autonomous answering."
        );
        send_intent(
            tx,
            AgentRequest::UserQuestionReply {
                request_id: req.id.clone(),
                answers: Vec::new(),
                parent_call_id: None,
            },
        )?;
        return Ok(());
    }

    for q in &req.questions {
        eprintln!("\n\x1b[35m[Question]\x1b[0m {}", q.question);
        for (i, opt) in q.options.iter().enumerate() {
            if let Some(desc) = &opt.description {
                eprintln!("  {}. {} ({desc})", i + 1, opt.label);
            } else {
                eprintln!("  {}. {}", i + 1, opt.label);
            }
        }
        eprint!("Your choice (number or write-in): ");
        io::stderr().flush()?;

        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let choice = line.trim();

        if let Ok(idx) = choice.parse::<usize>()
            && idx > 0
            && idx <= q.options.len()
        {
            all_answers.push(vec![q.options[idx - 1].label.clone()]);
            continue;
        }
        all_answers.push(vec![choice.to_string()]);
    }

    send_intent(
        tx,
        AgentRequest::UserQuestionReply {
            request_id: req.id,
            answers: all_answers,
            parent_call_id: None,
        },
    )?;
    Ok(())
}
