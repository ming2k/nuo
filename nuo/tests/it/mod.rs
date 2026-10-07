mod server_spawn;
mod lifecycle_integration;
mod retry_restore_integration;
mod serve_integration;
mod unattended_restore_integration;

pub fn sandbox_once() {
    use std::sync::Once;
    static SANDBOX: Once = Once::new();
    static KEEP: std::sync::Mutex<Option<tempfile::TempDir>> = std::sync::Mutex::new(None);
    SANDBOX.call_once(|| {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: single-writer (the Once) and set before any test body
        // spawns; the env is never mutated again in this process.
        unsafe { std::env::set_var("NUO_HOME", tmp.path()) };
        *KEEP.lock().unwrap() = Some(tmp);
    });
}
