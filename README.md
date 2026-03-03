# TeamSpeak3 AI Bot + Music Bot

A TeamSpeak 3 bot written in Rust with:

- AI chat/assistant actions (OpenAI-compatible API endpoints)
- TeamSpeak moderation/admin helpers (permissions-aware)
- Ticket workflow (`$ticket ...`)
- TTS playback (`$tts ...`)
- Music and radio streaming (`$play`, `$radio`, `$addradio`, `$delradio`, `$editradio`)

## Features

- Config-driven runtime (server, AI providers, file paths, TeamSpeak IDs, TTS paths)
- Primary + fallback AI model endpoints
- Permission model: User / Moderator / Admin
- Per-user workspace memory and auto-learning
- Audit logging for sensitive actions

## Quick start

1. Install Rust stable.
2. Copy config template:
   - `cp config.example.toml config.toml`
3. Edit `config.toml` with your TeamSpeak and model settings.
4. (Recommended) Set keys using environment variables instead of `config.toml`:
   - `TS3_BOT_AI_API_KEY`
   - `TS3_BOT_FALLBACK_API_KEY`
5. Run:
   - `cargo run --release -- --config config.toml`

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
- Radio presets:
  - `$radios`
  - `$radio <name>`
  - `$addradio "Name" <url>`
  - `$delradio <name>`
  - `$editradio "Name" <new_url>`

## Privacy and publishing checklist

Before publishing your repository, make sure:

- `config.toml` is not committed (use `config.example.toml` only)
- API keys are not hardcoded in tracked files
- Runtime files are ignored (`identity.key`, `tickets.json`, `identities.json`, `workspace/`)
- You rotate any key that was ever exposed

`.gitignore` in this repository is set up to block common private/runtime files.
