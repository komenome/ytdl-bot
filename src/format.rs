use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use url::Url;

#[derive(Debug, Clone, Deserialize)]
pub struct YtdlpInfo {
    pub title: String,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub uploader: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub is_live: Option<bool>,
    #[serde(default)]
    pub live_status: Option<String>,
    #[serde(default)]
    pub formats: Vec<Format>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Format {
    #[serde(default)]
    pub ext: String,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub vcodec: Option<String>,
    #[serde(default)]
    pub acodec: Option<String>,
    #[serde(default)]
    pub filesize: Option<u64>,
    #[serde(default)]
    pub filesize_approx: Option<u64>,
    #[serde(default)]
    pub tbr: Option<f64>,
    #[serde(default)]
    pub abr: Option<f64>,
}

impl Format {
    pub fn is_video(&self) -> bool {
        self.vcodec
            .as_deref()
            .map(|codec| !codec.is_empty() && codec != "none")
            .unwrap_or(false)
    }

    pub fn is_audio(&self) -> bool {
        self.acodec
            .as_deref()
            .map(|codec| !codec.is_empty() && codec != "none")
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct QualityOption {
    pub height: u32,
    pub width: Option<u32>,
    pub estimated_bytes: Option<u64>,
    pub too_large: bool,
}

impl QualityOption {
    pub fn size_label(&self, max_upload_bytes: u64) -> String {
        match self.estimated_bytes {
            Some(bytes) => {
                let label = human_size(bytes);
                if self.too_large || bytes > max_upload_bytes {
                    format!("{label} ⚠️")
                } else {
                    label
                }
            }
            None => "size ?".to_string(),
        }
    }
}

pub fn build_quality_options(
    info: &YtdlpInfo,
    max_height: u32,
    max_upload_bytes: u64,
) -> Vec<QualityOption> {
    let mut heights: Vec<u32> = info
        .formats
        .iter()
        .filter(|format| format.is_video())
        .filter_map(|format| format.height)
        .filter(|height| *height > 0 && *height <= max_height)
        .collect();
    heights.sort_unstable();
    heights.dedup();
    heights.reverse();
    heights.truncate(6);

    let audio = best_audio(&info.formats);
    let mut options = Vec::new();

    for height in heights {
        let Some(video) = best_video_for_height(&info.formats, height) else {
            continue;
        };
        let video_size = estimate_format_size(video, info.duration);
        let total = if video.is_audio() {
            video_size
        } else {
            match (
                video_size,
                audio.and_then(|format| estimate_format_size(format, info.duration)),
            ) {
                (Some(video_size), Some(audio_size)) => Some(video_size + audio_size),
                _ => None,
            }
        };
        let estimated_bytes = total.map(|bytes| bytes + bytes / 32);
        options.push(QualityOption {
            height,
            width: video.width,
            estimated_bytes,
            too_large: estimated_bytes
                .map(|bytes| bytes > max_upload_bytes)
                .unwrap_or(false),
        });
    }

    options
}

pub fn estimate_audio(info: &YtdlpInfo) -> Option<u64> {
    let format = best_audio(&info.formats)?;
    let size = estimate_format_size(format, info.duration)?;
    Some(size + size / 20)
}

fn best_video_for_height(formats: &[Format], height: u32) -> Option<&Format> {
    let mut candidates: Vec<&Format> = formats
        .iter()
        .filter(|format| format.is_video() && format.height == Some(height))
        .collect();
    candidates.sort_by_key(|format| std::cmp::Reverse(video_score(format)));
    candidates.first().copied()
}

fn video_score(format: &Format) -> (bool, bool, bool, i64) {
    let avc1 = format
        .vcodec
        .as_deref()
        .map(|codec| codec.starts_with("avc1"))
        .unwrap_or(false);
    let mp4 = format.ext == "mp4";
    let video_only = format
        .acodec
        .as_deref()
        .map(|codec| codec == "none")
        .unwrap_or(false);
    let bitrate = format.tbr.unwrap_or(0.0) as i64;
    (avc1, mp4, video_only, bitrate)
}

fn best_audio(formats: &[Format]) -> Option<&Format> {
    let mut candidates: Vec<&Format> = formats
        .iter()
        .filter(|format| format.is_audio() && !format.is_video())
        .collect();
    candidates.sort_by_key(|format| std::cmp::Reverse(audio_score(format)));
    candidates.first().copied()
}

fn audio_score(format: &Format) -> (bool, i64) {
    let m4a = format.ext == "m4a";
    let bitrate = format.abr.or(format.tbr).unwrap_or(0.0) as i64;
    (m4a, bitrate)
}

fn estimate_format_size(format: &Format, duration: Option<f64>) -> Option<u64> {
    if let Some(size) = format
        .filesize
        .or(format.filesize_approx)
        .filter(|size| *size > 0)
    {
        return Some(size);
    }

    let bitrate_kbps = format.tbr.or(format.abr)?;
    let duration = duration?;
    if duration <= 0.0 {
        return None;
    }
    Some((bitrate_kbps * 1000.0 / 8.0 * duration) as u64)
}

pub fn human_size(bytes: u64) -> String {
    humansize::format_size(bytes, humansize::DECIMAL)
}

pub fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0:00".to_string();
    }
    let total = seconds.round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let secs = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

pub fn video_selector(height: u32) -> String {
    format!(
        "bv*[height<={height}][ext=mp4][vcodec^=avc1]+ba[ext=m4a]/\
         bv*[height<={height}][ext=mp4]+ba[ext=m4a]/\
         bv*[height<={height}]+ba/b[height<={height}]"
    )
}

pub fn audio_selector() -> &'static str {
    "ba[ext=m4a]/ba"
}

pub fn quality_keyboard(
    pending_id: &str,
    qualities: &[QualityOption],
    audio_estimate: Option<u64>,
    max_upload_bytes: u64,
) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    let mut row: Vec<InlineKeyboardButton> = Vec::new();

