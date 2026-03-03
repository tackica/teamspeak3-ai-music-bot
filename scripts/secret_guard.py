#!/usr/bin/env python3
from __future__ import annotations

import argparse
import fnmatch
import pathlib
import re
import subprocess
import sys
from dataclasses import dataclass
from typing import Iterable

ZERO_SHA = "0" * 40
ALLOWLIST_FILES = (".secret-allowlist", ".secret-allowlist.local")

BLOCKED_BASENAMES = {
    "config.toml",
    "identity.key",
    "identities.json",
    "tickets.json",
    "radios.json",
    "instances.json",
    "id_rsa",
    "id_dsa",
    "known_hosts",
}

BLOCKED_SUFFIXES = (".pem", ".key", ".p12", ".pfx", ".jks")

PLACEHOLDER_HINTS = (
    "example",
    "replace",
    "changeme",
    "placeholder",
    "your_",
    "dummy",
    "sample",
    "test_",
    "<",
    "${",
)

HUNK_HEADER_RE = re.compile(r"@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")


@dataclass(frozen=True)
class SecretPattern:
    name: str
    regex: re.Pattern[str]
    secret_group: int | None = None


SECRET_PATTERNS: tuple[SecretPattern, ...] = (
    SecretPattern(
        "private key block",
        re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"),
    ),
    SecretPattern(
        "GitHub token",
        re.compile(r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36}\b"),
    ),
    SecretPattern(
        "GitHub fine-grained token",
        re.compile(r"\bgithub_pat_[A-Za-z0-9_]{80,}\b"),
    ),
    SecretPattern(
        "AWS access key",
        re.compile(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
    ),
    SecretPattern(
        "OpenAI-style key",
        re.compile(r"\bsk-[A-Za-z0-9]{20,}\b"),
    ),
    SecretPattern(
        "NVIDIA API key",
        re.compile(r"\bnvapi-[A-Za-z0-9_-]{20,}\b"),
    ),
    SecretPattern(
        "Slack token",
        re.compile(r"\bxox[baprs]-[0-9A-Za-z-]{10,}\b"),
    ),
    SecretPattern(
        "URL with embedded credentials",
        re.compile(r"[A-Za-z][A-Za-z0-9+.-]*://[^\s:@/]+:[^\s@/]+@"),
    ),
    SecretPattern(
        "hard-coded secret assignment",
        re.compile(
            r"(?i)\b(?:api[_-]?key|token|secret|password|passwd|client_secret)\b\s*[:=]\s*[\"']([^\"']{8,})[\"']"
        ),
        secret_group=1,
    ),
    SecretPattern(
        "Bearer token literal",
        re.compile(r"(?i)\bAuthorization\b\s*[:=]\s*[\"']?Bearer\s+([A-Za-z0-9._-]{12,})"),
        secret_group=1,
    ),
)


@dataclass(frozen=True)
class Finding:
    location: str
    reason: str
    snippet: str


class Allowlist:
    def __init__(self) -> None:
        self.path_globs: list[str] = []
        self.line_substrings: list[str] = []
        self.line_regexes: list[re.Pattern[str]] = []

    def path_allowed(self, path: str) -> bool:
        return any(fnmatch.fnmatch(path, pattern) for pattern in self.path_globs)

    def line_allowed(self, path: str, line: str) -> bool:
        if self.path_allowed(path):
            return True
        if any(text in line for text in self.line_substrings):
            return True
        return any(rx.search(line) for rx in self.line_regexes)


def run_git(*args: str) -> bytes:
    proc = subprocess.run(["git", *args], capture_output=True)
    if proc.returncode != 0:
        stderr = proc.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"git {' '.join(args)} failed: {stderr}")
    return proc.stdout


def run_git_text(*args: str) -> str:
    return run_git(*args).decode("utf-8", errors="replace")


def get_repo_root() -> pathlib.Path:
    root = run_git_text("rev-parse", "--show-toplevel").strip()
    return pathlib.Path(root)


def normalize_path(path: str) -> str:
    path = path.replace("\\", "/")
    while path.startswith("./"):
        path = path[2:]
    return path


