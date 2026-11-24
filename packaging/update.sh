#!/usr/bin/env bash
# Update ytdl-bot to the latest GitHub release.
# Installed as /usr/local/sbin/ytdl-bot-update and run by a systemd timer.
set -Eeuo pipefail

REPO="komenome/ytdl-bot"
ASSET="ytdl-bot-linux-x86_64.tar.xz"
INSTALL_DIR="/opt/ytdl-bot"
CONFIG_FILE="/etc/ytdl-bot/config.toml"
STATE_DIR="/var/lib/ytdl-bot"
UPDATE_SCRIPT="/usr/local/sbin/ytdl-bot-update"
SERVICE="ytdl-bot.service"
MAINT_FLAG="/var/lib/ytdl-bot/maintenance"
MAX_WAIT_SECONDS=$((30 * 60))

TOKEN=""
ADMIN=""
TMP_DIR=""
OLD_DIR=""
NEW_INSTALLED=0
MAINT_OWNED=0

log() { printf '[ytdl-bot-update] %s\n' "$*"; }
die() { log "ERROR: $*"; exit 1; }

install_atomic() {
    local src="$1"
    local dst="$2"
    local mode="$3"
    local tmp="${dst}.new.$$"
    install -m "$mode" "$src" "$tmp"
    mv -f "$tmp" "$dst"
}

cleanup() {
    local status=$?
    if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
        rm -rf "$TMP_DIR"
    fi
    if [ "$MAINT_OWNED" -eq 1 ]; then
        rm -f "$MAINT_FLAG"
    fi
    if [ "$NEW_INSTALLED" -eq 1 ] && [ ! -d "$INSTALL_DIR" ] && [ -d "$OLD_DIR" ]; then
        mv "$OLD_DIR" "$INSTALL_DIR"
    fi
    exit "$status"
}
trap cleanup EXIT

notify() {
    [ -n "$ADMIN" ] && [ -n "$TOKEN" ] || return 0
    curl -fsS --max-time 20 --retry 2 -o /dev/null \
        --data-urlencode "chat_id=$ADMIN" \
        --data-urlencode "text=$1" \
        "https://api.telegram.org/bot${TOKEN}/sendMessage" \
        || log "Telegram notification failed"
}

read_token() {
    [ -r "$CONFIG_FILE" ] || return 0
    sed -n 's/^[[:space:]]*bot_token[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
        "$CONFIG_FILE" | head -n 1
}

read_admin() {
    python3 - "$STATE_DIR/state.json" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        data = json.load(handle)
    admins = data.get("admins") or []
    if admins:
        print(admins[0])
except Exception:
    pass
PY
}

read_version_field() {
    python3 - "$1" "$2" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        data = json.load(handle)
    print(data.get(sys.argv[2], ""))
except Exception:
    print("")
PY
}

active_jobs() {
    local value
    value=$(cat "$STATE_DIR/active_jobs" 2>/dev/null || true)
    if [[ "$value" =~ ^[0-9]+$ ]]; then
        echo "$value"
    else
        echo 0
    fi
}

rollback() {
    local reason="$1"
    log "rolling back: $reason"
    systemctl stop "$SERVICE" >/dev/null 2>&1 || true
    rm -rf "$INSTALL_DIR"
    mv "$OLD_DIR" "$INSTALL_DIR"
    NEW_INSTALLED=0
    if [ -f "$INSTALL_DIR/update.sh" ]; then
        install_atomic "$INSTALL_DIR/update.sh" "$UPDATE_SCRIPT" 0755
    fi
    if [ -d "$INSTALL_DIR/systemd" ]; then
        install -m 0644 "$INSTALL_DIR/systemd/ytdl-bot.service" \
            /etc/systemd/system/ytdl-bot.service
        install -m 0644 "$INSTALL_DIR/systemd/ytdl-bot-update.service" \
            /etc/systemd/system/ytdl-bot-update.service
        install -m 0644 "$INSTALL_DIR/systemd/ytdl-bot-update.timer" \
            /etc/systemd/system/ytdl-bot-update.timer
    fi
    systemctl daemon-reload
    systemctl start "$SERVICE" >/dev/null 2>&1 || true
    notify "❌ ytdl-bot update failed ($reason). Rolled back to ${CURRENT_TAG:-the previous version}."
    exit 1
}

[ "$(id -u)" -eq 0 ] || die "must run as root"

for command in curl tar xz sha256sum systemctl python3 flock; do
    command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done

exec 9>/run/ytdl-bot-update.lock
flock -n 9 || { log "another update is already running"; exit 0; }

TOKEN=$(read_token)
if [ -z "$TOKEN" ]; then
    log "no bot_token in $CONFIG_FILE; skipping notification and update"
    exit 0
fi
ADMIN=$(read_admin)

CURRENT_TAG=""
if [ -f "$INSTALL_DIR/versions.json" ]; then
    CURRENT_TAG=$(read_version_field "$INSTALL_DIR/versions.json" build_tag)
fi

LATEST_URL=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
    "https://github.com/${REPO}/releases/latest") \
    || die "could not resolve the latest release"
