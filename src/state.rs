use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        RwLock,
    },
    time::Instant,
};

use anyhow::{Context, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{
    config::{Config, ToolPaths},
    downloader::Downloader,
    format::{QualityOption, YtdlpInfo},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Admin,
    Allowed,
    Denied,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccessFile {
    #[serde(default)]
    pub admins: Vec<i64>,
    #[serde(default)]
    pub allowed: Vec<i64>,
}

pub struct AccessStore {
    path: PathBuf,
    state: RwLock<AccessFile>,
}

impl AccessStore {
    pub fn load(path: PathBuf, seed_admins: &[i64], seed_allowed: &[i64]) -> Result<Self> {
        let mut state = if path.exists() {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            serde_json::from_str(&text).with_context(|| {
                format!(
                    "invalid access state in {}; fix or delete it to re-run first-user bootstrap",
                    path.display()
                )
            })?
        } else {
            AccessFile::default()
        };

        for id in seed_admins {
            if !state.admins.contains(id) {
                state.admins.push(*id);
            }
        }
        for id in seed_allowed {
            if !state.allowed.contains(id) && !state.admins.contains(id) {
                state.allowed.push(*id);
            }
        }

        let store = Self {
            path,
            state: RwLock::new(state),
        };
        {
            let state = store.state.read().unwrap();
            store.persist(&state)?;
        }
        Ok(store)
    }

    pub fn check(&self, user_id: i64) -> Access {
        let state = self.state.read().unwrap();
        if state.admins.contains(&user_id) {
            Access::Admin
        } else if state.allowed.contains(&user_id) {
            Access::Allowed
        } else {
            Access::Denied
        }
    }

    pub fn is_admin(&self, user_id: i64) -> bool {
        self.state.read().unwrap().admins.contains(&user_id)
    }

    pub fn claim_admin_if_empty(&self, user_id: i64) -> Result<bool> {
        self.update(|state| {
            if !state.admins.is_empty() {
                false
            } else {
                state.admins.push(user_id);
                state.allowed.retain(|id| *id != user_id);
                true
            }
        })
    }

    pub fn allow(&self, user_id: i64) -> Result<bool> {
        self.update(|state| {
            if state.admins.contains(&user_id) || state.allowed.contains(&user_id) {
                false
            } else {
                state.allowed.push(user_id);
                true
            }
        })
    }

    pub fn deny(&self, user_id: i64) -> Result<bool> {
        self.update(|state| {
            if state.admins.contains(&user_id) {
                false
            } else {
                let before = state.allowed.len();
                state.allowed.retain(|id| *id != user_id);
                state.allowed.len() != before
            }
        })
    }

    pub fn list(&self) -> (Vec<i64>, Vec<i64>) {
        let state = self.state.read().unwrap();
        (state.admins.clone(), state.allowed.clone())
    }

    fn update(&self, apply: impl FnOnce(&mut AccessFile) -> bool) -> Result<bool> {
        let mut state = self.state.write().unwrap();
        let previous = state.clone();
        if !apply(&mut state) {
            return Ok(false);
        }
        if let Err(error) = self.persist(&state) {
            *state = previous;
            return Err(error);
        }
        Ok(true)
    }

    fn persist(&self, state: &AccessFile) -> Result<()> {
        let data = serde_json::to_vec_pretty(state)?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, data).with_context(|| format!("cannot write {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("cannot replace {}", self.path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

pub struct ActiveJobs {
    count: AtomicUsize,
    path: PathBuf,
}

impl ActiveJobs {
    pub fn new(path: PathBuf) -> Self {
        let jobs = Self {
            count: AtomicUsize::new(0),
            path,
        };
        jobs.write();
        jobs
    }

    pub fn inc(&self) -> usize {
        let count = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        self.write();
        count
    }

    pub fn dec(&self) -> usize {
        let count = self.count.fetch_sub(1, Ordering::SeqCst).saturating_sub(1);
        self.write();
        count
    }

    pub fn get(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    fn write(&self) {
        let _ = fs::write(&self.path, self.get().to_string());
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct VersionInfo {
    #[serde(default)]
    pub build_tag: String,
    #[serde(default)]
    pub bot_version: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub built_at: String,
    #[serde(default)]
    pub yt_dlp: String,
    #[serde(default)]
    pub ffmpeg: String,
    #[serde(default)]
    pub yt_dlp_sig: String,
    #[serde(default)]
    pub ffmpeg_sig: String,
    #[serde(default)]
    pub ffmpeg_asset: String,
}

impl VersionInfo {
    pub fn load(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }
}

pub struct PendingVideo {
    pub user_id: i64,
    pub url: String,
    pub info: YtdlpInfo,
    pub qualities: Vec<QualityOption>,
    pub audio_estimate: Option<u64>,
    pub created_at: Instant,
}

pub struct JobHandle {
    pub user_id: i64,
    pub cancel: CancellationToken,
}

pub struct AppState {
    pub config: Config,
    pub access: AccessStore,
    pub pending: DashMap<String, PendingVideo>,
    pub jobs: DashMap<String, JobHandle>,
    pub user_jobs: DashMap<i64, String>,
    pub semaphore: std::sync::Arc<Semaphore>,
    pub active: ActiveJobs,
    pub started_at: Instant,
    pub versions: Option<VersionInfo>,
    pub downloader: Downloader,
}

impl AppState {
    pub async fn new(config: Config, tools: ToolPaths) -> Result<Self> {
        tokio::fs::create_dir_all(&config.data_dir)
            .await
            .with_context(|| {
                format!("cannot create data directory {}", config.data_dir.display())
            })?;

        let tmp_dir = config.data_dir.join("tmp");
        if tmp_dir.exists() {
            let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        }
        tokio::fs::create_dir_all(&tmp_dir)
            .await
            .with_context(|| format!("cannot create {}", tmp_dir.display()))?;

        let access = AccessStore::load(
            config.data_dir.join("state.json"),
            &config.admin_ids,
            &config.allowed_users,
        )?;
        let active = ActiveJobs::new(config.data_dir.join("active_jobs"));
        let downloader = Downloader::new(config.clone(), tools.clone());
        let versions = VersionInfo::load(&exe_dir().join("versions.json"));
        let permits = config.max_concurrent_downloads;

        Ok(Self {
            config,
            access,
            pending: DashMap::new(),
            jobs: DashMap::new(),
            user_jobs: DashMap::new(),
            semaphore: std::sync::Arc::new(Semaphore::new(permits)),
            active,
            started_at: Instant::now(),
            versions,
            downloader,
        })
    }
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_state_path() -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "ytdl-bot-state-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        dir.join("state.json")
    }

    #[test]
    fn first_user_becomes_admin_and_is_persisted() {
        let path = temp_state_path();
        let store = AccessStore::load(path.clone(), &[], &[]).expect("load");
        assert_eq!(store.check(1), Access::Denied);
        assert!(store.claim_admin_if_empty(1).expect("claim"));
        assert_eq!(store.check(1), Access::Admin);
        assert!(!store.claim_admin_if_empty(2).expect("claim again"));
        assert_eq!(store.check(2), Access::Denied);

        let reloaded = AccessStore::load(path.clone(), &[], &[]).expect("reload");
        assert_eq!(reloaded.check(1), Access::Admin);
        assert_eq!(reloaded.list().0, vec![1]);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn allow_and_deny_are_persisted() {
        let path = temp_state_path();
        let store = AccessStore::load(path.clone(), &[], &[10]).expect("load");
        assert_eq!(store.check(10), Access::Allowed);
        assert!(store.allow(20).expect("allow"));
        assert!(!store.allow(20).expect("allow twice"));

        let reloaded = AccessStore::load(path.clone(), &[], &[]).expect("reload");
        assert_eq!(reloaded.check(20), Access::Allowed);

        assert!(reloaded.deny(20).expect("deny"));
        assert_eq!(reloaded.check(20), Access::Denied);
        assert!(!reloaded.is_admin(20));

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_state_is_rejected() {
        let path = temp_state_path();
        fs::write(&path, b"not json").expect("write");
        assert!(AccessStore::load(path.clone(), &[], &[]).is_err());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