def load_allowlist(repo_root: pathlib.Path) -> Allowlist:
    allowlist = Allowlist()
    for name in ALLOWLIST_FILES:
        file_path = repo_root / name
        if not file_path.exists():
            continue
        lines = file_path.read_text(encoding="utf-8", errors="replace").splitlines()
        for raw_line in lines:
            line = raw_line.strip()
            if not line or line.startswith("#"):
                continue
            if line.startswith("path:"):
                value = line.split(":", 1)[1].strip()
                if value:
                    allowlist.path_globs.append(normalize_path(value))
                continue
            if line.startswith("line:"):
                value = line.split(":", 1)[1]
                if value:
                    allowlist.line_substrings.append(value)
                continue
            if line.startswith("regex:"):
                value = line.split(":", 1)[1].strip()
                if value:
                    allowlist.line_regexes.append(re.compile(value))
                continue
            allowlist.line_substrings.append(line)
    return allowlist


def blocked_path_reason(path: str, allowlist: Allowlist) -> str | None:
    if allowlist.path_allowed(path):
        return None

    basename = pathlib.PurePosixPath(path).name

    if basename.startswith(".env") and basename != ".env.example":
        return "environment file is blocked"

    if basename in BLOCKED_BASENAMES:
        return f"{basename} is blocked"

    if any(basename.endswith(suffix) for suffix in BLOCKED_SUFFIXES):
        return f"{basename} looks like a key/certificate file"

    return None


def looks_like_placeholder(value: str) -> bool:
    lowered = value.lower()
    if any(hint in lowered for hint in PLACEHOLDER_HINTS):
        return True

    compact = re.sub(r"[^a-z0-9]", "", lowered)
    if len(compact) >= 12 and len(set(compact)) <= 2:
        return True

    return False


def redact(line: str, span: tuple[int, int]) -> str:
    start, end = span
    redacted = f"{line[:start]}<redacted>{line[end:]}"
    return redacted.strip()[:180]


def scan_line(path: str, line_number: int, line: str, allowlist: Allowlist) -> list[Finding]:
    if allowlist.line_allowed(path, line):
        return []

    findings: list[Finding] = []

    for pattern in SECRET_PATTERNS:
        for match in pattern.regex.finditer(line):
            candidate = match.group(pattern.secret_group or 0)
            if looks_like_placeholder(candidate):
                continue
            findings.append(
                Finding(
                    location=f"{path}:{line_number}",
                    reason=pattern.name,
                    snippet=redact(line, match.span()),
                )
            )

    return findings


def parse_z_paths(raw: bytes) -> list[str]:
    if not raw:
        return []
    chunks = [chunk.decode("utf-8", errors="replace") for chunk in raw.split(b"\x00") if chunk]
    return [normalize_path(chunk) for chunk in chunks]


def scan_staged(allowlist: Allowlist) -> list[Finding]:
    findings: list[Finding] = []
    staged_paths = parse_z_paths(run_git("diff", "--cached", "--name-only", "--diff-filter=ACMR", "-z"))

    for path in staged_paths:
        reason = blocked_path_reason(path, allowlist)
        if reason:
            findings.append(
                Finding(location=path, reason=reason, snippet="remove it from staging"),
            )

        blob = run_git("show", f":{path}")
        if b"\x00" in blob:
            continue

        text = blob.decode("utf-8", errors="replace")
        for line_number, line in enumerate(text.splitlines(), start=1):
            findings.extend(scan_line(path, line_number, line, allowlist))

    return findings


def parse_name_status_output(output: str) -> Iterable[str]:
    for raw in output.splitlines():
        if not raw.strip():
            continue
        parts = raw.split("\t")
        status = parts[0]
        if status.startswith("R") and len(parts) >= 3:
            yield normalize_path(parts[2])
        elif len(parts) >= 2:
            yield normalize_path(parts[1])


def scan_commit(commit: str, allowlist: Allowlist) -> list[Finding]:
    findings: list[Finding] = []

    changed_paths = run_git_text("diff-tree", "--no-commit-id", "--name-status", "-r", "--diff-filter=ACMR", commit)
    for path in parse_name_status_output(changed_paths):
        reason = blocked_path_reason(path, allowlist)
        if reason:
            findings.append(
                Finding(
                    location=f"{commit[:12]}:{path}",
                    reason=reason,
                    snippet="file path is forbidden in pushed commits",
                )
            )

    patch = run_git_text("show", "--format=", "--unified=0", "--no-color", "--no-ext-diff", commit)
    current_path = ""
    current_line = 0

    for raw_line in patch.splitlines():
        if raw_line.startswith("+++ b/"):
            current_path = normalize_path(raw_line[6:])
            current_line = 0
            continue

        hunk_match = HUNK_HEADER_RE.match(raw_line)
        if hunk_match:
            current_line = int(hunk_match.group(1))
            continue

        if raw_line.startswith("+") and not raw_line.startswith("+++") and current_path:
            line = raw_line[1:]
            findings.extend(scan_line(current_path, current_line, line, allowlist))
            current_line += 1
            continue

        if raw_line.startswith(" ") and current_path and current_line > 0:
            current_line += 1

    return findings


