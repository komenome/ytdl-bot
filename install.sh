#!/usr/bin/env bash
# One-line installer for ytdl-bot.
#
#   curl -fsSL https://raw.githubusercontent.com/komenome/ytdl-bot/main/install.sh | sudo bash
#
# Environment overrides:
#   YTDL_BOT_TOKEN      bot token (skips the interactive prompt)
#   YTDL_BOT_TARBALL    install from a local release tarball (testing)
#   YTDL_BOT_SHA256SUMS matching SHA256SUMS file for YTDL_BOT_TARBALL
set -Eeuo pipefail

REPO="komenome/ytdl-bot"
ASSET="ytdl-bot-linux-x86_64.tar.xz"
RELEASES="https://github.com/${REPO}/releases"
INSTALL_DIR="/opt/ytdl-bot"
CONFIG_DIR="/etc/ytdl-bot"
CONFIG_FILE="${CONFIG_DIR}/config.toml"
STATE_DIR="/var/lib/ytdl-bot"
SERVICE_USER="ytdl-bot"
UPDATE_SCRIPT="/usr/local/sbin/ytdl-bot-update"
TMP_DIR=""
STAGE_DIR=""
OLD_DIR=""
CONFIG_TMP=""
INSTALL_OK=0

log() { printf '[ytdl-bot] %s\n' "$*"; }
warn() { printf '[ytdl-bot] WARNING: %s\n' "$*" >&2; }
die() { printf '[ytdl-bot] ERROR: %s\n' "$*" >&2; exit 1; }

cleanup() {
    local status=$?
    if [ -n "${CONFIG_TMP:-}" ]; then
        rm -f "$CONFIG_TMP"
    fi
    if [ -n "${TMP_DIR:-}" ]; then
        rm -rf "$TMP_DIR"
    fi
    if [ -n "${STAGE_DIR:-}" ]; then
        rm -rf "$STAGE_DIR"
    fi
    if [ "$status" -ne 0 ] && [ "$INSTALL_OK" -eq 0 ] \
        && [ -n "${OLD_DIR:-}" ] && [ -d "$OLD_DIR" ]; then
        log "restoring the previous installation"
        systemctl stop ytdl-bot.service >/dev/null 2>&1 || true
        rm -rf "$INSTALL_DIR"
        mv "$OLD_DIR" "$INSTALL_DIR"
        if [ -f "$INSTALL_DIR/update.sh" ]; then
            install -m 0755 "$INSTALL_DIR/update.sh" "$UPDATE_SCRIPT" || true
        fi
        systemctl daemon-reload >/dev/null 2>&1 || true
        systemctl start ytdl-bot.service >/dev/null 2>&1 || true
    fi
    exit "$status"
}
trap cleanup EXIT

[ "$(id -u)" -eq 0 ] || die "run this installer as root (curl ... | sudo bash)"

case "$(uname -s)" in
    Linux) ;;
    *) die "only Linux is supported" ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) ;;
    *) die "only x86_64 is supported (this server is $(uname -m))" ;;
esac

command -v systemctl >/dev/null 2>&1 || die "systemd is required"
if [ ! -d /run/systemd/system ]; then
    die "systemd does not appear to be running (containers without systemd are not supported)"
fi

missing=()
for command in curl tar xz sha256sum python3; do
    command -v "$command" >/dev/null 2>&1 || missing+=("$command")
done

install_dependencies() {
    log "installing required packages: ${missing[*]}"
    if command -v apt-get >/dev/null 2>&1; then
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -y
        apt-get install -y --no-install-recommends ca-certificates curl tar xz-utils python3
    elif command -v dnf >/dev/null 2>&1; then
        dnf install -y ca-certificates curl tar xz python3
    elif command -v yum >/dev/null 2>&1; then
        yum install -y ca-certificates curl tar xz python3
    elif command -v apk >/dev/null 2>&1; then
        apk add --no-cache ca-certificates curl tar xz python3
    else
        die "cannot install missing packages automatically; install: ca-certificates curl tar xz python3"
    fi
}

if [ "${#missing[@]}" -gt 0 ]; then
    install_dependencies
fi

for command in curl tar xz sha256sum python3 systemctl; do
    command -v "$command" >/dev/null 2>&1 || die "missing required command after installation: $command"
done

python3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 1)' \
    || die "python3 >= 3.9 is required by yt-dlp"

TOKEN="${YTDL_BOT_TOKEN:-}"
KEEP_CONFIG=0

if [ -f "$CONFIG_FILE" ]; then
    if [ -z "$TOKEN" ]; then
        if [ -r /dev/tty ]; then
            printf 'Existing config found at %s. Keep it and its token? [Y/n] ' "$CONFIG_FILE" > /dev/tty
            read -r answer < /dev/tty || answer=""
            case "${answer:-Y}" in
                [yY] | [yY][eE][sS] | "") KEEP_CONFIG=1 ;;
                *) KEEP_CONFIG=0 ;;
            esac
        else
            KEEP_CONFIG=1
        fi
    fi
