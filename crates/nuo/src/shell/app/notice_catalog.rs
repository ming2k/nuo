//! The client notice catalog (ADR-0138): one closed, typed registry of every
//! disclosure the shell itself produces.
//!
//! # Why a catalog and not a string
//!
//! ADR-0121 centralized notice *residency* (`SurfaceLog(NOTICES)`) and ADR-0134
//! centralized notice *presentation* (the notice-center dialog). What stayed
//! scattered was the *content*: each producer formatted its own sentence and
//! invented its own topic tag inline, so
//!
//! - the same class of failure could be worded differently in every file,
//! - local producers' free-form tags (`"attach"`, `"client"`, `"link"`) sat
//!   outside the closed wire `NoticeKind` vocabulary, so the notice center fell
//!   back to the generic `notification` topic for the client's own faults, and
//! - timestamps were faked at several call sites (`at_ms: 0`), producing rows
//!   whose wall-clock stamp was nonsense.
//!
//! This module closes both vocabularies and gives the funnel exactly one shape
//! to accept:
//!
//! - [`ClientTopic`] — the **closed** set of topics the client speaks.
//! - [`ClientNotice`] — the **closed** set of structural client notices. Adding
//!   a disclosure means adding a variant here, which is a deliberate cost:
//!   wording is a contract, and the invariant test makes forgetting it loud.
//! - [`RenderedNotice`] — the typed payload the funnel consumes. The only
//!   constructors are [`ClientNotice::render`] (this closed enum) and
//!   [`RenderedNotice::from_core`] (the closed wire `AgentNotice` projection),
//!   so a producer cannot build an arbitrary disclosure out of loose strings.
//!
//! # The one dynamic path, named and bounded
//!
//! `show_local_toast` carries client-authored wording that is genuinely dynamic
//! ("clipboard is empty", "can't paste image into this field", …). Enumerating
//! every such sentence would be a large, low-value enum and would forbid honest
//! runtime detail. That path is therefore explicit:
//! [`ClientNotice::LocalFault`], which still forces a **closed** [`ClientTopic`]
//! and a severity, so the *vocabulary* is closed even where the *sentence* is
//! supplied by the caller.

use nuo_view::model::document::{NoticeSeverity, notice_topic_label};

/// A topic the client speaks — the closed local counterpart of the wire's
/// [`notice_topic_label`] vocabulary.
///
/// The labels are the strings the notice center and transcript already render;
/// centralizing them here is what stops a new producer inventing a synonym.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTopic {
    /// The client↔server link (outbox closed, reader stopped).
    Link,
    /// Session attach / readiness / tab retirement.
    Attach,
    /// A generic client-process disclosure.
    Client,
    /// A clipboard write or read that failed.
    Copy,
    /// A slash-command / configuration outcome surfaced locally.
    Command,
    /// An image attachment that was withheld from a request.
    Images,
    /// A provider retry setback (a retained receipt; the live countdown is
    /// chrome, `nuo_view::retry`, never a notice).
    Retry,
    /// An OAuth/authorization flow step (pending, authorized, failed).
    Oauth,
    /// A model-catalog / connection-state notice (e.g. a list-sync warning).
    Models,
}

