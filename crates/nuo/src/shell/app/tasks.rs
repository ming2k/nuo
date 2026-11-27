use super::App;
use super::workspaces::ThreadInstance;
use nuo_view::chrome::tasks_bar::BackgroundTaskItem;

impl ThreadInstance {
    pub fn upsert_background_task(&mut self, id: String, label: String, started_at_ms: u64) {
        if let Some(task) = self.background_tasks.iter_mut().find(|t| t.id == id) {
            task.label = label;
            task.running = true;
            task.dismissed = false;
        } else {
            self.background_tasks.push(BackgroundTaskItem {
                id,
                label,
                running: true,
                started_at_ms,
                duration_secs: None,
                success: None,
                exit_code: None,
                dismissed: false,
            });
        }
    }

    pub fn complete_background_task(
        &mut self,
        id: &str,
        success: bool,
        exit_code: Option<i32>,
        duration_secs: u64,
    ) {
        if let Some(task) = self.background_tasks.iter_mut().find(|t| t.id == id) {
            task.running = false;
            task.success = Some(success);
            task.exit_code = exit_code;
            task.duration_secs = Some(duration_secs);
            task.dismissed = false;
        }
    }

    pub fn dismiss_settled_background_tasks(&mut self) -> bool {
        let had_settled = self
            .background_tasks
            .iter()
            .any(|t| !t.running && !t.dismissed);
        self.background_tasks.retain(|t| t.running);
        had_settled
    }

    pub fn has_settled_background_tasks(&self) -> bool {
        self.background_tasks
            .iter()
            .any(|t| !t.running && !t.dismissed)
    }

    pub fn has_visible_background_tasks(&self) -> bool {
        self.background_tasks.iter().any(|t| !t.dismissed)
    }
}

impl App {
    pub fn upsert_background_task(&mut self, id: String, label: String, started_at_ms: u64) {
        self.threads
            .upsert_background_task(id, label, started_at_ms);
    }

    pub fn complete_background_task(
        &mut self,
        id: &str,
        success: bool,
        exit_code: Option<i32>,
        duration_secs: u64,
    ) {
        self.threads
            .complete_background_task(id, success, exit_code, duration_secs);
    }

    pub fn dismiss_settled_background_tasks(&mut self) -> bool {
        self.threads.dismiss_settled_background_tasks()
    }

    pub fn has_settled_background_tasks(&self) -> bool {
        self.threads.has_settled_background_tasks()
    }

    pub fn has_visible_background_tasks(&self) -> bool {
        self.threads.has_visible_background_tasks()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_background_task_lifecycle_in_app() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        assert!(!app.has_visible_background_tasks());
        assert!(!app.has_settled_background_tasks());

        app.upsert_background_task("job-1".to_string(), "cargo build".to_string(), 1000);
        assert!(app.has_visible_background_tasks());
        assert!(!app.has_settled_background_tasks());
        assert_eq!(app.threads.background_tasks.len(), 1);
        assert!(app.threads.background_tasks[0].running);

        app.complete_background_task("job-1", true, Some(0), 42);
        assert!(app.has_visible_background_tasks());
        assert!(app.has_settled_background_tasks());
        assert!(!app.threads.background_tasks[0].running);
        assert_eq!(app.threads.background_tasks[0].duration_secs, Some(42));

        let dismissed = app.dismiss_settled_background_tasks();
        assert!(dismissed);
        assert!(!app.has_visible_background_tasks());
        assert!(!app.has_settled_background_tasks());
        assert!(app.threads.background_tasks.is_empty());
    }
}
