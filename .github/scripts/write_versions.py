#!/usr/bin/env python3
"""Write the versions.json metadata file for a ytdl-bot release bundle."""
import datetime
import json
import os
import pathlib
import re
import sys


def main() -> int:
    output = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "versions.json")
    cargo = pathlib.Path("Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', cargo)
    bot_version = match.group(1) if match else ""

    built_at = (
        datetime.datetime.now(datetime.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )

    data = {
        "build_tag": os.environ.get("BUILD_TAG", ""),
        "bot_version": bot_version,
        "commit": os.environ.get("GIT_SHA", ""),
        "built_at": built_at,
        "yt_dlp": os.environ.get("YTDLP_VERSION", ""),
        "ffmpeg": os.environ.get("FFMPEG_VERSION", ""),
        "yt_dlp_sig": os.environ.get("YTDLP_SIG", ""),
        "ffmpeg_sig": os.environ.get("FFMPEG_SIG", ""),
        "ffmpeg_asset": os.environ.get("FFMPEG_ASSET", ""),
    }

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
