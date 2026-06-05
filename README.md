<strong>Hoondex CLI</strong> is a coding agent that runs locally on your computer, powered by the Hoonify-hosted DeepSeek V4 Pro model.

---

## Quickstart

Hoondex is built and run from source (see [Running from source](#running-from-source)). The fastest path:

```shell
# from the repo root
ln -s "$(pwd)/hoondex-dev" ~/.local/bin/hoondex-dev
hoondex-dev
```

This puts the launcher on your `PATH`, so `hoondex-dev` (and the release `hoondex`
set up below) run from **any directory** — `cd` into whatever project you want and
just run it. If the command isn't found, make sure `~/.local/bin` is on your
`PATH` (add it to your shell profile and restart your shell):

```shell
export PATH="$HOME/.local/bin:$PATH"
```

Set your Hoonify API key before first use (export it from your shell profile so
it's set in every session):

```shell
export HOONIFY_API_KEY=...   # create one on the Hoonify dashboard
```

## Running from source

If you're developing on this repo, build and run `hoondex` straight from your
local checkout instead of installing a release.

| You run                                              | Builds?                                  | Reflects latest source?                            |
| ---------------------------------------------------- | ---------------------------------------- | -------------------------------------------------- |
| `hoondex`                                            | ❌ no — runs the existing release binary | only as of the last `cargo build --release`        |
| `hoondex-dev`                                         | ✅ yes (fast incremental debug)          | ✅ always — and it won't launch if the build fails |
| `cargo build --release --bin hoondex` then `hoondex` | ✅ yes (slow release)                     | ✅ as of that build                                |

### Use `hoondex-dev` (always-fresh debug build)

[`hoondex-dev`](./hoondex-dev) builds the binary in debug mode (fast, incremental)
and launches it, so it always reflects your latest source. Symlink it onto your
`PATH` once:

```shell
# from the repo root
ln -s "$(pwd)/hoondex-dev" ~/.local/bin/hoondex-dev
```

Then run `hoondex-dev` from anywhere — it forwards all args, e.g. `hoondex-dev "explain this codebase"`.

> Make sure `~/.local/bin` is on your `PATH` (add `export PATH="$HOME/.local/bin:$PATH"` to your shell profile if it isn't).

### Use `hoondex` globally (release build)

Build the optimized binary and point a `PATH` entry at it. Because it's a
symlink, re-running the build refreshes the global `hoondex` with no extra steps:

```shell
# from the repo root
cargo build --release --bin hoondex --manifest-path codex-rs/Cargo.toml
ln -s "$(pwd)/codex-rs/target/release/hoondex" ~/.local/bin/hoondex
```

Run `hoondex` from anywhere. To pick up later changes, just rebuild:

```shell
cargo build --release --bin hoondex --manifest-path codex-rs/Cargo.toml
```

Alternatively, install it straight into `~/.cargo/bin` (already on most `PATH`s):

```shell
cargo install --path codex-rs/cli --bin hoondex
```

With `cargo install` you must re-run the command after each change to refresh the
global binary.

This repository is licensed under the [Apache-2.0 License](LICENSE).