LATEST_TAG=${LATEST_URL##*/}
[ -n "$LATEST_TAG" ] || die "could not determine the latest release tag"

if [ "$CURRENT_TAG" = "$LATEST_TAG" ]; then
    log "already up to date ($CURRENT_TAG)"
    exit 0
fi

log "update available: ${CURRENT_TAG:-unknown} -> $LATEST_TAG"

# Ask the bot to stop accepting new jobs while we wait for the active ones.
[ -d "$STATE_DIR" ] || die "$STATE_DIR does not exist; is ytdl-bot installed?"
: > "$MAINT_FLAG"
MAINT_OWNED=1

# Do not interrupt downloads. Queued jobs also count as active.
deadline=$((SECONDS + MAX_WAIT_SECONDS))
waiting_notified=0
while :; do
    jobs=$(active_jobs)
    if [ "$jobs" -eq 0 ]; then
        break
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
        log "$jobs download(s) still active; postponing until the next timer"
        notify "⏳ ytdl-bot update ${CURRENT_TAG:-?} → $LATEST_TAG postponed: $jobs download(s) still active. Will retry at the next daily check."
        exit 0
    fi
    if [ "$waiting_notified" -eq 0 ]; then
        log "$jobs active download(s); waiting before updating"
        notify "⏳ ytdl-bot update ${CURRENT_TAG:-?} → $LATEST_TAG is waiting for $jobs active download(s) to finish."
        waiting_notified=1
    fi
    sleep 60
done

TMP_DIR=$(mktemp -d /tmp/ytdl-bot-update.XXXXXX)
DOWNLOAD_BASE="https://github.com/${REPO}/releases/download/${LATEST_TAG}"

log "downloading $ASSET"
curl -fsSL --retry 3 --retry-delay 5 -o "$TMP_DIR/$ASSET" \
    "$DOWNLOAD_BASE/$ASSET" || die "download failed"
curl -fsSL --retry 3 --retry-delay 5 -o "$TMP_DIR/SHA256SUMS" \
    "$DOWNLOAD_BASE/SHA256SUMS" || die "could not download SHA256SUMS"

log "verifying checksum"
(
    cd "$TMP_DIR"
    grep " ${ASSET}\$" SHA256SUMS | sha256sum -c - >/dev/null
) || die "checksum verification failed"

log "extracting"
tar -xJf "$TMP_DIR/$ASSET" -C "$TMP_DIR"
NEW_ROOT="$TMP_DIR/ytdl-bot"
[ -x "$NEW_ROOT/ytdl-bot" ] || die "release bundle has no ytdl-bot binary"
[ -x "$NEW_ROOT/bin/yt-dlp" ] || die "release bundle has no bin/yt-dlp"
[ -x "$NEW_ROOT/bin/ffmpeg" ] || die "release bundle has no bin/ffmpeg"
[ -x "$NEW_ROOT/bin/ffprobe" ] || die "release bundle has no bin/ffprobe"
bash -n "$NEW_ROOT/update.sh" || die "release bundle update.sh has invalid syntax"
NEW_TAG=$(read_version_field "$NEW_ROOT/versions.json" build_tag)
[ "$NEW_TAG" = "$LATEST_TAG" ] \
    || die "release bundle tag (${NEW_TAG:-missing}) does not match $LATEST_TAG"

log "checking the new build"
if ! "$NEW_ROOT/ytdl-bot" --check --config "$CONFIG_FILE" >>/var/log/ytdl-bot-update.log 2>&1; then
    tail -n 30 /var/log/ytdl-bot-update.log >&2 || true
    notify "❌ ytdl-bot update $LATEST_TAG failed its self-check. The current version is still running."
    exit 1
fi

notify "⬆️ Installing ytdl-bot ${CURRENT_TAG:-?} → $LATEST_TAG..."

# Give jobs that passed the maintenance check a moment to register, then make
# sure the bot is idle before stopping it. The maintenance flag prevents new
# jobs from starting while we are here.
sleep 5
jobs=$(active_jobs)
if [ "$jobs" -ne 0 ]; then
    log "$jobs download(s) started during the update; postponing"
    notify "⏳ ytdl-bot update $LATEST_TAG postponed: $jobs download(s) are still active. Will retry at the next daily check."
    exit 0
fi

log "stopping $SERVICE"
systemctl stop "$SERVICE" || die "could not stop $SERVICE"

OLD_DIR="${INSTALL_DIR}.old"
rm -rf "$OLD_DIR"
if [ -d "$INSTALL_DIR" ]; then
    mv "$INSTALL_DIR" "$OLD_DIR"
fi
mv "$NEW_ROOT" "$INSTALL_DIR"
NEW_INSTALLED=1

install_atomic "$INSTALL_DIR/update.sh" "$UPDATE_SCRIPT" 0755

units_changed=0
for unit in ytdl-bot.service ytdl-bot-update.service ytdl-bot-update.timer; do
    source="$INSTALL_DIR/systemd/$unit"
    target="/etc/systemd/system/$unit"
    if [ -f "$source" ] && ! cmp -s "$source" "$target"; then
        install -m 0644 "$source" "$target"
        units_changed=1
    fi
done
if [ "$units_changed" -eq 1 ]; then
    systemctl daemon-reload
fi

log "restarting $SERVICE"
systemctl start "$SERVICE" || rollback "systemctl start failed"

stable_checks=0
for _ in $(seq 1 12); do
    sleep 5
    if systemctl is-active --quiet "$SERVICE"; then
        stable_checks=$((stable_checks + 1))
        if [ "$stable_checks" -ge 2 ]; then
            break
        fi
    else
        stable_checks=0
    fi
done

if [ "$stable_checks" -lt 2 ]; then
    journalctl -u "$SERVICE" -n 40 --no-pager >&2 || true
    rollback "$SERVICE did not stay running"
fi

rm -rf "$OLD_DIR"
NEW_INSTALLED=0
rm -f "$MAINT_FLAG"
MAINT_OWNED=0

log "updated to $LATEST_TAG"
notify "✅ ytdl-bot updated: ${CURRENT_TAG:-?} → $LATEST_TAG. No downloads were interrupted."
