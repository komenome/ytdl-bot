use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    config::{Config, ToolPaths},
    format::{audio_selector, video_selector, YtdlpInfo},
    progress::{parse_line, Progress},
};

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("cancelled")]
    Cancelled,
    #[error("timed out")]
    Timeout,
    #[error("file too large ({size} bytes)")]
    TooLarge { path: PathBuf, size: u64 },
    #[error("video is too long ({secs} seconds)")]
    TooLong { secs: u64 },
    #[error("YouTube requires sign-in verification")]
    BotCheck,
    #[error("private video")]
    Private,
    #[error("video unavailable")]
    Unavailable,
    #[error("age-restricted video")]
    AgeRestricted,
    #[error("live streams are not supported")]
    Live,
    #[error("requested format is not available")]
    FormatUnavailable,
    #[error("yt-dlp failed: {0}")]
    Failed(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct MediaProbe {
    pub duration_secs: Option<u64>,
    pub size: u64,
    pub bit_rate: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ToolVersions {
    pub ytdlp: String,
    pub ffmpeg: String,
    pub ffprobe: String,
}

#[derive(Clone)]
pub struct Downloader {
    ytdlp: PathBuf,
    ffprobe: PathBuf,
    ffmpeg_dir: PathBuf,
    cookies: Option<PathBuf>,
    max_upload_bytes: u64,
    max_duration_secs: u64,
    info_timeout: Duration,
    download_timeout: Duration,
}

impl Downloader {
    pub fn new(config: Config, tools: ToolPaths) -> Self {
        let cookies = config
            .cookies_file
            .as_ref()
            .filter(|path| !path.as_os_str().is_empty())
            .filter(|path| {
                if path.exists() {
                    true
                } else {
                    warn!("cookies file not found: {}", path.display());
                    false
                }
            })
            .cloned();

        Self {
            ytdlp: tools.ytdlp,
            ffprobe: tools.ffprobe,
            ffmpeg_dir: tools.ffmpeg_dir,
            cookies,
            max_upload_bytes: config.max_upload_bytes(),
            max_duration_secs: config.max_duration_secs,
            info_timeout: Duration::from_secs(config.info_timeout_secs),
            download_timeout: Duration::from_secs(config.download_timeout_secs),
        }
    }

    pub async fn fetch_info(&self, url: &str) -> Result<YtdlpInfo, DownloadError> {
        let mut command = Command::new(&self.ytdlp);
        command
            .arg("--dump-single-json")
            .arg("--no-playlist")
            .arg("--no-warnings")
            .arg("--skip-download")
            .arg("--socket-timeout")
            .arg("15")
            .arg("--retries")
            .arg("3")
            .arg("--ffmpeg-location")
            .arg(&self.ffmpeg_dir);
        if let Some(cookies) = &self.cookies {
            command.arg("--cookies").arg(cookies);
        }
        command.arg(url);

        let output = tokio::time::timeout(self.info_timeout, command.output())
            .await
            .map_err(|_| DownloadError::Timeout)??;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(classify_error(&stderr));
        }

        let info: YtdlpInfo = serde_json::from_slice(&output.stdout).map_err(|error| {
            DownloadError::Other(format!("could not parse yt-dlp metadata: {error}"))
        })?;

        if info.is_live == Some(true)
            || matches!(info.live_status.as_deref(), Some("is_live" | "is_upcoming"))
        {
            return Err(DownloadError::Live);
        }

        if let Some(duration) = info.duration {
            if duration > self.max_duration_secs as f64 {
                return Err(DownloadError::TooLong {
                    secs: duration.round() as u64,
                });
            }
        }

        Ok(info)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn download(
        &self,
        url: &str,
        title: &str,
        height: u32,
        audio: bool,
        dir: &Path,
        progress_tx: mpsc::Sender<Progress>,
        cancel: CancellationToken,
    ) -> Result<DownloadResult, DownloadError> {
        let free = self.available_space(dir);
        let required = self
            .max_upload_bytes
            .saturating_mul(3)
            .saturating_add(200 * 1024 * 1024);
        if free < required {
            return Err(DownloadError::Other(
                "not enough free disk space for this download".to_string(),
            ));
        }

        let mut command = Command::new(&self.ytdlp);
        command
            .arg("--no-playlist")
            .arg("--newline")
            .arg("--no-warnings")
            .arg("--ffmpeg-location")
            .arg(&self.ffmpeg_dir)
            .arg("--output")
            .arg(dir.join("media.%(ext)s"))
            .arg("--socket-timeout")
            .arg("15")
            .arg("--retries")
            .arg("3")
            .arg("--fragment-retries")
            .arg("3")
            .arg("--concurrent-fragments")
            .arg("4")
            .arg("--max-filesize")
            .arg(self.max_upload_bytes.saturating_mul(4).to_string())
            .arg("--progress-template")
            .arg(
                "download:PROGRESS %(progress.downloaded_bytes)s \
                 %(progress.total_bytes)s %(progress.total_bytes_estimate)s \
                 %(progress.speed)s %(progress.eta)s",
            );

        if let Some(cookies) = &self.cookies {
            command.arg("--cookies").arg(cookies);
        }

        if audio {
            command
                .arg("-f")
                .arg(audio_selector())
                .arg("-x")
                .arg("--audio-format")
                .arg("mp3")
                .arg("--audio-quality")
                .arg("0")
                .arg("--embed-metadata");
        } else {
            command
                .arg("-f")
                .arg(video_selector(height))
                .arg("--merge-output-format")
                .arg("mp4");
        }
        command.arg(url);

        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(DownloadError::Io)?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DownloadError::Other("could not capture yt-dlp stdout".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| DownloadError::Other("could not capture yt-dlp stderr".to_string()))?;

        let stdout_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if let Some(progress) = parse_line(&line) {
                    if progress_tx.send(progress).await.is_err() {
                        break;
                    }
                }
            }
        });

        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            let mut tail: VecDeque<String> = VecDeque::with_capacity(50);
            while let Ok(Some(line)) = reader.next_line().await {
                if tail.len() == 50 {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            tail.into_iter().collect::<Vec<_>>().join("\n")
        });

        enum Outcome {
            Exited(std::io::Result<std::process::ExitStatus>),
            Cancelled,
            Timeout,
        }

        let outcome = tokio::select! {
            result = child.wait() => Outcome::Exited(result),
            _ = cancel.cancelled() => Outcome::Cancelled,
            _ = tokio::time::sleep(self.download_timeout) => Outcome::Timeout,
        };

        let status = match outcome {
            Outcome::Exited(Ok(status)) => status,
            Outcome::Exited(Err(error)) => return Err(DownloadError::Io(error)),
            Outcome::Cancelled => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(DownloadError::Cancelled);
            }
            Outcome::Timeout => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(DownloadError::Timeout);
            }
        };

        let _ = stdout_task.await;
        let stderr = stderr_task.await.unwrap_or_default();

        if !status.success() {
            return Err(classify_error(&stderr));
        }

        let (path, size) = find_output(dir).await?;
        if size > self.max_upload_bytes {
            return Err(DownloadError::TooLarge { path, size });
        }

        let path = rename_output(path, title).await;
        debug!("downloaded {} ({} bytes)", path.display(), size);
        Ok(DownloadResult { path, size })
    }

    pub async fn probe(&self, path: &Path) -> Result<MediaProbe, DownloadError> {
        #[derive(Deserialize)]
        struct Probe {
            format: ProbeFormat,
        }
        #[derive(Deserialize)]
        struct ProbeFormat {
            #[serde(default)]
            duration: Option<String>,
            #[serde(default)]
            size: Option<String>,
            #[serde(default)]
            bit_rate: Option<String>,
        }

        let output = tokio::time::timeout(
            Duration::from_secs(30),
            Command::new(&self.ffprobe)
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration,size,bit_rate",
                    "-of",
                    "json",
                ])
                .arg(path)
                .output(),
        )
        .await
        .map_err(|_| DownloadError::Timeout)??;

        if !output.status.success() {
            return Err(DownloadError::Other(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }

        let parsed: Probe = serde_json::from_slice(&output.stdout).map_err(|error| {
            DownloadError::Other(format!("could not parse ffprobe output: {error}"))
        })?;

        let fallback_size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        Ok(MediaProbe {
            duration_secs: parsed
                .format
                .duration
                .and_then(|value| value.parse::<f64>().ok())
                .map(|value| value.round() as u64),
            size: parsed
                .format
                .size
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(fallback_size),
            bit_rate: parsed
                .format
                .bit_rate
                .and_then(|value| value.parse::<u64>().ok()),
        })
    }

    pub async fn tool_versions(&self, tools: &ToolPaths) -> Result<ToolVersions, DownloadError> {
        let ytdlp = run_version(&tools.ytdlp, &["--version"]).await?;
        let ffmpeg_line = run_version(&tools.ffmpeg, &["-version"]).await?;
        let ffprobe_line = run_version(&tools.ffprobe, &["-version"]).await?;
        Ok(ToolVersions {
            ytdlp,
            ffmpeg: short_ffmpeg_version(&ffmpeg_line),
            ffprobe: short_ffmpeg_version(&ffprobe_line),
        })
    }

    pub fn available_space(&self, path: &Path) -> u64 {
        fs2::available_space(path).unwrap_or(u64::MAX)
    }
}