impl ClientTopic {
    /// The rendered topic label (the pill/head word).
    pub fn label(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Attach => "attach",
            Self::Client => "client",
            Self::Copy => "copy",
            Self::Command => "command",
            Self::Images => "images",
            Self::Retry => "retry",
            Self::Oauth => "oauth",
            Self::Models => "models",
        }
    }
}
/// One structural disclosure the client produces.
///
/// Every variant owns its severity and topic; the wording lives in
/// [`Self::render`] so "what does the client say when the link dies?" has one
/// answer. Dynamic detail is carried as typed data, never pre-formatted at the
/// call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientNotice {
    /// The outbox channel closed: an intent could not be delivered.
    LinkSendFailed,
    /// The reader stopped: the session is still hosted, but this client can no
    /// longer reach it. `reason` is the transport's own message.
    LinkLost { reason: String },
    /// The session could not be prepared after Welcome (`ThreadPhase::Failed`).
    SessionPrepFailed { detail: String },
    /// A prompt was queued locally because the session was not yet `Ready`.
    QueuedWhilePreparing,
    /// A preserved tab's session no longer exists on the server.
    AttachSessionGone { detail: String },
    /// A preserved tab's session exists but could not be assembled.
    AttachSessionUnresumable { detail: String },
    /// A local action failed with client-authored wording under a closed topic.
    ///
    /// The bounded dynamic escape hatch (see the module docs): the topic and
    /// severity are still fixed by the catalog, only the sentence is supplied.
    LocalFault { topic: ClientTopic, message: String },
    /// A local action succeeded and is reported as an acknowledgement.
    LocalAck { topic: ClientTopic, message: String },
    /// A provider retry setback: a retained receipt whose summary names the
    /// attempt and the route, and whose detail is the fault verbatim so the
    /// provider's full body stays readable and pasteable in the viewer.
    ProviderRetryScheduled { summary: String, detail: String },
    /// An OAuth authorization is pending: the URL (and code, when present) the
    /// user must visit.
    OauthPending {
        message: String,
        url: String,
        user_code: String,
    },
    /// A provider completed the OAuth flow.
    OauthAuthorized { provider: String },
    /// A provider connection attempt failed outright.
    OauthConnectFailed { provider: String, message: String },
    /// A provider's model list could not be refreshed. `refused` distinguishes a
    /// permanent credential/plan refusal from a transient failure.
    CatalogSyncWarning {
        provider: String,
        message: String,
        refused: bool,
    },
    /// A link could not be opened in the browser.
    LinkOpenFailed { url: String, error: String },
    /// A steer cancellation lost the race: the steer was admitted after all, so
    /// the requested deletion did not happen.
    SteerCancelRaced,
    /// A steer missed the active round and was handed back to the composer.
    SteerMissedRoundRestored,
    /// A steer ended without admission, but its outbox row was already gone,
    /// so the text could not be restored to the composer.
    ///
    /// This is the last-resort arm: the row itself is the payload, so an
    /// ending that arrives after the row disappeared means the user's message
    /// is unrecoverable. Dropping it silently would be a message vanishing
    /// with no transcript row, no draft, and no receipt — the worst possible
    /// outcome, and the exact one a full-replace queue snapshot used to cause.
    SteerContentLost,
    /// A slash-command reply that is a **reading** (ADR-0140): the answer to a
    /// question about the world at the moment it was asked.
    ///
    /// A reading changed nothing durable, so it has no ledger row and no place on
    /// the thread's time axis — the transcript's contract is that every row was
    /// true when it was said, and a measurement carries no freshness signal. The
    /// reply still arrives in full: `body` is the command's own rendered text and
    /// rides in the retained line's detail, which the notice center already shows
    /// expanded, scrolled, and copied.
    ///
    /// `failed` is the result's own error shape, not a string sniff: a reading
    /// that could not be produced is a fault worth a bubble, and one that was
    /// produced is information.
    CommandReceipt {
        invocation: String,
        body: String,
        failed: bool,
    },
}

