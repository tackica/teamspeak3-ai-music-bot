# Contributing

Thanks for your interest in improving this project.

This bot is experimental, so feedback and contributions are welcome.
If you have an idea, bug report, or feature request, please open an Issue.

## Ways to help

- Report bugs with clear reproduction steps.
- Suggest improvements or new commands in Issues.
- Open pull requests for focused, testable changes.
- Improve docs and setup instructions.

## Before opening an Issue

- Use a clear title and expected behavior.
- Include steps to reproduce the problem.
- Include relevant logs/errors (without secrets).
- Include environment details when relevant (OS, Rust version).

## Pull request checklist

1. Keep the change focused and easy to review.
2. Install local hooks (one-time): `./install_git_hooks.sh`
3. Run secret guard on your staged changes: `python3 secret_guard.py staged`
4. Build before submitting: `cargo build --release`
5. Run tests when available: `cargo test`
6. Update docs when behavior or commands change.

## Security and secrets

- Never commit local secrets/runtime files such as `config.toml`, `.env`,
  `identity.key`, `tickets.json`, `identities.json`, `radios.json`, and `workspace/`.
- If a secret is ever exposed, rotate it immediately.
- Use `.secret-allowlist` only for narrow, justified false positives.

## Style

- Follow existing project conventions and keep changes minimal.
- Prefer clear commit messages that explain the reason for changes.