def rev_list(*args: str) -> list[str]:
    output = run_git_text("rev-list", "--reverse", *args)
    return [line.strip() for line in output.splitlines() if line.strip()]


def scan_commits(commits: Iterable[str], allowlist: Allowlist) -> list[Finding]:
    findings: list[Finding] = []
    for commit in commits:
        findings.extend(scan_commit(commit, allowlist))
    return findings


def scan_range(range_spec: str, allowlist: Allowlist) -> list[Finding]:
    commits = rev_list(range_spec)
    return scan_commits(commits, allowlist)


def unique_in_order(values: Iterable[str]) -> list[str]:
    seen: set[str] = set()
    ordered: list[str] = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        ordered.append(value)
    return ordered


def parse_push_updates(stdin_text: str) -> list[tuple[str, str, str, str]]:
    updates: list[tuple[str, str, str, str]] = []
    for raw in stdin_text.splitlines():
        line = raw.strip()
        if not line:
            continue
        parts = line.split()
        if len(parts) != 4:
            continue
        local_ref, local_sha, remote_ref, remote_sha = parts
        updates.append((local_ref, local_sha, remote_ref, remote_sha))
    return updates


def commits_for_new_remote_ref(local_sha: str, remote_name: str | None) -> list[str]:
    if remote_name:
        commits = rev_list(local_sha, "--not", f"--remotes={remote_name}")
        if commits:
            return commits
    return rev_list(local_sha, "--not", "--remotes")


def scan_pre_push(remote_name: str | None, stdin_text: str, allowlist: Allowlist) -> list[Finding]:
    updates = parse_push_updates(stdin_text)
    commits: list[str] = []

    for _local_ref, local_sha, _remote_ref, remote_sha in updates:
        if local_sha == ZERO_SHA:
            continue
        if remote_sha == ZERO_SHA:
            commits.extend(commits_for_new_remote_ref(local_sha, remote_name))
        else:
            commits.extend(rev_list(f"{remote_sha}..{local_sha}"))

    commits = unique_in_order(commits)
    return scan_commits(commits, allowlist)


def print_findings(findings: list[Finding]) -> None:
    print("Secret guard blocked this operation.", file=sys.stderr)
    print("Potential secrets were found:", file=sys.stderr)
    for finding in findings:
        print(f"- {finding.location}: {finding.reason}", file=sys.stderr)
        print(f"  {finding.snippet}", file=sys.stderr)
    print("", file=sys.stderr)
    print("Fix the leak and retry.", file=sys.stderr)
    print("If this is a false positive, add a narrow exception to .secret-allowlist.", file=sys.stderr)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Block commits/pushes that contain secrets.")
    subparsers = parser.add_subparsers(dest="command", required=True)

    subparsers.add_parser("staged", help="Scan staged files (for pre-commit).")

    range_parser = subparsers.add_parser("range", help="Scan commits in a rev range.")
    range_parser.add_argument("range_spec", help="Git rev range, e.g. origin/main..HEAD")

    pre_push_parser = subparsers.add_parser("pre-push", help="Scan commits being pushed.")
    pre_push_parser.add_argument("remote_name", nargs="?", default="", help="Remote name from git hook")
    pre_push_parser.add_argument("remote_url", nargs="?", default="", help="Remote URL from git hook")

    subparsers.add_parser("history", help="Scan all commits in repository history.")

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    repo_root = get_repo_root()
    allowlist = load_allowlist(repo_root)

    try:
        if args.command == "staged":
            findings = scan_staged(allowlist)
        elif args.command == "range":
            findings = scan_range(args.range_spec, allowlist)
        elif args.command == "pre-push":
            stdin_text = sys.stdin.read()
            remote_name = args.remote_name.strip() or None
            findings = scan_pre_push(remote_name, stdin_text, allowlist)
        elif args.command == "history":
            findings = scan_commits(rev_list("--all"), allowlist)
        else:
            parser.error(f"unsupported command: {args.command}")
    except RuntimeError as exc:
        print(f"secret_guard.py failed: {exc}", file=sys.stderr)
        return 2

    if findings:
        print_findings(findings)
        return 1

    print("Secret guard: no leaks detected.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