impl ClientNotice {
    /// Render this notice into the funnel's typed payload.
    ///
    /// This is the **only** place a client notice's wording is decided.
    pub fn render(&self) -> RenderedNotice {
        match self {
            Self::CommandReceipt {
                invocation,
                body,
                failed,
            } => RenderedNotice {
                severity: if *failed {
                    NoticeSeverity::Error
                } else {
                    NoticeSeverity::Info
                },
                topic: ClientTopic::Command.label(),
                title: if *failed {
                    let first_line = body.lines().next().unwrap_or(body.as_str()).trim();
                    if first_line.is_empty() {
                        format!("Failed: {invocation}")
                    } else {
                        first_line.to_string()
                    }
                } else {
                    invocation.clone()
                },
                detail: Some(body.clone()),
            },
            Self::LinkSendFailed => RenderedNotice {
                severity: NoticeSeverity::Error,
                topic: ClientTopic::Link.label(),
                title: "Lost the connection to the session server: outbox channel closed. \
                        The session is still running — reconnecting..."
                    .to_string(),
                detail: None,
            },
            Self::LinkLost { reason } => RenderedNotice {
                severity: NoticeSeverity::Error,
                topic: ClientTopic::Link.label(),
                title: format!(
                    "Lost the connection to the session server: {reason}. \
                     The session is still running — reconnect with `nuo`."
                ),
                detail: None,
            },
            Self::SessionPrepFailed { detail } => RenderedNotice {
                severity: NoticeSeverity::Error,
                topic: ClientTopic::Attach.label(),
                title: format!("The session could not be prepared: {detail}"),
                detail: None,
            },
            Self::QueuedWhilePreparing => RenderedNotice {
                severity: NoticeSeverity::Info,
                topic: ClientTopic::Attach.label(),
                title: "Session is still preparing; your message will be sent when it is ready."
                    .to_string(),
                detail: None,
            },
            Self::AttachSessionGone { detail } => RenderedNotice {
                severity: NoticeSeverity::Warning,
                topic: ClientTopic::Attach.label(),
                title: format!(
                    "That tab's session no longer exists; it has been removed. ({detail})"
                ),
                detail: None,
            },
            Self::AttachSessionUnresumable { detail } => RenderedNotice {
                severity: NoticeSeverity::Error,
                topic: ClientTopic::Attach.label(),
                title: format!("Could not reopen that tab's session: {detail}"),
                detail: None,
            },
            Self::LocalFault { topic, message } => RenderedNotice {
                severity: NoticeSeverity::Error,
                topic: topic.label(),
                title: message.clone(),
                detail: None,
            },
            Self::LocalAck { topic, message } => RenderedNotice {
                severity: NoticeSeverity::Info,
                topic: topic.label(),
                title: message.clone(),
                detail: None,
            },
            Self::ProviderRetryScheduled { summary, detail } => RenderedNotice {
                severity: NoticeSeverity::Warning,
                topic: ClientTopic::Retry.label(),
                title: summary.clone(),
                detail: Some(detail.clone()),
            },
            Self::OauthPending {
                message,
                url,
                user_code,
            } => {
                let title = if user_code.is_empty() {
                    format!("{message} Open: {url}")
                } else {
                    format!("{message} Open: {url} — code {user_code}")
                };
                RenderedNotice {
                    severity: NoticeSeverity::Info,
                    topic: ClientTopic::Oauth.label(),
                    title,
                    detail: Some("Waiting for authorization…".to_string()),
                }
            }
            Self::OauthAuthorized { provider } => RenderedNotice {
                severity: NoticeSeverity::Info,
                topic: ClientTopic::Oauth.label(),
                title: format!("{provider} authorized."),
                detail: None,
            },
            Self::OauthConnectFailed { provider, message } => RenderedNotice {
                severity: NoticeSeverity::Error,
                topic: ClientTopic::Oauth.label(),
                title: format!("{provider} connect failed: {message}"),
                detail: None,
            },
            Self::CatalogSyncWarning {
                provider,
                message,
                refused,
            } => RenderedNotice {
                severity: NoticeSeverity::Warning,
                topic: ClientTopic::Models.label(),
                title: if *refused {
                    format!(
                        "{provider}: the upstream refused the model-list request ({message}). This connection is not usable with the current credentials or plan; the previously cached list is still shown."
                    )
                } else {
                    format!(
                        "{provider}: could not refresh the model list ({message}). Showing the previous list."
                    )
                },
                detail: None,
            },
            Self::LinkOpenFailed { url, error } => RenderedNotice {
                severity: NoticeSeverity::Error,
                topic: ClientTopic::Link.label(),
                title: format!("Failed to open link {url}: {error}"),
                detail: None,
            },
            Self::SteerCancelRaced => RenderedNotice {
                severity: NoticeSeverity::Warning,
                topic: ClientTopic::Command.label(),
                title: "Steer already admitted; it could not be cancelled".to_string(),
                detail: None,
            },
            Self::SteerMissedRoundRestored => RenderedNotice {
                severity: NoticeSeverity::Info,
                topic: ClientTopic::Command.label(),
                title: "Steer missed active round; restored to composer".to_string(),
                detail: None,
            },
            Self::SteerContentLost => RenderedNotice {
                severity: NoticeSeverity::Error,
                topic: ClientTopic::Command.label(),
                title: "Steer was never admitted and its text could not be restored".to_string(),
                detail: Some(
                    "The steer left the outbox before its round reported an outcome, so the \
                     client no longer holds the prompt. Re-send it if it is still wanted."
                        .to_string(),
                ),
            },
        }
    }
}

