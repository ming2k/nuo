//! Pure scene and tab identity types shared by `nuo_view::view_header` and the
//! interactive surface router (`[INV-ARCH-VIEW-ZERO-IO]`).

/// Root full-screen scene identifier (closed set of destinations).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SceneKind {
    /// The live thread: transcript + composer. The default workspace.
    #[default]
    Thread,
    /// The session and cluster dashboard (`/dashboard`).
    Dashboard,
    /// The full-screen settings center (`/config` / `/settings`).
    Settings,
    /// Deep-dive inspection into a spawned subagent's execution stream.
    Subagent,
    /// One command step's whole log, opened from the transcript's fold block
    /// (`v` / `view full log`) as a peer of the transcript rather than a floating
    /// dialog over it (ADR-0116 `[INV-LAYER-01]`).
    CommandLog,
    /// Fullscreen diff viewer for session file changes.
    Diff,
}

impl SceneKind {
    /// Human label: palette section titles and context headers.
    pub fn label(self) -> &'static str {
        match self {
            Self::Thread => "Thread",
            Self::Dashboard => "Session dashboard",
            Self::Settings => "Settings",
            Self::Subagent => "Subagent",
            Self::CommandLog => "Command log",
            Self::Diff => "Diff viewer",
        }
    }

    /// The scene-head label (ADR-0040, ADR-0042).
    pub fn scene_label(self) -> &'static str {
        match self {
            Self::Thread => "thread",
            Self::Dashboard => "dashboard",
            Self::Settings => "settings",
            Self::Subagent => "subagent",
            Self::CommandLog => "log",
            Self::Diff => "diff",
        }
    }

    /// The breadcrumb label shown in the tab navigation bar (ADR-0040, ADR-0042).
    pub fn breadcrumb_label(self) -> &'static str {
        self.scene_label()
    }

    /// Whether this scene *is* content the user reads rather than a workspace
    /// they act in — the property that decides whether the chat pane and its
    /// composer are hidden behind it.
    pub fn is_reading_scene(self) -> bool {
        matches!(self, Self::CommandLog | Self::Diff)
    }
}

/// Discriminator for a client tab container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabKind {
    Thread(String),
    Dashboard,
    Settings,
}

/// One client-owned tab in the viewport strip (ADR-0039 `[INV-TAB-01]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTab {
    pub kind: TabKind,
    /// Authentic semantic title of the tab, if established (ADR-0050 `[INV-TITLE-01, INV-TAB-08]`).
    pub title: Option<String>,
    /// Tab-autonomous navigation history stack (ADR-0040 `[INV-HEAD-02]`).
    pub history: Vec<SceneKind>,
    /// Pointer within `history` for Back/Forward navigation.
    pub cursor: usize,
}

impl ClientTab {
    pub fn thread(id: impl Into<String>, title: Option<impl Into<String>>) -> Self {
        let id_str = id.into();
        Self {
            kind: TabKind::Thread(id_str),
            title: title
                .map(|t| t.into())
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
            history: vec![SceneKind::Thread],
            cursor: 0,
        }
    }

    pub fn dashboard() -> Self {
        Self {
            kind: TabKind::Dashboard,
            title: Some("Dashboard".to_string()),
            history: vec![SceneKind::Dashboard],
            cursor: 0,
        }
    }

    pub fn settings() -> Self {
        Self {
            kind: TabKind::Settings,
            title: Some("Settings".to_string()),
            history: vec![SceneKind::Settings],
            cursor: 0,
        }
    }

    pub fn label(&self) -> &str {
        match &self.kind {
            TabKind::Thread(_) => self.title.as_deref().unwrap_or("thread"),
            TabKind::Dashboard => "Dashboard",
            TabKind::Settings => "Settings",
        }
    }

    /// Return the authentic semantic display title, or None if untitled (ADR-0050 `[INV-TITLE-01, INV-TAB-08]`).
    pub fn display_title(&self) -> Option<&str> {
        match &self.kind {
            TabKind::Thread(_) => self.title.as_deref(),
            TabKind::Dashboard => Some("dashboard"),
            TabKind::Settings => Some("settings"),
        }
    }

    pub fn is_thread(&self) -> bool {
        matches!(self.kind, TabKind::Thread(_))
    }

    pub fn thread_id(&self) -> Option<&str> {
        match &self.kind {
            TabKind::Thread(id) => Some(id.as_str()),
            _ => None,
        }
    }

    pub fn current_scene(&self) -> SceneKind {
        self.history
            .get(self.cursor)
            .copied()
            .unwrap_or(match &self.kind {
                TabKind::Thread(_) => SceneKind::Thread,
                TabKind::Dashboard => SceneKind::Dashboard,
                TabKind::Settings => SceneKind::Settings,
            })
    }

    pub fn can_back(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_forward(&self) -> bool {
        self.cursor + 1 < self.history.len()
    }

    /// Whether this tab container accepts `scene` under `[INV-SCENE-04]` (ADR-0075).
    pub fn accepts_scene(&self, scene: SceneKind) -> bool {
        match &self.kind {
            TabKind::Thread(_) => matches!(
                scene,
                SceneKind::Thread | SceneKind::Subagent | SceneKind::CommandLog | SceneKind::Diff
            ),
            TabKind::Dashboard => matches!(scene, SceneKind::Dashboard),
            TabKind::Settings => matches!(scene, SceneKind::Settings),
        }
    }

    /// Push a scene onto this tab's history stack if allowed by container discipline.
    /// Returns `false` when the scene was rejected as cross-container pollution.
    pub fn push_scene(&mut self, scene: SceneKind) -> bool {
        if self.current_scene() == scene {
            return true;
        }
        if !self.accepts_scene(scene) {
            return false;
        }
        self.history.truncate(self.cursor + 1);
        self.history.push(scene);
        self.cursor = self.history.len() - 1;
        true
    }

    pub fn back(&mut self) -> Option<SceneKind> {
        if self.can_back() {
            self.cursor -= 1;
            Some(self.current_scene())
        } else {
            None
        }
    }

    pub fn forward(&mut self) -> Option<SceneKind> {
        if self.can_forward() {
            self.cursor += 1;
            Some(self.current_scene())
        } else {
            None
        }
    }

    pub fn breadcrumbs(&self) -> &[SceneKind] {
        &self.history[..=self.cursor]
    }
}
