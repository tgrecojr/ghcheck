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
- `src/github.rs` — shells out to `gh api graphql` to fetch open PRs scoped to `user:<login>`; uses `gh api user` to discover the current login at runtime
- `src/model.rs` — serde structs mirroring the GraphQL response, plus convenience methods (`is_failing`, `is_bot`, `rollup`, `author_login`)
- `src/render.rs` — filtering logic and colored table output, including a "Failing checks" detail section that names each failed check inline

Data flow: `main` → `github::current_user` → `github::fetch_prs(owner)` → `render::filter` → `render::print` (or JSON).

## Environment Variables

None. Authentication and identity come from the user's existing `gh` CLI session.

## Scope

Strictly the authenticated user's personal repos. The GraphQL search query is `is:pr is:open archived:false user:<login>`. Org repos are intentionally excluded.
