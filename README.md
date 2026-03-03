# TeamSpeak3 AI Bot + Music Bot

Rust TeamSpeak 3 bot with:

- AI chat/assistant actions (OpenAI-compatible APIs)
- permissions-aware moderation/admin helpers
- ticket workflow (`$ticket ...`)
- TTS playback (`$tts ...`)
- music/radio streaming (`$play`, `$radio`, `$addradio`, `$delradio`, `$editradio`, `$vol`, `$bass`)

## Features

- config-driven runtime (server, AI providers, IDs, paths, behavior)
- primary + fallback AI endpoints/models
- role model: User / Moderator / Admin
- per-user workspace memory and auto-learning
- audit logging for sensitive actions

## Community

- This project is experimental and may change quickly.
- Have an idea, bug report, or improvement? Open an Issue.
- Please read `CONTRIBUTING.md` before submitting a PR.
- License: `MIT` (`LICENSE`).

## What is safe to commit

Track these files in git:

- `config.example.toml`
- `.env.example`
- `tickets.example.json`
- `identities.example.json`
- `radios.example.json`

Do **not** commit local runtime/secrets:

- `config.toml`
- `.env`
- `identity.key`
- `tickets.json`, `identities.json`, `radios.json`
- `workspace/`

`/.gitignore` is set to keep those local files out of GitHub.

## Prerequisites

Required:

1. Rust (stable toolchain)
2. TeamSpeak 3 server access
3. `ffmpeg` in `PATH` (or set `TS3_BOT_FFMPEG_PATH` / `ffmpeg_binary_path`)

Optional but recommended:

1. `yt-dlp` for YouTube/SoundCloud and similar URLs
2. Piper + voice models for TTS

## Install Rust (one-time)

Linux/macOS:

- `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- restart shell, then verify with `rustc --version` and `cargo --version`

Windows:

- install from `https://rustup.rs/`
- restart terminal, then verify with `rustc --version` and `cargo --version`

Note: build artifacts are generated in `target/` and are ignored by Git.

## Setup (clean install)

1. Copy templates:

   - `cp config.example.toml config.toml`
   - `cp .env.example .env`
   - `cp tickets.example.json tickets.json`
   - `cp identities.example.json identities.json`
   - `cp radios.example.json radios.json`

2. Edit `config.toml`:

   - set `server_address`
   - set `bot_name`
   - set `channel` (optional)
   - set your `admin_uids`
   - adjust TeamSpeak IDs (`channel_admin_group_id`, `code_output_channel_id`, etc.)
   - set TTS/media paths if not using defaults

3. Put API keys in `.env` (recommended):

   - `TS3_BOT_AI_API_KEY`
   - `TS3_BOT_FALLBACK_API_KEY`

   Optional overrides:

   - `TS3_BOT_SERVER_ADDRESS`
   - `TS3_BOT_CHANNEL`
   - `TS3_BOT_FFMPEG_PATH`
   - `TS3_BOT_YT_DLP_PATH`

4. Build and run:

   - If running directly: `set -a; . ./.env; set +a` then `cargo run --release -- --config config.toml`

   or:

   - `./start_bot.sh` (auto-loads `.env` when present)

5. Install local git hooks (recommended, one-time):

   - `./install_git_hooks.sh`

## Runtime files behavior

- If `tickets.json`, `identities.json`, or `radios.json` do not exist, the bot starts with empty in-memory defaults.
- Those files are created automatically on first write.
- Copying `*.example.json` is optional, but useful for predictable first-run setup.

## Commands

- AI chat: `$ask <message>`
- TTS: `$tts <message>`
- Tickets:
  - `$ticket <message>`
  - `$ticket list`
  - `$ticket read <id>`
  - `$ticket reply <id> <text>`
  - `$ticket close <id>`
- Music:
  - `$play <url>`
  - `$stop`
  - `$vol [0-100]`
  - `$bass [1-100]`
- Radio presets:
  - `$radios`
  - `$radio <name>`
  - `$addradio "Name" <url>`
  - `$delradio <name>`
  - `$editradio "Name" <new_url>`

## Publish safely to GitHub

Local hooks and CI guard now block common secret leaks before push.

If hooks are not installed yet:

- `./install_git_hooks.sh`

Before every push:

1. Check tracked files:
   - `git status`
2. Confirm no local secrets are staged:
   - `git diff --cached`
3. Verify templates only:
   - keep keys empty in `config.example.toml` and `.env.example`
4. Rotate any key that was ever exposed outside local machine.

## Secret guard (pre-commit + pre-push)

- Local hook templates live in `githooks/pre-commit` and `githooks/pre-push`.
- The scanner is `secret_guard.py`.
- Manual checks:
  - staged changes: `python3 secret_guard.py staged`
  - branch/range: `python3 secret_guard.py range origin/main..HEAD`
  - full history: `python3 secret_guard.py history`
- False-positive exceptions:
  - repo-wide rules in `.secret-allowlist`
  - local-only rules in `.secret-allowlist.local` (ignored by git)

## Resource usage and minimum specs (preliminary)

These are practical starting estimates before formal profiling:

- CPU: 2 vCPU minimum, 4 vCPU recommended (TTS/music/transcoding spikes)
- RAM: 2 GB minimum, 4 GB recommended
- Disk: 2 GB free minimum for binary/logs/runtime data, 5+ GB recommended with voice models
- Network: stable low-latency connection to TS3 + AI endpoint

Expected runtime profile:

- Idle: low CPU, modest RAM
- AI requests: short CPU/network spikes
- TTS/music: highest CPU usage (Piper + ffmpeg/Opus encoding)

Measured baseline on this host (8 vCPU, 7.7 GiB RAM, 45s connected idle run):

- CPU: avg `0.36%`, peak `4.0%`
- Memory (RSS): avg `18.53 MiB`, peak `18.57 MiB`
- Virtual memory (VSZ): ~`686 MiB`
- Release binary size: `16 MiB` (`target/release/ts3-ai-bot`)
- Local runtime data during test: `workspace` `64 KiB`, `identities.json` `448 KiB`, others near `4 KiB`
- Optional Piper assets on this machine: `~513 MiB` (`piper/`)

Important: these are idle numbers; real load (AI/TTS/music) can be significantly higher.

## Profiling checklist (when you are ready)

Use this on the target machine while the bot is under normal load:

1. CPU/RAM live:
   - `top -p <bot_pid>`
   - `ps -p <bot_pid> -o %cpu,%mem,rss,vsz,etime,cmd`
2. Disk usage:
   - `du -sh .`
   - `du -sh workspace tickets.json identities.json radios.json audit.log`
3. Network/IO (optional):
   - `pidstat -dru -p <bot_pid> 1`

After collecting this data for idle + peak usage, update minimum specs with real values.
