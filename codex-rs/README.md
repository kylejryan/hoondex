# Hoondex CLI

[**Hoondex CLI Documentation**](https://developers.openai.com/codex/cli)

## Local development

The `hoondex` binary's dependency tree spans ~108 of the workspace's 122 crates, so a
full `cargo build` is inherently expensive. Iterate with `cargo check` (skips codegen and
linking — the slow, CPU-/heat-heavy steps) and only `build`/`run` when you actually need to
execute the binary. Convenience aliases are defined in [`.cargo/config.toml`](.cargo/config.toml):

| Command | Runs | Use for |
| --- | --- | --- |
| `cargo ck` | `check --workspace --all-targets` | Primary inner loop — type-check everything, no codegen/link |
| `cargo ckc` | `check -p codex-cli` | Tighter loop — CLI tree only |
| `cargo lint` | `clippy --workspace --all-targets -- -D warnings` | Match CI before pushing |
| `cargo hx -- <args>` | `run -p codex-cli --bin hoondex` | Build + run the binary |

Notes:

- After a cold warm-up, a `cargo ck` following a one-line edit completes in seconds thanks to
  incremental compilation. Editing a low-level crate (`core`, `protocol`) rebuilds everything
  downstream — that's inherent to the dependency graph.
- `target/` is a cache (tens of GB is normal). Avoid `cargo clean`: it forces a full rebuild of
  all ~108 crates plus dependencies.
- The dev profile builds dependencies at `opt-level = 1` for fast, cool builds, keeping the
  render-critical TUI crates (`ratatui`, `crossterm`, `syntect`) at `opt-level = 3` so the
  interface stays smooth. This is local-only; CI uses its own profiles.
