# Third-party notices

ytdl-bot is MIT licensed, but its release bundle contains third-party programs
with separate licenses. This file summarizes them; the upstream projects are
the authoritative source.

## yt-dlp

- Project: https://github.com/yt-dlp/yt-dlp
- License: Unlicense (public domain dedication), with some bundled components
  under their own licenses as documented in the yt-dlp repository.
- Distribution: the `yt-dlp` zipapp binary from the official GitHub releases is
  included unmodified in each release bundle.

## FFmpeg / FFprobe

- Project: https://ffmpeg.org/
- Source code: https://github.com/FFmpeg/FFmpeg and
  https://github.com/FFmpeg-Builds/FFmpeg-Builds
- License: FFmpeg is LGPL-2.1-or-later by default. The binaries bundled here are
  from the FFmpeg-Builds `linux64-gpl` build, which enables GPL components
  (including x264/x265) and is therefore distributed under the GNU General
  Public License, version 3 or later.
- Distribution: `ffmpeg` and `ffprobe` binaries from the official FFmpeg-Builds
  `ffmpeg-n9.0-latest-linux64-gpl-9.0` archive are included unmodified in each
  release bundle.
- Compliance: the corresponding FFmpeg and FFmpeg-Builds source is available at
  the URLs above. The build configuration and license text are published with
  the FFmpeg-Builds releases.

Because the bundle includes GPL-licensed FFmpeg binaries, redistribution of the
whole bundle must comply with the GPL for those binaries. The ytdl-bot source
code itself remains MIT licensed. If you redistribute modified FFmpeg binaries,
you must also make the corresponding source available.

## Rust dependencies

ytdl-bot is built with Rust crates listed in `Cargo.lock`, primarily under the
MIT and Apache-2.0 licenses. Notable dependencies include:

- [teloxide](https://github.com/teloxide/teloxide) — MIT
- [tokio](https://github.com/tokio-rs/tokio) — MIT
- [reqwest](https://github.com/seanmonstar/reqwest) — MIT/Apache-2.0
- [serde](https://github.com/serde-rs/serde) — MIT/Apache-2.0
- [dashmap](https://github.com/xacrimon/dashmap) — MIT
- [ring](https://github.com/briansmith/ring) — ISC-style

Run `cargo install cargo-license && cargo license` for a complete dependency
license report.

## Trademarks

YouTube and Google are trademarks of Google LLC. Telegram is a trademark of
Telegram FZ-LLC. This project is not affiliated with or endorsed by any of
these companies.
