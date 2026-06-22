# ghcheck

## Overview

A small Rust CLI that prints a consolidated, color-coded view of the user's personal GitHub PRs and their CI status. Built to surface stuck Renovate/Dependabot PRs faster than the GitHub web UI can.

## Tech Stack

- Language: Rust (2021 edition)
- CLI parsing: `clap`
- Tables: `comfy-table`
- Colors: `owo-colors`
- JSON: `serde` / `serde_json`
- Time: `chrono`
- Authentication: shells out to `gh` CLI — no token handling in this codebase

## Commands

- `cargo build --release` — Build optimized binary at `target/release/ghcheck`
- `cargo run -- [flags]` — Run during development
- `cargo test` — Run tests
- `cargo clippy --all-targets -- -D warnings` — Lint
- `cargo fmt` — Format

## Architecture

Single-binary CLI with three internal modules:

- `src/main.rs` — clap arg parsing, top-level orchestration
- `src/github.rs` — shells out to `gh api graphql`. `fetch_prs` fetches open PRs scoped to `user:<login>`; `fetch_merged_prs` fetches PRs merged in the last 7 days and their merge-commit check rollup (post-merge CI). Uses `gh api user` to discover the current login at runtime
- `src/model.rs` — serde structs mirroring the GraphQL responses (generic `GraphQLResponse<T>` wrapper), plus convenience methods. `PullRequest` (`is_failing`, `is_bot`, `rollup`, `author_login`) and `MergedPullRequest` (`merge_rollup`, `is_failing`)
- `src/render.rs` — filtering logic and colored table output. Includes a "Failing checks" detail section for open PRs and a "Post-merge CI failures" section that names each failed default-branch check inline; both stay silent when empty. `latest_failing_per_repo` reduces the merged PRs to the most-recently-merged one per repo (then keeps only the still-failing ones) — see the post-merge note below

Data flow: `main` → `github::current_user` → `github::fetch_prs(owner)` + `github::fetch_merged_prs(owner, since)` → `render::filter` → `render::print` + `render::latest_failing_per_repo` → `render::print_post_merge` (or combined JSON under `open` / `post_merge_failures` keys).

The post-merge section exists because post-merge workflows (supply-chain scan, docker build+push) are triggered by the push to the default branch, so their check suite attaches to the **merge commit** — not the PR head commit the open-PR rollup reflects. Without this, a PR that merges green but fails its delivery pipeline is silent.

Within the 7-day window, only the **most-recently-merged PR per repo** is reported (and only if its post-merge CI is still failing). Each merge to the default branch re-runs the full pipeline over the rolled-up tree, so a later green merge supersedes earlier red ones — their changes are already in the passing build. `latest_failing_per_repo` runs over the full merged list before filtering by failure, so a later success can override earlier failures; the reduction is per-repo, so one red repo never suppresses another.

## Environment Variables

None. Authentication and identity come from the user's existing `gh` CLI session.

## Scope

Strictly the authenticated user's personal repos. The GraphQL search query is `is:pr is:open archived:false user:<login>`. Org repos are intentionally excluded.
