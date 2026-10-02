//! Shared `/sessions` routing + posture-flag parsing, split out of the
//! retired `/schedule` module (ADR-0186 removed the scheduled-prompt
//! feature; these helpers serve `/sessions` and the command flags).

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionRoute<'a> {
    Open(Option<&'a str>),
    New,
    Fork,
    Status,
}

pub(crate) fn session_route<'a>(name: &str, parts: &'a [&str]) -> Result<SessionRoute<'a>, String> {
    if name != "session" {
        return Ok(SessionRoute::Open(parts.get(1).copied()));
    }
    match parts.get(1).copied().unwrap_or("") {
        "open" | "resume" => Ok(SessionRoute::Open(parts.get(2).copied())),
        "" => Ok(SessionRoute::Open(None)),
        "list" => Ok(SessionRoute::Open(None)),
        "new" => Ok(SessionRoute::New),
        "fork" => Ok(SessionRoute::Fork),
        "status" => Ok(SessionRoute::Status),
        unknown => Err(format!(
            "Unknown session command '{unknown}'. /session is retired: use /sessions to browse \
             or open, /new, or /fork."
        )),
    }
}

pub(crate) fn parse_unattended_arg(arg: &str) -> Result<Option<bool>, String> {
    match arg.trim() {
        "" => Ok(None),
        "on" | "true" | "1" | "enable" | "enabled" | "unattended" | "auto" | "delegate"
        | "yolo" => Ok(Some(true)),
        "off" | "false" | "0" | "disable" | "disabled" | "attended" => Ok(Some(false)),
        other => Err(format!(
            "Unknown value '{other}'. Use `/unattended` to toggle, or `/unattended on|off`."
        )),
    }
}

pub(crate) fn parse_confinement_arg(arg: &str) -> Result<Option<bool>, String> {
    match arg.trim() {
        "" => Ok(None),
        "on" | "true" | "1" | "enable" | "enabled" | "confine" | "confined" | "jail" => {
            Ok(Some(true))
        }
        "off" | "false" | "0" | "disable" | "disabled" | "unconfine" | "unconfined" | "escape" => {
            Ok(Some(false))
        }
        other => Err(format!(
            "Unknown value '{other}'. Use `/confinement` to toggle, or `/confinement on|off`."
        )),
    }
}
