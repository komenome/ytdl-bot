# ytdl-bot

A small Telegram bot that downloads YouTube videos and sends them back to you.
It runs as a single static Rust binary and bundles `yt-dlp`, `ffmpeg`, and
`ffprobe`, so a fresh VPS needs no media tooling installed.

## Features

- YouTube links: `watch`, `youtu.be`, `shorts`, `live`, `embed`, and `music.youtube.com`
- Quality picker with estimated file sizes (1080p / 720p / 480p / 360p)
- MP3 audio extraction and a "Best MP4" option
- Clear warning when a file will exceed Telegram's 50 MB bot upload limit
- Access control: the first `/start` user becomes admin; everyone else needs `/allow`
- Progress messages with cancel button
- Daily CI check for new `yt-dlp` / FFmpeg builds, with automatic GitHub releases
- Automatic server updates via systemd timer, with checksum verification and rollback
- Optional `cookies.txt` for age-restricted or bot-checked videos

## Requirements

- x86_64 Linux VPS with systemd
- `python3` (yt-dlp is a Python zipapp); the installer installs it if missing
- Outbound HTTPS access to Telegram, YouTube, and GitHub
- A Telegram bot token from [@BotFather](https://t.me/BotFather)

There is no Windows/macOS/ARM support. The release binary is built for
`x86_64-unknown-linux-musl` and is statically linked.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/komenome/ytdl-bot/main/install.sh | sudo bash
```

The installer will:

1. Install missing dependencies (`python3`, `curl`, `xz`, ...).
2. Download the latest release and verify its SHA256 checksum.
3. Ask for your bot token (input is hidden). For unattended installs, set
   `YTDL_BOT_TOKEN` instead.
4. Create the `ytdl-bot` system user and install to `/opt/ytdl-bot`.
5. Write `/etc/ytdl-bot/config.toml` and enable:
   - `ytdl-bot.service`
   - `ytdl-bot-update.timer`

After installation, open Telegram, send `/start` to your bot, and the first
user becomes the admin. Allow other people with:

```text
/allow 123456789
```

They can discover their ID with `/id` or from the "not authorized" message.

### Non-interactive install

```sh
curl -fsSL https://raw.githubusercontent.com/komenome/ytdl-bot/main/install.sh \
  | sudo YTDL_BOT_TOKEN='123456789:AA...' bash
```

Note: with `curl | sudo bash`, the installer reads prompts from `/dev/tty`, not
from the piped script.

## Commands

| Command | Description |
| --- | --- |
| `/start` | Welcome and basic instructions |
| `/help` | Command reference |
| `/id` | Show your Telegram user ID |
| `/cancel` | Cancel your active download |
| `/stats` | Uptime, jobs, versions, disk space (admin) |
| `/allow <id>` | Allow a user (admin) |
| `/deny <id>` | Remove a user (admin) |
| `/users` | List admins and allowed users (admin) |

Send any supported YouTube URL as a normal message. The bot replies with a
quality keyboard. Buttons marked ⚠️ are estimated to exceed 50 MB.

## The 50 MB limit

Telegram bots can upload at most **50 MB** per file. This is a Telegram Bot API
limit, not a bot setting. The quality menu shows estimated sizes and marks
likely-too-large options.

The bot does **not** re-encode video to fit. If the actual file still exceeds
the limit, it offers a lower quality again instead of uploading a broken file.
For longer videos, MP3 audio or 360p/480p is usually the practical choice.

Running a local [Telegram Bot API server](https://github.com/tdlib/telegram-bot-api)
removes this limit, but this project does not configure one.

## Configuration

The installer writes `/etc/ytdl-bot/config.toml`. A commented example is in
[`config.example.toml`](config.example.toml).

| Option | Default | Description |
| --- | --- | --- |
| `bot_token` | required | Telegram bot token |
| `admin_ids` | `[]` | Seed admins; empty lets the first `/start` user become admin |
| `allowed_users` | `[]` | Seed allowlist |
| `max_concurrent_downloads` | `2` | Simultaneous downloads (1–8) |
| `max_upload_mb` | `50` | Upload limit; capped at 50 |
| `max_video_height` | `1080` | Highest offered quality |
| `max_duration_secs` | `7200` | Reject longer videos |
| `download_timeout_secs` | `1800` | Per-download timeout |
| `info_timeout_secs` | `60` | Metadata timeout |
| `data_dir` | `/var/lib/ytdl-bot` | Temp downloads, access state, active job counter |
| `cookies_file` | empty | Optional Netscape-format cookies file |
| `ytdlp_path`, `ffmpeg_path`, `ffprobe_path` | auto | Override bundled tool paths |

`YTDL_BOT_TOKEN` overrides `bot_token` from the environment (useful for local
tests). `RUST_LOG` controls logging, e.g. `RUST_LOG=debug`.

Access state is stored in `/var/lib/ytdl-bot/state.json` and is preserved across
updates. Edit the allowlist with Telegram commands rather than editing the file.

## Cookies (age-restricted / bot-checked videos)

If YouTube shows "Sign in to confirm you're not a bot" or an age gate:

1. Export cookies in Netscape format from a browser where YouTube works.
2. Save the file as `/etc/ytdl-bot/cookies.txt`.
3. Make it readable by the service:
   ```sh
   sudo chown root:ytdl-bot /etc/ytdl-bot/cookies.txt
   sudo chmod 0640 /etc/ytdl-bot/cookies.txt
   ```
4. Set `cookies_file = "/etc/ytdl-bot/cookies.txt"` in the config and restart:
   ```sh
   sudo systemctl restart ytdl-bot
   ```

Cookies are sensitive. Treat the file like a password and do not commit it.

## Updates

### Automatic

`ytdl-bot-update.timer` runs daily around 04:00 local time (with a randomized
delay). `ytdl-bot-update`:

1. Reads the latest GitHub release tag.
2. Stops accepting new jobs and waits up to 30 minutes for active downloads.
3. Messages the first admin about the update (and postponement, if any).
4. Downloads and verifies the new bundle, runs its self-check.
5. Stops the bot, swaps `/opt/ytdl-bot`, restarts it, and checks stability.
6. Rolls back automatically if the new version fails to start.
7. Messages the admin with the result.

The update script is installed at `/usr/local/sbin/ytdl-bot-update`. The bot
reads a `maintenance` flag in its data directory so new jobs are not accepted
while an update is pending.

Check the schedule and logs:

```sh
systemctl list-timers ytdl-bot-update.timer
journalctl -u ytdl-bot-update -f
```

Run an update check manually:

```sh
sudo systemctl start ytdl-bot-update.service
```

### Releases

The workflow in `.github/workflows/release.yml`:

- Builds on every push to `main`.
- Runs daily at 20:00 UTC and rebuilds only when the `yt-dlp` binary hash or the
  FFmpeg archive hash changed.
- Publishes `ytdl-bot-linux-x86_64.tar.xz`, `versions.json`, and `SHA256SUMS`
  with tags like `v2026.11.24-7`.

The release bundle contains:

```text
ytdl-bot/
  ytdl-bot          # static x86_64 musl binary
  bin/yt-dlp
  bin/ffmpeg
  bin/ffprobe
  versions.json
  update.sh
  systemd/
  README.md
  LICENSE
  THIRD_PARTY_NOTICES.md
```

FFmpeg and yt-dlp binaries are never committed to git; they are attached to
GitHub releases instead.

GitHub pauses scheduled workflows after 60 days without repository activity.
If automatic update checks stop, open the Actions tab and re-enable the
workflow, or run it manually.

## Local development

The bundled tools are expected in the repository root for local runs:

```sh
# Grab the same tools used by CI
curl -fsSL -o yt-dlp https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp
chmod +x yt-dlp

curl -fsSL -o ffmpeg.tar.xz \
  https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/download/latest/ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz
tar -xJf ffmpeg.tar.xz --strip-components=2 -C . \
  ffmpeg-n9.0-latest-linux64-gpl-9.0/bin/ffmpeg \
  ffmpeg-n9.0-latest-linux64-gpl-9.0/bin/ffprobe
rm ffmpeg.tar.xz
```

Build and test:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
./target/debug/ytdl-bot --check
```

Run locally with a test bot token:

```sh
cp config.example.toml config.toml
# edit bot_token and use data_dir = "./data"
./target/debug/ytdl-bot --config config.toml
```

`--check` validates the config, finds and executes all three tools, verifies
the data directory is writable, and checks that FFmpeg has an MP3 encoder.

For a release build:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

You need `musl-tools` (or a musl cross compiler) installed.

## Troubleshooting

```sh
# Service state and recent logs
systemctl status ytdl-bot
journalctl -u ytdl-bot -n 100 --no-pager
journalctl -u ytdl-bot -f

# Validate the installation as the service user would see the files
/opt/ytdl-bot/ytdl-bot --check --config /etc/ytdl-bot/config.toml

# Check tool versions
/opt/ytdl-bot/bin/yt-dlp --version
/opt/ytdl-bot/bin/ffmpeg -version
```

Common issues:

- **"requested format is not available"** — YouTube changed formats; wait for
  the daily `yt-dlp` update or run the update service manually.
- **"Sign in to confirm you're not a bot"** — configure `cookies_file` (see above).
- **Uploads fail near 50 MB** — choose a lower quality or MP3; bots cannot exceed
  the Telegram limit.
- **Disk full** — downloads use `/var/lib/ytdl-bot/tmp`; failed jobs are cleaned
  up, but check `df -h` and remove stale directories if needed.

## Uninstall

```sh
sudo systemctl disable --now ytdl-bot.service ytdl-bot-update.timer
sudo rm -f /etc/systemd/system/ytdl-bot.service \
           /etc/systemd/system/ytdl-bot-update.service \
           /etc/systemd/system/ytdl-bot-update.timer
sudo systemctl daemon-reload
sudo rm -rf /opt/ytdl-bot /etc/ytdl-bot /var/lib/ytdl-bot /var/cache/ytdl-bot
sudo rm -f /usr/local/sbin/ytdl-bot-update
sudo userdel ytdl-bot
```

## License and third-party software

ytdl-bot itself is MIT licensed. It bundles and invokes third-party tools with
their own licenses; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

This project is not affiliated with YouTube, Google, Telegram, yt-dlp, or
FFmpeg. Downloading content may violate YouTube's Terms of Service or local
law; use it only for content you have the right to download.
