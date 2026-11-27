# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [0.0.1] - 2026-11-27

First release.

### Added
- Plan limits panel: a dial for each limit window Anthropic reports (5-hour session, weekly, per-model weekly), with percent used and time until reset. These limits are shared by claude.ai and Claude Code.
- Claude Code token panel: daily bar chart for the last 7, 14 or 30 calendar days (UTC), with input, output and cache breakdown on hover. The chosen range is remembered between launches.
- Per-model totals across all local Claude Code logs.
- Automatic refresh every 5 seconds, off the UI thread, with a status dot and plain-language errors (signed out, login expired, rate limited, offline).
- Reads the Claude Code login from `~/.claude/.credentials.json` on Windows and Linux, and from the Keychain on macOS.
- Builds for Windows x64, Linux x64 and macOS Apple Silicon, published by the release workflow.

[0.0.1]: https://github.com/DID-Software-Solutions/claudewatch/releases/tag/v0.0.1
