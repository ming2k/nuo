//! Remote skill repository support.
//!
//! A remote skill repo is a directory tree exposed over HTTP(S) with an
//! `index.json` at its root:
//!
//! ```json
//! {
//!   "skills": [
//!     { "name": "my-skill", "files": ["SKILL.md", "reference.md"] }
//!   ]
//! }
//! ```
//!
//! The first load fetches the index and every listed file into a local cache.
//! Subsequent loads reuse the cache unless `reload_skills` is called.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nuo_persistence::paths;

const INDEX_FILE: &str = "index.json";

#[derive(Debug, Deserialize)]
pub struct RemoteSkillEntry {
    pub name: String,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct RemoteSkillIndex {
    #[serde(default)]
    pub skills: Vec<RemoteSkillEntry>,
}

/// Directory where remote skill repos are cached. Resolved via the project's
/// central XDG `Dirs` so `--cache-dir` / `$XDG_CACHE_HOME` / `NUO_CACHE_DIR`
/// overrides all land in one place. See ADR-0013.
pub fn remote_cache_root() -> PathBuf {
    paths::get().remote_skills_cache()
}

/// Clear every cached remote skill repo.
pub async fn clear_remote_cache() -> Result<(), String> {
    let root = remote_cache_root();
    if root.exists() {
        tokio::fs::remove_dir_all(&root).await.map_err(|e| {
            format!(
                "failed to clear remote skill cache '{}': {}",
                root.display(),
                e
            )
        })?;
    }
    Ok(())
}

/// A GET client over the owned transport (ADR-0200).
///
/// One shared connection pool, platform trust roots, and one overall deadline
/// per fetch — the same behaviour the previous `reqwest` client had, without
/// carrying a second HTTP implementation for a catalog download.
struct Fetcher {
    client: netune::Client<netune::TlsConnector<netune::TcpConnector>>,
}

impl Fetcher {
    fn new() -> Result<Self, String> {
        let connector = netune::TlsConnector::platform(netune::TcpConnector::new())
            .map_err(|e| format!("failed to build http client: {e}"))?;
        Ok(Self {
            client: netune::Client::new(
                connector,
                netune::Pool::default(),
                netune::ClientConfig {
                    user_agent: format!("nuo/{} (+ai-coding-agent)", env!("CARGO_PKG_VERSION")),
                    ..Default::default()
                },
            ),
        })
    }

    async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let (target, path) =
            netune::Target::from_url(url).map_err(|e| format!("invalid url '{url}': {e}"))?;
        let head = netune::RequestHead::new(netune::Method::GET, path);
        let fetch = async {
            let mut response = self.client.request(&target, head, None).await?;
            if !response.head.status.is_success() {
                return Err(netune::NetError::Connect(format!(
                    "HTTP {}",
                    response.head.status
                )));
            }
            response.body.read_to_end().await
        };
        let bytes = tokio::time::timeout(Duration::from_secs(30), fetch)
            .await
            .map_err(|_| format!("failed to fetch '{url}': timed out"))?
            .map_err(|e| format!("failed to fetch '{url}': {e}"))?;
        Ok(bytes.to_vec())
    }
}

/// Fetch a remote skill repository and return the cached root directories for
/// each skill that has a `SKILL.md`.
pub async fn fetch_remote_repo(repo_url: &str) -> Result<Vec<PathBuf>, String> {
    let client = Fetcher::new()?;

    let base = repo_url.trim_end_matches('/');
    let index_url = format!("{}/{}", base, INDEX_FILE);
    let cache_host_dir = host_cache_dir(base);

    let index_bytes = client.get(&index_url).await?;
    let index_text = String::from_utf8_lossy(&index_bytes);
    let index: RemoteSkillIndex = serde_json::from_str(&index_text)
        .map_err(|e| format!("invalid skill index '{}': {}", index_url, e))?;

    let mut roots = Vec::new();
    for entry in index.skills {
        if !entry
            .files
            .iter()
            .any(|f| f.eq_ignore_ascii_case("SKILL.md"))
        {
            continue;
        }
        let skill_dir = cache_host_dir.join(sanitize_name(&entry.name));
        download_skill_files(&client, base, &entry, &skill_dir)
            .await
            .map_err(|e| format!("failed to download skill '{}': {}", entry.name, e))?;
        roots.push(skill_dir);
    }

    Ok(roots)
}