    for quality in qualities {
        let label = format!(
            "{}p · {}",
            quality.height,
            quality.size_label(max_upload_bytes)
        );
        row.push(InlineKeyboardButton::callback(
            label,
            format!("q:{pending_id}:{}", quality.height),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }

    let mut extras = Vec::new();
    let audio_label = match audio_estimate {
        Some(bytes) => format!("🎵 MP3 · {}", human_size(bytes)),
        None => "🎵 Audio MP3".to_string(),
    };
    extras.push(InlineKeyboardButton::callback(
        audio_label,
        format!("q:{pending_id}:audio"),
    ));

    let show_best = qualities
        .first()
        .and_then(|quality| quality.estimated_bytes)
        .map(|bytes| bytes <= max_upload_bytes)
        .unwrap_or(true);
    if show_best && !qualities.is_empty() {
        extras.push(InlineKeyboardButton::callback(
            "⭐ Best MP4",
            format!("q:{pending_id}:best"),
        ));
    }
    rows.push(extras);

    InlineKeyboardMarkup::new(rows)
}

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s<>]+").expect("valid URL regex"));
static ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{11}$").expect("valid ID regex"));

pub fn extract_youtube_url(text: &str) -> Option<String> {
    let raw = URL_RE
        .find(text)
        .map(|matched| matched.as_str())
        .unwrap_or_else(|| text.trim());
    let raw = raw.trim_end_matches(|c: char| {
        matches!(
            c,
            ')' | ']' | '}' | ',' | '.' | ';' | '!' | '?' | '"' | '\''
        )
    });

    let url = Url::parse(raw).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let host = host.strip_prefix("m.").unwrap_or(host);

    let id = match host {
        "youtu.be" => {
            let id = url.path().trim_matches('/');
            valid_video_id(id).then(|| id.to_string())
        }
        "youtube.com" | "music.youtube.com" => {
            let path = url.path();
            if path == "/watch" {
                url.query_pairs()
                    .find(|(key, _)| key == "v")
                    .map(|(_, value)| value.into_owned())
                    .filter(|id| valid_video_id(id))
            } else if let Some(id) = path
                .strip_prefix("/shorts/")
                .or_else(|| path.strip_prefix("/embed/"))
                .or_else(|| path.strip_prefix("/live/"))
                .or_else(|| path.strip_prefix("/v/"))
            {
                let id = id.split('/').next().unwrap_or("");
                valid_video_id(id).then(|| id.to_string())
            } else {
                None
            }
        }
        _ => None,
    }?;

    Some(format!("https://www.youtube.com/watch?v={id}"))
}

fn valid_video_id(id: &str) -> bool {
    ID_RE.is_match(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_common_youtube_urls() {
        assert_eq!(
            extract_youtube_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string())
        );
        assert_eq!(
            extract_youtube_url("https://youtu.be/dQw4w9WgXcQ?t=10"),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string())
        );
        assert_eq!(
            extract_youtube_url("https://www.youtube.com/shorts/dQw4w9WgXcQ"),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string())
        );
        assert_eq!(
            extract_youtube_url("look at this https://youtu.be/dQw4w9WgXcQ thanks"),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string())
        );
    }

    #[test]
    fn rejects_non_video_urls() {
        assert_eq!(
            extract_youtube_url("https://www.youtube.com/playlist?list=PL123"),
            None
        );
        assert_eq!(
            extract_youtube_url("https://example.com/watch?v=dQw4w9WgXcQ"),
            None
        );
        assert_eq!(extract_youtube_url("hello"), None);
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(0.0), "0:00");
        assert_eq!(format_duration(59.4), "0:59");
        assert_eq!(format_duration(61.0), "1:01");
        assert_eq!(format_duration(3661.0), "1:01:01");
    }
}
