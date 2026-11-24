use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{bail, Result};
use dashmap::mapref::entry::Entry;
use teloxide::{
    prelude::*,
    types::{
        ChatId, InlineKeyboardButton, InlineKeyboardMarkup, Me, Message, MessageId, ParseMode,
    },
    utils::command::BotCommands,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::{
    build_info,
    downloader::{DownloadError, DownloadResult},
    format::{
        build_quality_options, estimate_audio, extract_youtube_url, format_duration, human_size,
        quality_keyboard, QualityOption, YtdlpInfo,
    },
    progress::{render_progress, Progress},
    state::{Access, AppState, JobHandle, PendingVideo},
    uploader,
};

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
pub enum Command {
    /// Start the bot
    Start,
    /// Show help
    Help,
    /// Show your Telegram user ID
    Id,
    /// Cancel your active download
    Cancel,
    /// Show bot statistics (admin)
    Stats,
    /// Allow a user ID (admin)
    Allow(i64),
    /// Remove a user ID (admin)
    Deny(i64),
    /// List allowed users (admin)
    Users,
}

pub async fn handle_message(bot: Bot, msg: Message, me: Me, state: Arc<AppState>) -> Result<()> {
    let Some(text) = msg.text().map(ToOwned::to_owned) else {
        return Ok(());
    };
    let Some(from) = msg.from.as_ref() else {
        return Ok(());
    };
    let user_id = from.id.0 as i64;

    if !msg.chat.is_private() {
        bot.send_message(msg.chat.id, "Please use this bot in a private chat.")
            .await?;
        return Ok(());
    }

    if text.starts_with('/') {
        match Command::parse(&text, me.username()) {
            Ok(command) => return handle_command(bot, msg, command, user_id, &state).await,
            Err(error) => {
                let text = match error {
                    teloxide::utils::command::ParseError::IncorrectFormat(_)
                    | teloxide::utils::command::ParseError::TooFewArguments { .. }
                    | teloxide::utils::command::ParseError::TooManyArguments { .. } => {
                        "Invalid arguments. Usage: /allow &lt;id&gt; or /deny &lt;id&gt;"
                    }
                    _ => "Unknown command. Use /help.",
                };
                bot.send_message(msg.chat.id, text)
                    .parse_mode(ParseMode::Html)
                    .await?;
                return Ok(());
            }
        }
    }

    handle_url(bot, msg, user_id, &state, &text).await
}

async fn handle_command(
    bot: Bot,
    msg: Message,
    command: Command,
    user_id: i64,
    state: &Arc<AppState>,
) -> Result<()> {
    let chat_id = msg.chat.id;

    match command {
        Command::Start => {
            match state.access.claim_admin_if_empty(user_id) {
                Ok(true) => {
                    bot.send_message(
                        chat_id,
                        "👑 You are now the admin of this bot.\nUse /allow &lt;id&gt; to allow users and /users to list them.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await?;
                }
                Ok(false) => {}
                Err(error) => {
                    error!("cannot persist admin claim: {error}");
                    bot.send_message(
                        chat_id,
                        "❌ Could not save the admin allowlist. Check the bot logs and disk permissions.",
                    )
                    .await?;
                    return Ok(());
                }
            }

            match state.access.check(user_id) {
                Access::Admin => {
                    bot.send_message(chat_id, start_text(true))
                        .parse_mode(ParseMode::Html)
                        .await?;
                }
                Access::Allowed => {
                    bot.send_message(chat_id, start_text(false))
                        .parse_mode(ParseMode::Html)
                        .await?;
                }
                Access::Denied => {
                    bot.send_message(chat_id, denied_text(user_id))
                        .parse_mode(ParseMode::Html)
                        .await?;
                }
            }
            Ok(())
        }
        Command::Help => {
            if !ensure_access(&bot, chat_id, user_id, state).await? {
                return Ok(());
            }
            bot.send_message(chat_id, help_text(state.access.is_admin(user_id)))
                .parse_mode(ParseMode::Html)
                .await?;
            Ok(())
        }
        Command::Id => {
            bot.send_message(
                chat_id,
                format!("Your Telegram user ID: <code>{user_id}</code>"),
            )
            .parse_mode(ParseMode::Html)
            .await?;
            Ok(())
        }
        Command::Cancel => {
            if !ensure_access(&bot, chat_id, user_id, state).await? {
                return Ok(());
            }
            let job_id = state.user_jobs.get(&user_id).map(|entry| entry.clone());
            if let Some(job_id) = job_id {
                if let Some(handle) = state.jobs.get(&job_id) {
                    handle.cancel.cancel();
                }
                bot.send_message(chat_id, "🛑 Cancelling your active job...")
                    .await?;
            } else {
                bot.send_message(chat_id, "You have no active job.").await?;
            }
            Ok(())
        }
        Command::Stats => {
            if !require_admin(&bot, chat_id, user_id, state).await? {
                return Ok(());
            }
            let (admins, allowed) = state.access.list();
            let uptime = format_duration(state.started_at.elapsed().as_secs_f64());
            let disk = fs2::available_space(&state.config.data_dir)
                .map(human_size)
                .unwrap_or_else(|_| "?".to_string());
            let build = state
                .versions
                .as_ref()
                .map(|versions| versions.build_tag.clone())
                .unwrap_or_else(|| build_info::BUILD_TAG.to_string());
            let ytdlp = state
                .versions
                .as_ref()
                .map(|versions| versions.yt_dlp.clone())
                .unwrap_or_else(|| "?".to_string());
            let ffmpeg = state
                .versions
                .as_ref()
                .map(|versions| versions.ffmpeg.clone())
                .unwrap_or_else(|| "?".to_string());

            let text = format!(
                "<b>📊 Bot statistics</b>\n\
                 Uptime: {uptime}\n\
                 Active jobs: {}\n\
                 Pending menus: {}\n\
                 Admins: {}\n\
                 Allowed users: {}\n\
                 Concurrency: {}\n\
                 Upload limit: {} MB\n\
                 Build: <code>{build}</code>\n\
                 Commit: <code>{}</code>\n\
                 yt-dlp: <code>{ytdlp}</code>\n\
                 FFmpeg: <code>{ffmpeg}</code>\n\
                 Disk free: {disk}",
                state.active.get(),
                state.pending.len(),
                admins.len(),
                allowed.len(),
                state.config.max_concurrent_downloads,
                state.config.max_upload_mb,
                build_info::GIT_SHA,
            );
            bot.send_message(chat_id, text)
                .parse_mode(ParseMode::Html)
                .await?;
            Ok(())
        }
        Command::Allow(arg) => {
            if !require_admin(&bot, chat_id, user_id, state).await? {
                return Ok(());
            }
            let target = match parse_user_id(arg) {
                Ok(target) => target,
                Err(error) => {
                    bot.send_message(chat_id, error.to_string()).await?;
                    return Ok(());
                }
            };
            if state.access.is_admin(target) {
                bot.send_message(chat_id, format!("{target} is already an admin."))
                    .await?;
            } else {
                match state.access.allow(target) {
                    Ok(true) => {
                        bot.send_message(chat_id, format!("✅ Allowed user {target}."))
                            .await?;
                    }
                    Ok(false) => {
                        bot.send_message(chat_id, format!("User {target} is already allowed."))
                            .await?;
                    }
                    Err(error) => {
                        error!("cannot persist allow for {target}: {error}");
                        bot.send_message(
                            chat_id,
                            "❌ Could not save the allowlist. Check the bot logs.",
                        )
                        .await?;
                    }
                }
            }
            Ok(())
        }
        Command::Deny(arg) => {
            if !require_admin(&bot, chat_id, user_id, state).await? {
                return Ok(());
            }
            let target = match parse_user_id(arg) {
                Ok(target) => target,
                Err(error) => {
                    bot.send_message(chat_id, error.to_string()).await?;
                    return Ok(());
                }
            };
            if state.access.is_admin(target) {
                bot.send_message(chat_id, "Cannot remove an admin with /deny.")
                    .await?;
            } else {
                match state.access.deny(target) {
                    Ok(true) => {
                        bot.send_message(chat_id, format!("✅ Removed user {target}."))
                            .await?;
                    }
                    Ok(false) => {
                        bot.send_message(
                            chat_id,
                            format!("User {target} was not in the allowlist."),
                        )
                        .await?;
                    }
                    Err(error) => {
                        error!("cannot persist deny for {target}: {error}");
                        bot.send_message(
                            chat_id,
                            "❌ Could not save the allowlist. Check the bot logs.",
                        )
                        .await?;
                    }
                }
            }
            Ok(())
        }
        Command::Users => {
            if !require_admin(&bot, chat_id, user_id, state).await? {
                return Ok(());
            }
            let (admins, allowed) = state.access.list();
            let admins_text = if admins.is_empty() {
                "none".to_string()
            } else {
                admins
                    .iter()
                    .map(|id| format!("<code>{id}</code>"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let allowed_text = if allowed.is_empty() {
                "none".to_string()
            } else {
                allowed
                    .iter()
                    .map(|id| format!("<code>{id}</code>"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            bot.send_message(
                chat_id,
                format!(
                    "<b>👑 Admins</b>\n{admins_text}\n\n<b>✅ Allowed users</b>\n{allowed_text}"
                ),
            )
            .parse_mode(ParseMode::Html)
            .await?;
            Ok(())
        }
    }
}

fn maintenance_active(state: &AppState) -> bool {
    let path = state.config.data_dir.join("maintenance");
    let Ok(metadata) = std::fs::metadata(&path) else {
        return false;
    };
    let age = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .unwrap_or_default();
    age < Duration::from_secs(2 * 60 * 60)
}

async fn handle_url(
    bot: Bot,
    msg: Message,
    user_id: i64,
    state: &Arc<AppState>,
    text: &str,
) -> Result<()> {
    let chat_id = msg.chat.id;
    if !ensure_access(&bot, chat_id, user_id, state).await? {
        return Ok(());
    }

    if maintenance_active(state) {
        bot.send_message(
            chat_id,
            "🛠 The bot is updating right now. Please send the link again in a minute.",
        )
        .await?;
        return Ok(());
    }

    let Some(url) = extract_youtube_url(text) else {
        bot.send_message(
            chat_id,
            "Send me a YouTube video link (watch, youtu.be, or Shorts).",
        )
        .await?;
        return Ok(());
    };

    if state.user_jobs.contains_key(&user_id) {
        bot.send_message(
            chat_id,
            "You already have an active job. Use /cancel or wait for it to finish.",
        )
        .await?;
        return Ok(());
    }

    let status = bot
        .send_message(chat_id, "🔎 Fetching video info...")
        .await?;

    let result = tokio::time::timeout(
        Duration::from_secs(state.config.info_timeout_secs),
        state.downloader.fetch_info(&url),
    )
    .await;

    let info = match result {
        Err(_) => {
            edit_status(
                &bot,
                chat_id,
                status.id,
                "❌ Timed out while fetching video info. Try again.",
            )
            .await;
            return Ok(());
        }
        Ok(Err(error)) => {
            edit_status(&bot, chat_id, status.id, &friendly_error(&error)).await;
            return Ok(());
        }
        Ok(Ok(info)) => info,
    };

    let qualities = build_quality_options(
        &info,
        state.config.max_video_height,
        state.config.max_upload_bytes(),
    );
    let audio_estimate = estimate_audio(&info);
    if qualities.is_empty() && audio_estimate.is_none() {
        edit_status(
            &bot,
            chat_id,
            status.id,
            "❌ No downloadable formats were found for this link.",
        )
        .await;
        return Ok(());
    }

    let pending_id = short_id();
    let info_message = info_text(&info);
    let keyboard = quality_keyboard(
        &pending_id,
        &qualities,
        audio_estimate,
        state.config.max_upload_bytes(),
    );

    state.pending.insert(
        pending_id,
        PendingVideo {
            user_id,
            url,
            info,
            qualities,
            audio_estimate,
            created_at: Instant::now(),
        },
    );

    match bot
        .edit_message_text(chat_id, status.id, &info_message)
        .parse_mode(ParseMode::Html)
        .reply_markup(keyboard.clone())
        .await
    {
        Ok(_) => {}
        Err(error) => {
            debug!("could not edit info message: {error}");
            bot.send_message(chat_id, info_message)
                .parse_mode(ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
        }
    }

    Ok(())
}

pub async fn handle_callback(bot: Bot, query: CallbackQuery, state: Arc<AppState>) -> Result<()> {
    let user_id = query.from.id.0 as i64;
    let chat_id = query
        .message
        .as_ref()
        .map(|message| message.chat().id)
        .unwrap_or(ChatId(user_id));
    let status_id = query
        .message
        .as_ref()
        .and_then(|message| message.regular_message())
        .map(|message| message.id);

    let _ = bot.answer_callback_query(query.id.clone()).await;

    let Some(data) = query.data.as_deref() else {
        return Ok(());
    };

    if let Some(rest) = data.strip_prefix("q:") {
        let mut parts = rest.splitn(2, ':');
        let pending_id = parts.next().unwrap_or_default();
        let code = parts.next().unwrap_or_default();
        if pending_id.is_empty() || code.is_empty() {
            return Ok(());
        }
        return handle_quality(bot, chat_id, status_id, user_id, pending_id, code, &state).await;
    }

    if let Some(job_id) = data.strip_prefix("c:") {
        if state.access.check(user_id) == Access::Denied {
            bot.send_message(chat_id, denied_text(user_id))
                .parse_mode(ParseMode::Html)
                .await?;
            return Ok(());
        }
        if let Some(handle) = state.jobs.get(job_id) {
            if handle.user_id == user_id || state.access.is_admin(user_id) {
                handle.cancel.cancel();
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_quality(
    bot: Bot,
    chat_id: ChatId,
    status_id: Option<MessageId>,
    user_id: i64,
    pending_id: &str,
    code: &str,
    state: &Arc<AppState>,
) -> Result<()> {
    if !ensure_access(&bot, chat_id, user_id, state).await? {
        return Ok(());
    }

    if maintenance_active(state) {
        let text = "🛠 The bot is updating right now. Please send the link again in a minute.";
        match status_id {
            Some(message_id) => edit_status(&bot, chat_id, message_id, text).await,
            None => {
                bot.send_message(chat_id, text).await?;
            }
        }
        return Ok(());
    }

    if state.user_jobs.contains_key(&user_id) {
        let text = "You already have an active job. Use /cancel or wait for it to finish.";
        match status_id {
            Some(message_id) => edit_status(&bot, chat_id, message_id, text).await,
            None => {
                bot.send_message(chat_id, text).await?;
            }
        }
        return Ok(());
    }

    let Some(pending) = state.pending.remove(pending_id).map(|(_, value)| value) else {
        let text = "This menu expired. Send the YouTube link again.";
        match status_id {
            Some(message_id) => edit_status(&bot, chat_id, message_id, text).await,
            None => {
                bot.send_message(chat_id, text).await?;
            }
        }
        return Ok(());
    };

    if pending.user_id != user_id {
        state.pending.insert(pending_id.to_string(), pending);
        let text = "This menu belongs to another user.";
        match status_id {
            Some(message_id) => edit_status(&bot, chat_id, message_id, text).await,
            None => {
                bot.send_message(chat_id, text).await?;
            }
        }
        return Ok(());
    }

    let (height, audio, quality) = if code == "audio" {
        (state.config.max_video_height, true, None)
    } else if code == "best" {
        let Some(quality) = pending.qualities.first().cloned() else {
            let text = "No video quality is available. Send the link again.";
            if let Some(message_id) = status_id {
                edit_status(&bot, chat_id, message_id, text).await;
            } else {
                bot.send_message(chat_id, text).await?;
            }
            return Ok(());
        };
        (quality.height, false, Some(quality))
    } else {
        let parsed_height = code.parse::<u32>().ok();
        let quality = parsed_height.and_then(|height| {
            pending
                .qualities
                .iter()
                .find(|quality| quality.height == height)
                .cloned()
        });
        let Some(quality) = quality else {
            let text = "That quality is no longer available. Send the link again.";
            if let Some(message_id) = status_id {
                edit_status(&bot, chat_id, message_id, text).await;
            } else {
                bot.send_message(chat_id, text).await?;
            }
            return Ok(());
        };
        (quality.height, false, Some(quality))
    };

    let status_id = match status_id {
        Some(message_id) => message_id,
        None => bot.send_message(chat_id, "Starting...").await?.id,
    };

    tokio::spawn(run_job(
        bot,
        chat_id,
        status_id,
        user_id,
        pending,
        height,
        audio,
        quality,
        state.clone(),
    ));

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_job(
    bot: Bot,
    chat_id: ChatId,
    status_id: MessageId,
    user_id: i64,
    pending: PendingVideo,
    height: u32,
    audio: bool,
    quality: Option<QualityOption>,
    state: Arc<AppState>,
) {
    let job_id = short_id();

    match state.user_jobs.entry(user_id) {
        Entry::Occupied(_) => {
            edit_status(&bot, chat_id, status_id, "You already have an active job.").await;
            return;
        }
        Entry::Vacant(entry) => {
            entry.insert(job_id.clone());
        }
    }

    let cancel = CancellationToken::new();
    state.jobs.insert(
        job_id.clone(),
        JobHandle {
            user_id,
            cancel: cancel.clone(),
        },
    );
    state.active.inc();
    let _guard = JobGuard {
        state: state.clone(),
        job_id: job_id.clone(),
        user_id,
    };

    let label = if audio {
        "Audio (MP3)".to_string()
    } else {
        format!("{height}p")
    };

    edit_status_kb(
        &bot,
        chat_id,
        status_id,
        "⏳ Queued...",
        InlineKeyboardMarkup::default(),
    )
    .await;

    let permit = tokio::select! {
        permit = state.semaphore.clone().acquire_owned() => permit,
        _ = cancel.cancelled() => {
            edit_status(&bot, chat_id, status_id, "❌ Cancelled.").await;
            return;
        }
    };
    let _permit = match permit {
        Ok(permit) => permit,
        Err(_) => {
            edit_status(&bot, chat_id, status_id, "❌ The job queue is closed.").await;
            return;
        }
    };

    if cancel.is_cancelled() {
        edit_status(&bot, chat_id, status_id, "❌ Cancelled.").await;
        return;
    }

    let job_dir = state.config.data_dir.join("tmp").join(&job_id);
    if let Err(error) = tokio::fs::create_dir_all(&job_dir).await {
        edit_status(
            &bot,
            chat_id,
            status_id,
            &format!("❌ Could not create a temp directory: {error}"),
        )
        .await;
        return;
    }

    let free = fs2::available_space(&state.config.data_dir).unwrap_or(u64::MAX);
    if free < 350 * 1024 * 1024 {
        edit_status(
            &bot,
            chat_id,
            status_id,
            "❌ Not enough free disk space (need about 350 MB).",
        )
        .await;
        let _ = tokio::fs::remove_dir_all(&job_dir).await;
        return;
    }

    edit_status_kb(
        &bot,
        chat_id,
        status_id,
        &format!("⬇️ Downloading {label}..."),
        cancel_keyboard(&job_id),
    )
    .await;

    let (progress_tx, mut progress_rx) = mpsc::channel::<Progress>(64);
    let downloader = state.downloader.clone();
    let url = pending.url.clone();
    let title = pending.info.title.clone();
    let dir = job_dir.clone();
    let cancel_download = cancel.clone();
    let mut download_handle = tokio::spawn(async move {
        downloader
            .download(
                &url,
                &title,
                height,
                audio,
                &dir,
                progress_tx,
                cancel_download,
            )
            .await
    });

    let mut last_edit = Instant::now() - Duration::from_secs(10);
    let result: Result<DownloadResult, DownloadError> = loop {
        tokio::select! {
            result = &mut download_handle => {
                break match result {
                    Ok(result) => result,
                    Err(error) => Err(DownloadError::Other(format!(
                        "download task failed: {error}"
                    ))),
                };
            }
            Some(progress) = progress_rx.recv() => {
                if last_edit.elapsed() >= Duration::from_secs(3) {
                    let text = render_progress(progress, &label);
                    edit_status_kb(
                        &bot,
                        chat_id,
                        status_id,
                        &text,
                        cancel_keyboard(&job_id),
                    )
                    .await;
                    last_edit = Instant::now();
                }
            }
        }
    };

    match result {
        Ok(download) => {
            if let Ok(probe) = state.downloader.probe(&download.path).await {
                debug!(
                    "ffprobe {}: duration={:?} size={} bit_rate={:?}",
                    download.path.display(),
                    probe.duration_secs,
                    probe.size,
                    probe.bit_rate
                );
            }

            edit_status(&bot, chat_id, status_id, "⬆️ Uploading to Telegram...").await;

            let upload = if audio {
                uploader::send_audio(&bot, chat_id, &download.path, &pending.info, download.size)
                    .await
            } else {
                uploader::send_video(
                    &bot,
                    chat_id,
                    &download.path,
                    &pending.info,
                    quality.as_ref(),
                    download.size,
                )
                .await
            };

            match upload {
                Ok(_) => {
                    let _ = bot.delete_message(chat_id, status_id).await;
                    info!("job {} finished for user {}", job_id, user_id);
                }
                Err(error) => {
                    error!("upload failed for job {}: {error}", job_id);
                    edit_status(
                        &bot,
                        chat_id,
                        status_id,
                        &format!("❌ Upload failed: {error}"),
                    )
                    .await;
                }
            }
        }
        Err(DownloadError::Cancelled) => {
            edit_status(&bot, chat_id, status_id, "❌ Cancelled.").await;
        }
        Err(DownloadError::Timeout) => {
            edit_status(
                &bot,
                chat_id,
                status_id,
                "❌ The download timed out. Try a lower quality.",
            )
            .await;
        }
        Err(DownloadError::TooLarge { size, .. }) => {
            let max_upload_bytes = state.config.max_upload_bytes();
            let lower: Vec<QualityOption> = pending
                .qualities
                .iter()
                .filter(|quality| {
                    quality
                        .estimated_bytes
                        .map(|bytes| bytes <= max_upload_bytes)
                        .unwrap_or(true)
                })
                .cloned()
                .collect();
            let new_pending_id = short_id();
            state.pending.insert(
                new_pending_id.clone(),
                PendingVideo {
                    user_id,
                    url: pending.url.clone(),
                    info: pending.info.clone(),
                    qualities: pending.qualities.clone(),
                    audio_estimate: pending.audio_estimate,
                    created_at: Instant::now(),
                },
            );
            let text = format!(
                "⚠️ The file is {} (Telegram limit: {} MB).\nChoose a lower quality:",
                human_size(size),
                state.config.max_upload_mb
            );
            let keyboard = quality_keyboard(
                &new_pending_id,
                &lower,
                pending.audio_estimate,
                max_upload_bytes,
            );
            edit_status_kb(&bot, chat_id, status_id, &text, keyboard).await;
        }
        Err(error) => {
            edit_status(&bot, chat_id, status_id, &friendly_error(&error)).await;
        }
    }

    let _ = tokio::fs::remove_dir_all(&job_dir).await;
}

struct JobGuard {
    state: Arc<AppState>,
    job_id: String,
    user_id: i64,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.state.jobs.remove(&self.job_id);
        let should_remove = self
            .state
            .user_jobs
            .get(&self.user_id)
            .map(|entry| *entry == self.job_id)
            .unwrap_or(false);
        if should_remove {
            self.state.user_jobs.remove(&self.user_id);
        }
        self.state.active.dec();
    }
}

async fn ensure_access(bot: &Bot, chat_id: ChatId, user_id: i64, state: &AppState) -> Result<bool> {
    match state.access.check(user_id) {
        Access::Admin | Access::Allowed => Ok(true),
        Access::Denied => {
            bot.send_message(chat_id, denied_text(user_id))
                .parse_mode(ParseMode::Html)
                .await?;
            Ok(false)
        }
    }
}

async fn require_admin(bot: &Bot, chat_id: ChatId, user_id: i64, state: &AppState) -> Result<bool> {
    if state.access.is_admin(user_id) {
        Ok(true)
    } else {
        bot.send_message(chat_id, "⛔ This command is for admins only.")
            .await?;
        Ok(false)
    }
}

fn parse_user_id(id: i64) -> Result<i64> {
    if id <= 0 {
        bail!("Telegram user IDs are positive numbers. Usage: /allow <id> or /deny <id>");
    }
    Ok(id)
}

fn start_text(admin: bool) -> String {
    let mut text = String::from(
        "👋 <b>Welcome to ytdl-bot!</b>\n\n\
         Send me a YouTube video link and I will download it and send it back.\n\n\
         <b>How to use</b>\n\
         1. Send a YouTube link (watch, youtu.be, or Shorts).\n\
         2. Pick a quality.\n\
         3. Wait for the upload.\n\n\
         Telegram bots can upload files up to <b>50 MB</b>. The quality buttons show estimated sizes and mark likely-too-large options with ⚠️.\n\n\
         /help — commands\n\
         /id — your Telegram user ID\n\
         /cancel — cancel your active download",
    );
    if admin {
        text.push_str(
            "\n\n👑 <b>Admin commands</b>\n\
             /allow &lt;id&gt; — allow a user\n\
             /deny &lt;id&gt; — remove a user\n\
             /users — list users\n\
             /stats — bot statistics",
        );
    }
    text
}

fn help_text(admin: bool) -> String {
    let mut text = String::from(
        "<b>📖 Commands</b>\n\
         /start — welcome message\n\
         /help — this help\n\
         /id — show your Telegram user ID\n\
         /cancel — cancel your active download\n\n\
         <b>How to use</b>\n\
         Send a YouTube link. The bot shows available qualities with estimated sizes. Pick one and wait for the video.\n\n\
         <b>Limits</b>\n\
         • Telegram bot uploads: 50 MB\n\
         • Playlists and live streams are not supported\n\
         • If YouTube asks for sign-in, the admin must add a cookies.txt file",
    );
    if admin {
        text.push_str(
            "\n\n<b>Admin commands</b>\n\
             /allow &lt;id&gt;\n\
             /deny &lt;id&gt;\n\
             /users\n\
             /stats",
        );
    }
    text
}

fn denied_text(user_id: i64) -> String {
    format!(
        "🔒 <b>You are not authorized to use this bot.</b>\n\n\
         Your Telegram user ID: <code>{user_id}</code>\n\n\
         Ask the admin to run:\n<code>/allow {user_id}</code>"
    )
}

fn info_text(info: &YtdlpInfo) -> String {
    let title = teloxide::utils::html::escape(&truncate_chars(&info.title, 300));
    let uploader = info
        .uploader
        .as_deref()
        .or(info.channel.as_deref())
        .unwrap_or("Unknown");
    let uploader = teloxide::utils::html::escape(&truncate_chars(uploader, 100));
    let duration = info
        .duration
        .map(format_duration)
        .unwrap_or_else(|| "?".to_string());
    format!("🎬 <b>{title}</b>\n👤 {uploader}\n⏱ {duration}\n\nChoose a quality:")
}

fn friendly_error(error: &DownloadError) -> String {
    match error {
        DownloadError::Cancelled => "❌ Cancelled.".to_string(),
        DownloadError::Timeout => "❌ The operation timed out. Try again.".to_string(),
        DownloadError::TooLarge { size, .. } => format!(
            "❌ The file is too large ({}). Telegram allows up to 50 MB.",
            human_size(*size)
        ),
        DownloadError::TooLong { secs } => format!(
            "❌ The video is too long ({}). Try a shorter video.",
            format_duration(*secs as f64)
        ),
        DownloadError::BotCheck => {
            "❌ YouTube requires sign-in verification.\nThe admin needs to add a cookies.txt file."
                .to_string()
        }
        DownloadError::Private => "❌ This video is private.".to_string(),
        DownloadError::Unavailable => "❌ This video is unavailable.".to_string(),
        DownloadError::AgeRestricted => {
            "❌ This video is age-restricted and needs cookies.".to_string()
        }
        DownloadError::Live => "❌ Live streams are not supported.".to_string(),
        DownloadError::FormatUnavailable => {
            "❌ The selected quality is not available. Try another.".to_string()
        }
        DownloadError::Failed(message) => {
            format!("❌ Download failed.\n{}", truncate_chars(message, 400))
        }
        DownloadError::Io(error) => format!("❌ I/O error: {error}"),
        DownloadError::Other(message) => format!("❌ {message}"),
    }
}

async fn edit_status(bot: &Bot, chat_id: ChatId, message_id: MessageId, text: &str) {
    if let Err(error) = bot.edit_message_text(chat_id, message_id, text).await {
        debug!("edit_message_text failed: {error}");
    }
}

async fn edit_status_kb(
    bot: &Bot,
    chat_id: ChatId,
    message_id: MessageId,
    text: &str,
    keyboard: InlineKeyboardMarkup,
) {
    if let Err(error) = bot
        .edit_message_text(chat_id, message_id, text)
        .reply_markup(keyboard)
        .await
    {
        debug!("edit_message_text failed: {error}");
    }
}

fn cancel_keyboard(job_id: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "❌ Cancel",
        format!("c:{job_id}"),
    )]])
}

fn short_id() -> String {
    let id = Uuid::new_v4().simple().to_string();
    id[..8].to_string()
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