/// The typed payload the residency funnel consumes.
///
/// Severity, topic, one-line title and optional structured detail — and nothing
/// else. In particular there is **no** `at_ms`: the funnel stamps time from the
/// single client clock, so the historical `at_ms: 0` defect is unrepresentable.
///
/// The fields are private to this module on purpose ([`INV-NOTICE-08]`): the
/// only constructors are [`ClientNotice::render`] (closed client enum) and
/// [`RenderedNotice::from_core`] (closed wire projection), because a public
/// struct-literal is exactly the hole a producer would use to smuggle in an
/// arbitrary topic or sentence. The funnel reads the fields through the
/// `pub(crate)` accessors below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedNotice {
    severity: NoticeSeverity,
    topic: &'static str,
    title: String,
    detail: Option<String>,
}

impl RenderedNotice {
    /// Severity of this disclosure.
    pub fn severity(&self) -> NoticeSeverity {
        self.severity
    }
    /// The closed-vocabulary topic label.
    pub fn topic(&self) -> &'static str {
        self.topic
    }
    /// The one-line title.
    pub fn title(&self) -> &str {
        &self.title
    }
    /// The optional structured detail (viewer's expanded body).
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Project a routed wire notice into the funnel payload.
    ///
    /// The topic comes from the closed wire vocabulary
    /// ([`notice_topic_label`]), so a routed notice never falls back to the
    /// generic topic either.
    pub fn from_core(notice: &nuo_tool::AgentNotice) -> Self {
        let title = notice.render_text();
        let (title, detail) = split_title_detail(title, notice.body.as_deref());
        Self {
            severity: nuo_view::model::document::notice_severity_from_core(notice.severity),
            topic: notice_topic_label(notice.kind),
            title,
            detail,
        }
    }

    /// Build a payload from loose parts — **test only**.
    ///
    /// The production constructors are the closed enum and the wire projection;
    /// this exists so unit tests can exercise the funnel's *residency* behaviour
    /// (dwell, badge, retention) without minting a domain kind for every
    /// scenario. It is `cfg(test)`, so it cannot be reached from production
    /// code and `[INV-NOTICE-08]` still holds.
    #[cfg(test)]
    pub(crate) fn for_test(
        severity: NoticeSeverity,
        topic: ClientTopic,
        title: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            topic: topic.label(),
            title: title.into(),
            detail: None,
        }
    }
}