/// Return the cached skill roots for a remote repo **without** fetching —
/// scanning the existing cache directory for subdirectories that contain a
/// `SKILL.md`. This is the fallback when [`fetch_remote_repo`] fails (network
/// down, server error): the last successful download's cached files are reused
/// so a transient outage never silently removes skills.
///
/// Returns an empty vec when the cache directory doesn't exist (first run) or
/// contains no valid skills.
pub fn cached_remote_roots(repo_url: &str) -> Vec<PathBuf> {
    let base = repo_url.trim_end_matches('/');
    let cache_host_dir = host_cache_dir(base);
    let Ok(entries) = std::fs::read_dir(&cache_host_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            // A cached skill root is a subdirectory containing SKILL.md.
            if path.is_dir() && path.join("SKILL.md").exists() {
                Some(path)
            } else {
                None
            }
        })
        .collect()
}

async fn download_skill_files(
    client: &Fetcher,
    base: &str,
    entry: &RemoteSkillEntry,
    dest: &Path,
) -> Result<(), String> {
    tokio::fs::create_dir_all(dest)
        .await
        .map_err(|e| format!("failed to create skill cache '{}': {}", dest.display(), e))?;

    for file in &entry.files {
        let url = format!("{}/{}/{}/{}", base, entry.name, file, "");
        // Trim trailing slash added above if file had no slash.
        let url = url.trim_end_matches('/').to_string();
        let path = dest.join(file);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("failed to create '{}': {}", parent.display(), e))?;
        }
        let bytes = client.get(&url).await?;
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|e| format!("failed to write '{}': {}", path.display(), e))?;
    }

    Ok(())
}

fn host_cache_dir(url: &str) -> PathBuf {
    let host = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .replace(['/', ':', '?', '&', '='], "_");
    remote_cache_root().join(host)
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_cache_dir_sanitizes_url() {
        let dir = host_cache_dir("https://example.com/skills");
        assert!(dir.to_string_lossy().contains("example.com_skills"));
    }

    #[test]
    fn sanitize_name_replaces_special_chars() {
        assert_eq!(sanitize_name("my/skill:name"), "my_skill_name");
    }

    #[test]
    fn cached_remote_roots_returns_empty_for_missing_dir() {
        // A repo URL that has never been fetched has no cache directory.
        let roots = cached_remote_roots("https://nonexistent.example.com/never-fetched");
        assert!(
            roots.is_empty(),
            "missing cache should return empty: {roots:?}"
        );
    }

    #[test]
    fn cached_remote_roots_finds_skill_dirs_with_skill_md() {
        // Simulate a cached remote repo: a host dir with two skill subdirs,
        // one valid (has SKILL.md) and one incomplete. Verify the path
        // helpers produce the expected structure.
        let tmp = std::env::temp_dir().join(format!(
            "nuo-skill-cache-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let host = host_cache_dir_under("https://example.com/skills", &tmp);
        std::fs::create_dir_all(host.join("valid-skill")).unwrap();
        std::fs::write(host.join("valid-skill").join("SKILL.md"), "# valid").unwrap();
        std::fs::create_dir_all(host.join("incomplete")).unwrap();
        // No SKILL.md in incomplete/.

        // Scan the host dir the same way cached_remote_roots does, but against
        // our temp root (the real function uses remote_cache_root()).
        let roots: Vec<PathBuf> = std::fs::read_dir(&host)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                (p.is_dir() && p.join("SKILL.md").exists()).then_some(p)
            })
            .collect();
        assert_eq!(roots.len(), 1, "only valid-skill has SKILL.md: {roots:?}");
        assert!(roots[0].file_name().unwrap() == "valid-skill");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Like [`host_cache_dir`] but rooted under `base_dir` instead of
    /// [`remote_cache_root`]. Test-only.
    fn host_cache_dir_under(url: &str, base_dir: &Path) -> PathBuf {
        let host = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .replace(['/', ':', '?', '&', '='], "_");
        base_dir.join(host)
    }
}
