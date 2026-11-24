use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/ytdl-bot/config.toml";

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub bot_token: String,
    #[serde(default)]
    pub admin_ids: Vec<i64>,
    #[serde(default)]
    pub allowed_users: Vec<i64>,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_downloads: usize,
    #[serde(default = "default_max_upload_mb")]
    pub max_upload_mb: u64,
    #[serde(default = "default_max_video_height")]
    pub max_video_height: u32,
    #[serde(default = "default_max_duration_secs")]
    pub max_duration_secs: u64,
    #[serde(default = "default_download_timeout_secs")]
    pub download_timeout_secs: u64,
    #[serde(default = "default_info_timeout_secs")]
    pub info_timeout_secs: u64,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub cookies_file: Option<PathBuf>,
    #[serde(default)]
    pub ytdlp_path: Option<PathBuf>,
    #[serde(default)]
    pub ffmpeg_path: Option<PathBuf>,
    #[serde(default)]
    pub ffprobe_path: Option<PathBuf>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read config file {}", path.display()))?;
        let mut config: Self = toml::from_str(&text)
            .with_context(|| format!("invalid config file {}", path.display()))?;

        if let Ok(token) = std::env::var("YTDL_BOT_TOKEN") {
            if !token.trim().is_empty() {
                config.bot_token = token;
            }
        }

        if config.bot_token.trim().is_empty() {
            bail!("bot_token is empty");
        }
        if !config.bot_token.contains(':') {
            bail!("bot_token does not look like a Telegram bot token");
        }

        config.max_concurrent_downloads = config.max_concurrent_downloads.clamp(1, 8);
        if config.max_upload_mb == 0 || config.max_upload_mb > 50 {
            config.max_upload_mb = 50;
        }
        if config.max_video_height == 0 {
            config.max_video_height = 1080;
        }
        if config.max_video_height > 2160 {
            config.max_video_height = 2160;
        }
        if config.max_duration_secs == 0 {
            config.max_duration_secs = 7200;
        }
        if config.download_timeout_secs == 0 {
            config.download_timeout_secs = 1800;
        }
        if config.info_timeout_secs == 0 {
            config.info_timeout_secs = 60;
        }
        if config.data_dir.as_os_str().is_empty() {
            config.data_dir = default_data_dir();
        }
        if config
            .cookies_file
            .as_ref()
            .map(|p| p.as_os_str().is_empty())
            .unwrap_or(false)
        {
            config.cookies_file = None;
        }

        Ok(config)
    }

    pub fn max_upload_bytes(&self) -> u64 {
        self.max_upload_mb.saturating_mul(1024 * 1024)
    }
}

fn default_max_concurrent() -> usize {
    2
}
fn default_max_upload_mb() -> u64 {
    50
}
fn default_max_video_height() -> u32 {
    1080
}
fn default_max_duration_secs() -> u64 {
    7200
}
fn default_download_timeout_secs() -> u64 {
    1800
}
fn default_info_timeout_secs() -> u64 {
    60
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("/var/lib/ytdl-bot")
}

#[derive(Debug, Clone)]
pub struct ToolPaths {
    pub ytdlp: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub ffmpeg_dir: PathBuf,
}

impl ToolPaths {
    pub fn discover(config: &Config) -> Result<Self> {
        let exe = std::env::current_exe().ok();
        let exe_dir = exe.as_deref().and_then(Path::parent);
        let cwd = std::env::current_dir().ok();

        let ytdlp = resolve_tool(&config.ytdlp_path, "yt-dlp", exe_dir, cwd.as_deref())
            .ok_or_else(|| tool_error("yt-dlp", &config.ytdlp_path, exe_dir, cwd.as_deref()))?;
        let ffmpeg = resolve_tool(&config.ffmpeg_path, "ffmpeg", exe_dir, cwd.as_deref())
            .ok_or_else(|| tool_error("ffmpeg", &config.ffmpeg_path, exe_dir, cwd.as_deref()))?;
        let ffprobe = resolve_tool(&config.ffprobe_path, "ffprobe", exe_dir, cwd.as_deref())
            .ok_or_else(|| tool_error("ffprobe", &config.ffprobe_path, exe_dir, cwd.as_deref()))?;

        let ffmpeg_dir = ffmpeg
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        Ok(Self {
            ytdlp,
            ffmpeg,
            ffprobe,
            ffmpeg_dir,
        })
    }
}

fn resolve_tool(
    configured: &Option<PathBuf>,
    name: &str,
    exe_dir: Option<&Path>,
    cwd: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(path) = configured {
        if !path.as_os_str().is_empty() {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                cwd.map(|dir| dir.join(path))
                    .unwrap_or_else(|| path.clone())
            };
            return is_executable(&path).then_some(path);
        }
    }

    let mut candidates = Vec::new();
    if let Some(dir) = exe_dir {
        candidates.push(dir.join("bin").join(name));
        candidates.push(dir.join(name));
    }
    if let Some(dir) = cwd {
        candidates.push(dir.join(name));
    }
    candidates.push(PathBuf::from("/usr/local/bin").join(name));
    candidates.push(PathBuf::from("/usr/bin").join(name));

    candidates.into_iter().find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
        && path
            .metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

fn tool_error(
    name: &str,
    configured: &Option<PathBuf>,
    exe_dir: Option<&Path>,
    cwd: Option<&Path>,
) -> anyhow::Error {
    let mut checked = Vec::new();
    if let Some(path) = configured {
        if !path.as_os_str().is_empty() {
            checked.push(path.display().to_string());
        }
    }
    if let Some(dir) = exe_dir {
        checked.push(dir.join("bin").join(name).display().to_string());
        checked.push(dir.join(name).display().to_string());
    }
    if let Some(dir) = cwd {
        checked.push(dir.join(name).display().to_string());
    }
    checked.push(format!("/usr/local/bin/{name}"));
    checked.push(format!("/usr/bin/{name}"));

    anyhow!(
        "{} was not found or is not executable. Checked: {}",
        name,
        checked.join(", ")
    )
}