async fn run_version(path: &Path, args: &[&str]) -> Result<String, DownloadError> {
    let output = Command::new(path)
        .args(args)
        .output()
        .await
        .map_err(DownloadError::Io)?;
    if !output.status.success() {
        return Err(DownloadError::Other(format!(
            "{} {} failed",
            path.display(),
            args.join(" ")
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next().unwrap_or("").trim().to_string();
    if line.is_empty() {
        return Err(DownloadError::Other(format!(
            "{} returned an empty version",
            path.display()
        )));
    }
    Ok(line)
}

fn short_ffmpeg_version(line: &str) -> String {
    line.split_whitespace()
        .nth(2)
        .unwrap_or(line)
        .trim()
        .to_string()
}

async fn find_output(dir: &Path) -> Result<(PathBuf, u64), DownloadError> {
    let mut entries = tokio::fs::read_dir(dir).await?;
    let mut best: Option<(u64, PathBuf)> = None;

    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.starts_with('.')
            || name.ends_with(".part")
            || name.ends_with(".ytdl")
            || name.ends_with(".json")
        {
            continue;
        }
        let size = entry.metadata().await?.len();
        if size == 0 {
            continue;
        }
        if best
            .as_ref()
            .map(|(best_size, _)| size > *best_size)
            .unwrap_or(true)
        {
            best = Some((size, path));
        }
    }

    best.map(|(size, path)| (path, size))
        .ok_or_else(|| DownloadError::Other("yt-dlp did not produce an output file".to_string()))
}

async fn rename_output(path: PathBuf, title: &str) -> PathBuf {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("mp4")
        .to_string();
    let sanitized = sanitize_filename::sanitize(title);
    let sanitized = sanitized.trim().trim_matches('.').trim();
    let sanitized: String = sanitized.chars().take(60).collect();
    let sanitized = if sanitized.is_empty() {
        "video".to_string()
    } else {
        sanitized
    };
    let new_path = path.with_file_name(format!("{sanitized}.{extension}"));
    if new_path == path {
        return path;
    }
    match tokio::fs::rename(&path, &new_path).await {
        Ok(()) => new_path,
        Err(error) => {
            warn!("could not rename output: {error}");
            path
        }
    }
}

fn classify_error(stderr: &str) -> DownloadError {
    let lower = stderr.to_ascii_lowercase();

    if lower.contains("sign in to confirm")
        || lower.contains("confirm you're not a bot")
        || lower.contains("confirm you are not a bot")
        || lower.contains("not a bot")
    {
        DownloadError::BotCheck
    } else if lower.contains("private video") {
        DownloadError::Private
    } else if lower.contains("age-restricted")
        || lower.contains("age restricted")
        || lower.contains("confirm your age")
    {
        DownloadError::AgeRestricted
    } else if lower.contains("live event")
        || lower.contains("is live")
        || lower.contains("live stream")
    {
        DownloadError::Live
    } else if lower.contains("requested format is not available")
        || lower.contains("no video formats found")
    {
        DownloadError::FormatUnavailable
    } else if lower.contains("video unavailable")
        || lower.contains("this video is unavailable")
        || lower.contains("has been removed")
    {
        DownloadError::Unavailable
    } else if lower.contains("max-filesize") {
        DownloadError::Other("the file is larger than the safe download limit".to_string())
    } else if lower.contains("http error 429") {
        DownloadError::Other(
            "YouTube rate-limited this server. Try again later or add cookies.".to_string(),
        )
    } else {
        let tail = tail_chars(stderr, 500);
        if tail.is_empty() {
            DownloadError::Failed("yt-dlp failed without error output".to_string())
        } else {
            DownloadError::Failed(tail)
        }
    }
}

fn tail_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        text.trim().to_string()
    } else {
        chars[chars.len() - max..]
            .iter()
            .collect::<String>()
            .trim()
            .to_string()
    }
}