fi

if [ "$KEEP_CONFIG" -eq 0 ] && [ -z "$TOKEN" ]; then
    [ -r /dev/tty ] || die "no TTY available. Set YTDL_BOT_TOKEN or run interactively."
    while :; do
        printf 'Telegram bot token (from @BotFather): ' > /dev/tty
        read -rs token < /dev/tty || die "could not read the token"
        printf '\n' > /dev/tty
        if [[ "$token" =~ ^[0-9]{5,}:[A-Za-z0-9_-]{20,}$ ]]; then
            TOKEN="$token"
            break
        fi
        printf 'That does not look like a bot token. Try again.\n' > /dev/tty
    done
fi

if [ "$KEEP_CONFIG" -eq 0 ]; then
    [[ "$TOKEN" =~ ^[0-9]{5,}:[A-Za-z0-9_-]{20,}$ ]] \
        || die "invalid bot token. Expected the value from @BotFather, like 123456789:AA..."
fi

BOT_USERNAME=""
if [ -n "$TOKEN" ]; then
    log "verifying the bot token with Telegram"
    get_me=$(curl -fsS --max-time 20 \
        "https://api.telegram.org/bot${TOKEN}/getMe") \
        || die "Telegram rejected the bot token (or the API is unreachable)"
    BOT_USERNAME=$(printf '%s' "$get_me" \
        | sed -n 's/.*"username":"\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$BOT_USERNAME" ] || die "unexpected response from Telegram getMe"
fi

if ! id -u "$SERVICE_USER" >/dev/null 2>&1; then
    log "creating system user $SERVICE_USER"
    if command -v useradd >/dev/null 2>&1; then
        useradd --system --user-group --home-dir "$STATE_DIR" \
            --shell /usr/sbin/nologin "$SERVICE_USER"
    else
        adduser -S -D -H -h "$STATE_DIR" -s /sbin/nologin "$SERVICE_USER"
    fi
fi

log "downloading the latest release"
TMP_DIR=$(mktemp -d /tmp/ytdl-bot-install.XXXXXX)

if [ -n "${YTDL_BOT_TARBALL:-}" ]; then
    [ -f "$YTDL_BOT_TARBALL" ] || die "YTDL_BOT_TARBALL not found: $YTDL_BOT_TARBALL"
    cp "$YTDL_BOT_TARBALL" "$TMP_DIR/$ASSET"
    if [ -n "${YTDL_BOT_SHA256SUMS:-}" ]; then
        [ -f "$YTDL_BOT_SHA256SUMS" ] || die "YTDL_BOT_SHA256SUMS not found: $YTDL_BOT_SHA256SUMS"
        cp "$YTDL_BOT_SHA256SUMS" "$TMP_DIR/SHA256SUMS"
    fi
else
    curl -fsSL --retry 3 --retry-delay 5 -o "$TMP_DIR/$ASSET" \
        "$RELEASES/latest/download/$ASSET" || die "could not download $ASSET"
    curl -fsSL --retry 3 --retry-delay 5 -o "$TMP_DIR/SHA256SUMS" \
        "$RELEASES/latest/download/SHA256SUMS" || die "could not download SHA256SUMS"
fi

if [ -f "$TMP_DIR/SHA256SUMS" ]; then
    log "verifying checksum"
    (
        cd "$TMP_DIR"
        grep " ${ASSET}\$" SHA256SUMS | sha256sum -c - >/dev/null
    ) || die "checksum verification failed"
else
    warn "SHA256SUMS not provided; skipping checksum verification (testing only)"
fi

log "extracting"
tar --no-same-owner -xJf "$TMP_DIR/$ASSET" -C "$TMP_DIR"
SRC="$TMP_DIR/ytdl-bot"
[ -x "$SRC/ytdl-bot" ] || die "release bundle has no ytdl-bot binary"
[ -x "$SRC/bin/yt-dlp" ] || die "release bundle has no bin/yt-dlp"
[ -x "$SRC/bin/ffmpeg" ] || die "release bundle has no bin/ffmpeg"
[ -x "$SRC/bin/ffprobe" ] || die "release bundle has no bin/ffprobe"
[ -f "$SRC/update.sh" ] || die "release bundle has no update.sh"
[ -f "$SRC/systemd/ytdl-bot.service" ] || die "release bundle has no systemd units"

log "preparing installation"
mkdir -p "$CONFIG_DIR" "$STATE_DIR"
chown "$SERVICE_USER:$SERVICE_USER" "$STATE_DIR"
chmod 0750 "$STATE_DIR"
mkdir -p "$STATE_DIR/tmp" "$STATE_DIR/.cache"
chown -R "$SERVICE_USER:$SERVICE_USER" "$STATE_DIR/tmp" "$STATE_DIR/.cache"
rm -f "$STATE_DIR/maintenance"

STAGE_DIR="/opt/.ytdl-bot.install.$$"
rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR"
cp -a "$SRC/." "$STAGE_DIR/"
chmod 0755 "$STAGE_DIR/ytdl-bot" "$STAGE_DIR/bin/yt-dlp" \
    "$STAGE_DIR/bin/ffmpeg" "$STAGE_DIR/bin/ffprobe" "$STAGE_DIR/update.sh"
chown -R root:root "$STAGE_DIR"
chmod -R go-w "$STAGE_DIR"

if systemctl is-active --quiet ytdl-bot.service 2>/dev/null; then
    log "stopping the existing service"
fi

if [ "$KEEP_CONFIG" -eq 0 ]; then
    log "preparing $CONFIG_FILE"
    CONFIG_TMP="${CONFIG_FILE}.new.$$"
    umask 077
    cat > "$CONFIG_TMP" <<EOF
# ytdl-bot configuration. See https://github.com/${REPO}#configuration
bot_token = "${TOKEN}"

# Leave admin_ids empty to let the first /start user become admin.
admin_ids = []
allowed_users = []

max_concurrent_downloads = 2
max_upload_mb = 50
max_video_height = 1080
max_duration_secs = 7200
download_timeout_secs = 1800
info_timeout_secs = 60

data_dir = "${STATE_DIR}"
cookies_file = ""
EOF
    chown root:"$SERVICE_USER" "$CONFIG_TMP"
    chmod 0640 "$CONFIG_TMP"
    CHECK_CONFIG="$CONFIG_TMP"
else
    log "keeping the existing $CONFIG_FILE"
    chown root:"$SERVICE_USER" "$CONFIG_FILE" 2>/dev/null || true
    chmod 0640 "$CONFIG_FILE" 2>/dev/null || true
    CHECK_CONFIG="$CONFIG_FILE"
fi

log "running a self-check"
if ! "$STAGE_DIR/ytdl-bot" --check --config "$CHECK_CONFIG"; then
    die "installation failed the self-check; nothing was changed"
fi

systemctl stop ytdl-bot.service >/dev/null 2>&1 || true

if [ -n "$CONFIG_TMP" ]; then
    mv -f "$CONFIG_TMP" "$CONFIG_FILE"
    CONFIG_TMP=""
fi

OLD_DIR="${INSTALL_DIR}.installer-old"
rm -rf "$OLD_DIR"
if [ -d "$INSTALL_DIR" ]; then
    mv "$INSTALL_DIR" "$OLD_DIR"
fi
mv "$STAGE_DIR" "$INSTALL_DIR"
STAGE_DIR=""

install -m 0755 "$INSTALL_DIR/update.sh" "$UPDATE_SCRIPT"
install -m 0644 "$INSTALL_DIR/systemd/ytdl-bot.service" /etc/systemd/system/ytdl-bot.service
install -m 0644 "$INSTALL_DIR/systemd/ytdl-bot-update.service" /etc/systemd/system/ytdl-bot-update.service
install -m 0644 "$INSTALL_DIR/systemd/ytdl-bot-update.timer" /etc/systemd/system/ytdl-bot-update.timer

systemctl daemon-reload
systemctl enable ytdl-bot.service ytdl-bot-update.timer >/dev/null
systemctl restart ytdl-bot.service
systemctl start ytdl-bot-update.timer

log "waiting for the service to start"
started=0
for _ in $(seq 1 12); do
    sleep 2
    if systemctl is-active --quiet ytdl-bot.service; then
        started=$((started + 1))
        [ "$started" -ge 2 ] && break
    else
        started=0
    fi
done

if [ "$started" -lt 2 ]; then
    journalctl -u ytdl-bot.service -n 60 --no-pager || true
    warn "the service did not stay running; restoring the previous installation"
    systemctl stop ytdl-bot.service >/dev/null 2>&1 || true
    rm -rf "$INSTALL_DIR"
    if [ -d "$OLD_DIR" ]; then
        mv "$OLD_DIR" "$INSTALL_DIR"
        install -m 0755 "$INSTALL_DIR/update.sh" "$UPDATE_SCRIPT" 2>/dev/null || true
        systemctl daemon-reload || true
        systemctl start ytdl-bot.service >/dev/null 2>&1 || true
        die "installation failed to start; the previous version was restored"
    fi
    die "installation failed to start; check the logs above"
fi

INSTALL_OK=1
rm -rf "$OLD_DIR"

echo
log "✅ ytdl-bot is installed and running."
if [ -n "$BOT_USERNAME" ]; then
    echo "    Bot:      @${BOT_USERNAME}"
fi
echo "    Config:   $CONFIG_FILE"
echo "    State:    $STATE_DIR"
echo "    Logs:     journalctl -u ytdl-bot -f"
echo "    Updates:  systemctl list-timers ytdl-bot-update.timer"
echo
echo "Open Telegram, send /start to the bot, and the first user becomes the admin."
echo "Then allow other users with /allow <telegram_user_id>."