/// Split a routed notice into its one-line title and optional structured
/// detail without re-deriving anything: a body, when present, is the detail;
/// otherwise the rendered text's first line is the title and the remainder is
/// the detail.
fn split_title_detail(rendered: String, body: Option<&str>) -> (String, Option<String>) {
    if let Some(body) = body.filter(|b| !b.trim().is_empty()) {
        let title = rendered
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        return (title, Some(body.to_string()));
    }
    match rendered.split_once('\n') {
        Some((first, rest)) if !rest.trim().is_empty() => {
            (first.trim().to_string(), Some(rest.trim().to_string()))
        }
        _ => (rendered.trim().to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog is only a registry if every entry is well-formed. This walks
    /// the closed set and locks the properties the notice surfaces depend on:
    /// a non-empty one-line title, a topic that is never the generic fallback,
    /// and a title short enough to fit the dialog's single headline row.
    #[test]
    fn every_catalog_entry_is_shaped_for_the_notice_surfaces() {
        let entries = vec![
            ClientNotice::LinkSendFailed,
            ClientNotice::LinkLost {
                reason: "outbox closed".to_string(),
            },
            ClientNotice::SessionPrepFailed {
                detail: "thread could not be assembled".to_string(),
            },
            ClientNotice::QueuedWhilePreparing,
            ClientNotice::AttachSessionGone {
                detail: "gone".to_string(),
            },
            ClientNotice::AttachSessionUnresumable {
                detail: "unresumable".to_string(),
            },
            ClientNotice::LocalFault {
                topic: ClientTopic::Copy,
                message: "clipboard is empty".to_string(),
            },
            ClientNotice::LocalAck {
                topic: ClientTopic::Command,
                message: "delegated on".to_string(),
            },
            ClientNotice::ProviderRetryScheduled {
                summary: "retry 1 of 3 — zai/glm: HTTP 429".to_string(),
                detail: "{\n  \"error\": \"rate limited\"\n}".to_string(),
            },
            ClientNotice::OauthPending {
                message: "Sign in to continue.".to_string(),
                url: "https://example.test/device".to_string(),
                user_code: "ABCD-1234".to_string(),
            },
            ClientNotice::OauthAuthorized {
                provider: "zai".to_string(),
            },
            ClientNotice::OauthConnectFailed {
                provider: "zai".to_string(),
                message: "bad credentials".to_string(),
            },
            ClientNotice::CatalogSyncWarning {
                provider: "zai".to_string(),
                message: "timeout".to_string(),
                refused: false,
            },
            ClientNotice::LinkOpenFailed {
                url: "https://example.test".to_string(),
                error: "no browser".to_string(),
            },
            ClientNotice::SteerCancelRaced,
            ClientNotice::SteerMissedRoundRestored,
            ClientNotice::SteerContentLost,
        ];

        for entry in &entries {
            let rendered = entry.render();
            assert!(
                !rendered.title.trim().is_empty(),
                "{entry:?} rendered an empty title"
            );
            assert!(
                !rendered.title.contains('\n'),
                "{entry:?} title must be one line (the dialog truncates to one row)"
            );
            assert!(
                !rendered.topic.trim().is_empty() && rendered.topic != "notification",
                "{entry:?} must name a real topic, never the generic fallback (got {:?})",
                rendered.topic
            );
        }
    }

    /// A fault interrupts; an acknowledgement does not. The catalog must place
    /// each kind on the right side of that line, or the bubble/badge behaviour
    /// silently diverges from the log content.
    #[test]
    fn catalog_severity_matches_the_interruption_contract() {
        for entry in [
            ClientNotice::LinkSendFailed,
            ClientNotice::LinkLost { reason: "x".into() },
            ClientNotice::SessionPrepFailed { detail: "x".into() },
            ClientNotice::AttachSessionGone { detail: "x".into() },
            ClientNotice::AttachSessionUnresumable { detail: "x".into() },
            ClientNotice::LocalFault {
                topic: ClientTopic::Copy,
                message: "x".into(),
            },
            ClientNotice::ProviderRetryScheduled {
                summary: "retry".into(),
                detail: "body".into(),
            },
            ClientNotice::OauthConnectFailed {
                provider: "p".into(),
                message: "m".into(),
            },
            ClientNotice::CatalogSyncWarning {
                provider: "p".into(),
                message: "m".into(),
                refused: true,
            },
            ClientNotice::LinkOpenFailed {
                url: "u".into(),
                error: "e".into(),
            },
            ClientNotice::SteerCancelRaced,
            ClientNotice::SteerContentLost,
        ] {
            assert!(
                entry.render().severity.is_fault(),
                "{entry:?} is a fault and must interrupt"
            );
        }

        for entry in [
            ClientNotice::QueuedWhilePreparing,
            ClientNotice::LocalAck {
                topic: ClientTopic::Command,
                message: "x".into(),
            },
            ClientNotice::OauthPending {
                message: "m".into(),
                url: "u".into(),
                user_code: String::new(),
            },
            ClientNotice::OauthAuthorized {
                provider: "p".into(),
            },
            ClientNotice::SteerMissedRoundRestored,
        ] {
            assert!(
                !entry.render().severity.is_fault(),
                "{entry:?} is an acknowledgement and must never raise the fault badge"
            );
        }
    }

    /// Every closed topic renders a stable, distinct label — a synonym creeping
    /// in is exactly the drift the closed vocabulary exists to prevent.
    #[test]
    fn client_topics_are_distinct_and_nonempty() {
        let topics = [
            ClientTopic::Link,
            ClientTopic::Attach,
            ClientTopic::Client,
            ClientTopic::Copy,
            ClientTopic::Command,
            ClientTopic::Images,
            ClientTopic::Retry,
            ClientTopic::Oauth,
            ClientTopic::Models,
        ];
        let mut seen = std::collections::HashSet::new();
        for topic in topics {
            let label = topic.label();
            assert!(!label.is_empty());
            assert!(seen.insert(label), "duplicate topic label: {label}");
        }
    }

    #[test]
    fn a_routed_notice_takes_its_topic_from_the_wire_vocabulary() {
        let notice = nuo_tool::AgentNotice::trust_changed("Workspace configurations changed")
            .with_body("Changed on disk.");
        let rendered = RenderedNotice::from_core(&notice);
        assert_eq!(rendered.topic, "trust");
        assert_eq!(rendered.severity, NoticeSeverity::Warning);
        assert_eq!(rendered.title, "Workspace configurations changed");
        assert_eq!(rendered.detail.as_deref(), Some("Changed on disk."));
    }
}
