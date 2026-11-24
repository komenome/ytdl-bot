mod build_info;
mod config;
mod downloader;
mod format;
mod handlers;
mod progress;
mod state;
mod uploader;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use teloxide::{dispatching::Dispatcher, dptree, net, prelude::*, types::BotCommand};
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

use crate::{
    build_info::{BUILD_TAG, GIT_SHA, VERSION},
    config::{Config, ToolPaths, DEFAULT_CONFIG_PATH},
    state::AppState,
};

const CLIENT_TIMEOUT: Duration = Duration::from_secs(900);
const PENDING_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Default)]
struct Cli {
    config: Option<PathBuf>,
    check: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = parse_args()?;
    init_tracing();

    if cli.check {
        return check_mode(cli.config.as_deref()).await;
    }

    let config_path = resolve_config_path(cli.config.as_deref())?;
    let config = Config::load(&config_path)?;
    let tools = ToolPaths::discover(&config)?;

    info!(
        "ytdl-bot {} (build {}, commit {})",
        VERSION, BUILD_TAG, GIT_SHA
    );
    info!("loaded config from {}", config_path.display());
    info!("yt-dlp: {}", tools.ytdlp.display());
    info!("ffmpeg: {}", tools.ffmpeg.display());

    let state = Arc::new(AppState::new(config.clone(), tools.clone()).await?);

    let client = net::default_reqwest_settings()
        .timeout(CLIENT_TIMEOUT)
        .connect_timeout(Duration::from_secs(30))
        .build()
        .context("could not build the HTTP client")?;
    let bot = Bot::with_client(config.bot_token.clone(), client);

    let me = bot
        .get_me()
        .await
        .context("could not connect to Telegram; check bot_token and network access")?;
    info!("connected to Telegram as @{}", me.username());

    if let Err(error) = register_commands(&bot).await {
        warn!("could not register bot commands: {error}");
    }

    spawn_pending_janitor(state.clone());

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(handlers::handle_message))
        .branch(Update::filter_callback_query().endpoint(handlers::handle_callback));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state, me])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    info!("dispatcher stopped");
    Ok(())
}

fn parse_args() -> Result<Cli> {
    let mut cli = Cli::default();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-c" | "--config" => {
                let path = args.next().context("--config requires a path argument")?;
                cli.config = Some(PathBuf::from(path));
            }
            "--check" => cli.check = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("ytdl-bot {VERSION} (build {BUILD_TAG}, commit {GIT_SHA})");
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}\nRun `ytdl-bot --help` for usage."),
        }
    }

    Ok(cli)
}

fn print_help() {
    println!(
        "ytdl-bot {VERSION}\n\
         \n\
         Usage: ytdl-bot [OPTIONS]\n\
         \n\
         Options:\n\
         \x20 -c, --config <PATH>   Config file (default: {DEFAULT_CONFIG_PATH} or ./config.toml)\n\
         \x20     --check           Check config, bundled tools, and disk access, then exit\n\
         \x20 -V, --version         Print version information\n\
         \x20 -h, --help            Print this help\n\
         \n\
         Environment:\n\
         \x20 YTDL_BOT_TOKEN        Overrides bot_token from the config file\n\
         \x20 RUST_LOG              Log filter (default: info)\n"
    );
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,teloxide=info,reqwest=warn,hyper=warn,h2=warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.is_file() {
            bail!("config file not found: {}", path.display());
        }
        return Ok(path.to_path_buf());
    }

    if let Ok(path) = std::env::var("YTDL_BOT_CONFIG") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    let system = PathBuf::from(DEFAULT_CONFIG_PATH);
    if system.is_file() {
        return Ok(system);
    }

    let local = PathBuf::from("config.toml");
    if local.is_file() {
        return Ok(local);
    }

    bail!(
        "no config file found. Checked {DEFAULT_CONFIG_PATH}, ./config.toml, and YTDL_BOT_CONFIG.\n\
         Pass --config <PATH> or create /etc/ytdl-bot/config.toml."
    )
}

