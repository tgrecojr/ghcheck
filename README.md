# ghcheck

A single command that gives you a consolidated, color-coded view of your personal GitHub repositories' open PRs and CI status — so you can quickly spot the ones that need attention (especially Renovate/Dependabot PRs blocked by failing checks).

## Why

The GitHub UI makes it tedious to answer "which of my PRs are stuck and why?" across many repos. `ghcheck` answers that in one terminal command:

- 🔴 PRs with failing CI (and *which* check failed)
- 🟡 PRs pending review or with running checks
- 🟢 PRs that are mergeable and just waiting on a click
- ⚠️ PRs with merge conflicts

Scoped strictly to your personal repos (`user:<your-login>`).

## Install

Requires the [GitHub CLI](https://cli.github.com/) (`gh`) to be installed and authenticated:

```bash
gh auth login
```

Then build and install:

```bash
cargo build --release
cp target/release/ghcheck ~/.local/bin/
```

(Make sure `~/.local/bin` is on your `PATH`.)

## Usage

```bash
ghcheck                      # all open PRs across your personal repos
ghcheck --failing            # only PRs with failing CI
ghcheck --bot                # only Renovate/Dependabot PRs
ghcheck --failing --bot      # the "stuck dependency PRs" view
ghcheck --no-drafts          # hide draft PRs
ghcheck --json               # machine-readable output
```

## How it works

`ghcheck` shells out to `gh api graphql` once with a single search query (`is:pr is:open user:<you>`) and parses the resulting `statusCheckRollup` per PR. No tokens to manage — it reuses your `gh` authentication.

## Tech stack

- Language: Rust (2021 edition)
- CLI: `clap`
- Tables: `comfy-table`
- Colors: `owo-colors`
- JSON: `serde` / `serde_json`
- Time: `chrono`
