use std::path::Path;

use anyhow::Result;
use teloxide::{
    prelude::*,
    types::{ChatId, InputFile, Message, ParseMode},
};
use tracing::debug;

use crate::format::{format_duration, human_size, QualityOption, YtdlpInfo};

pub async fn send_video(
    bot: &Bot,
    chat_id: ChatId,
    path: &Path,
    info: &YtdlpInfo,
    quality: Option<&QualityOption>,
    size: u64,
) -> Result<Message> {
    let title = truncate_chars(&info.title, 300);
    let title_html = teloxide::utils::html::escape(&title);
    let height = quality.map(|quality| quality.height).unwrap_or(0);
    let width = quality.and_then(|quality| quality.width).unwrap_or(0);
    let quality_label = quality
        .map(|quality| format!("{}p", quality.height))
        .unwrap_or_else(|| "video".to_string());
    let duration = info.duration.unwrap_or(0.0).round() as u32;

    let caption = format!(
        "<b>{title_html}</b>\n{quality_label} • {} • {}",
        human_size(size),
        format_duration(info.duration.unwrap_or(0.0))
    );

    let mut request = bot
        .send_video(chat_id, InputFile::file(path.to_path_buf()))
        .caption(caption)
        .parse_mode(ParseMode::Html)
        .supports_streaming(true);

    if duration > 0 {
        request = request.duration(duration);
    }
    if width > 0 {
        request = request.width(width);
    }
    if height > 0 {
        request = request.height(height);
    }

    debug!("uploading video {} ({} bytes)", path.display(), size);
    Ok(request.await?)
}

pub async fn send_audio(
    bot: &Bot,
    chat_id: ChatId,
    path: &Path,
    info: &YtdlpInfo,
    size: u64,
) -> Result<Message> {
    let title = truncate_chars(&info.title, 64);
    let performer = info
        .uploader
        .as_deref()
        .or(info.channel.as_deref())
        .unwrap_or("YouTube");
    let performer = truncate_chars(performer, 64);
    let caption_title = teloxide::utils::html::escape(&truncate_chars(&info.title, 300));
    let duration = info.duration.unwrap_or(0.0).round() as u32;

    let caption = format!(
        "<b>{caption_title}</b>\nAudio • {} • {}",
        human_size(size),
        format_duration(info.duration.unwrap_or(0.0))
    );

    let mut request = bot
        .send_audio(chat_id, InputFile::file(path.to_path_buf()))
        .caption(caption)
        .parse_mode(ParseMode::Html)
        .title(title)
        .performer(performer);

    if duration > 0 {
        request = request.duration(duration);
    }

    debug!("uploading audio {} ({} bytes)", path.display(), size);
    Ok(request.await?)
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