async fn check_mode(explicit: Option<&Path>) -> Result<()> {
    println!("ytdl-bot {VERSION} (build {BUILD_TAG}, commit {GIT_SHA})");

    let config_path = match explicit {
        Some(path) => {
            if !path.is_file() {
                bail!("config file not found: {}", path.display());
            }
            Some(path.to_path_buf())
        }
        None => {
            let system = PathBuf::from(DEFAULT_CONFIG_PATH);
            if system.is_file() {
                Some(system)
            } else if Path::new("config.toml").is_file() {
                Some(PathBuf::from("config.toml"))
            } else {
                None
            }
        }
    };

    let config = match &config_path {
        Some(path) => {
            println!("Config:   {}", path.display());
            Config::load(path)?
        }
        None => {
            println!("Config:   not found (checked {DEFAULT_CONFIG_PATH} and ./config.toml)");
            println!("          checking bundled tools with defaults");
            default_check_config()
        }
    };

    let tools = ToolPaths::discover(&config)?;
    println!("Data dir: {}", config.data_dir.display());
    println!(
        "Limits:   {} MB upload, {}p max, {}s max duration",
        config.max_upload_mb, config.max_video_height, config.max_duration_secs
    );
    println!();

    let downloader = downloader::Downloader::new(config.clone(), tools.clone());
    let versions = downloader
        .tool_versions(&tools)
        .await
        .map_err(|error| anyhow::anyhow!("tool check failed: {error}"))?;

    println!("yt-dlp:   {} ({})", versions.ytdlp, tools.ytdlp.display());
    println!("ffmpeg:   {} ({})", versions.ffmpeg, tools.ffmpeg.display());
    println!(
        "ffprobe:  {} ({})",
        versions.ffprobe,
        tools.ffprobe.display()
    );

    let mut warnings = Vec::new();

    if let Some(cookies) = config
        .cookies_file
        .as_ref()
        .filter(|path| !path.as_os_str().is_empty())
    {
        if cookies.is_file() {
            println!("cookies:  {}", cookies.display());
        } else {
            warnings.push(format!(
                "cookies_file is set but not readable: {}",
                cookies.display()
            ));
        }
    }

    match ensure_data_dir(&config.data_dir).await {
        Ok(()) => println!("Disk:     writable ({})", config.data_dir.display()),
        Err(error) => warnings.push(format!("data_dir is not writable: {error}")),
    }

    if let Some(space) = fs2::available_space(&config.data_dir)
        .ok()
        .or_else(|| fs2::available_space(".").ok())
    {
        println!("Free:     {}", format::human_size(space));
    }

    if let Err(error) = check_ffmpeg_encoders(&tools.ffmpeg).await {
        warnings.push(error.to_string());
    }

    if warnings.is_empty() {
        println!("\n✅ All checks passed.");
        Ok(())
    } else {
        println!();
        for warning in &warnings {
            println!("⚠️  {warning}");
        }
        bail!("{} check(s) failed", warnings.len())
    }
}

fn default_check_config() -> Config {
    Config {
        bot_token: "0:check".to_string(),
        admin_ids: Vec::new(),
        allowed_users: Vec::new(),
        max_concurrent_downloads: 2,
        max_upload_mb: 50,
        max_video_height: 1080,
        max_duration_secs: 7200,
        download_timeout_secs: 1800,
        info_timeout_secs: 60,
        data_dir: std::env::temp_dir().join("ytdl-bot-check"),
        cookies_file: None,
        ytdlp_path: None,
        ffmpeg_path: None,
        ffprobe_path: None,
    }
}

async fn ensure_data_dir(path: &Path) -> Result<()> {
    tokio::fs::create_dir_all(path)
        .await
        .with_context(|| format!("cannot create {}", path.display()))?;
    let probe = path.join(".ytdl-bot-write-test");
    tokio::fs::write(&probe, b"ok")
        .await
        .with_context(|| format!("cannot write to {}", path.display()))?;
    let _ = tokio::fs::remove_file(&probe).await;
    Ok(())
}

async fn check_ffmpeg_encoders(ffmpeg: &Path) -> Result<()> {
    let output = tokio::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-encoders"])
        .output()
        .await
        .context("could not run ffmpeg -encoders")?;
    if !output.status.success() {
        bail!("ffmpeg -encoders failed");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("libmp3lame") {
        bail!("this ffmpeg build has no MP3 encoder (libmp3lame); audio downloads will fail");
    }
    Ok(())
}

async fn register_commands(bot: &Bot) -> Result<()> {
    let commands = vec![
        BotCommand::new("start", "Start the bot"),
        BotCommand::new("help", "Show help"),
        BotCommand::new("id", "Show your Telegram user ID"),
        BotCommand::new("cancel", "Cancel your active download"),
        BotCommand::new("stats", "Bot statistics (admin)"),
        BotCommand::new("allow", "Allow a user by ID (admin)"),
        BotCommand::new("deny", "Remove a user by ID (admin)"),
        BotCommand::new("users", "List allowed users (admin)"),
    ];
    bot.set_my_commands(commands)
        .await
        .context("set_my_commands failed")?;
    Ok(())
}

fn spawn_pending_janitor(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let mut expired = Vec::new();
            for entry in state.pending.iter() {
                if entry.value().created_at.elapsed() > PENDING_TTL {
                    expired.push(entry.key().clone());
                }
            }
            for key in &expired {
                state.pending.remove(key);
            }
            if !expired.is_empty() {
                debug!("removed {} expired quality menu(s)", expired.len());
            }
        }
    });
}
