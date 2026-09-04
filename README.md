# delegate

A tiered task dispatcher. You write a packet (a YAML brief: goal, allowed paths, a verify
command); `delegate` runs it through a ladder of model tiers, cheapest first, and hands you
back a patch that already passed its own verifier.

## Why

- **The human writes packets, models never pick tiers.** A packet says what to do and how to
  check it; it never says which model should do it. Tier selection is config plus a health
  check, not a judgment call made by an LLM.
- **Verifiers decide, not vibes.** If a packet has a `verify` command, pass/fail is that
  command's exit code. No verifier means no objective signal — say so explicitly
  (`verified: false`) rather than pretend there is one.
- **Escalation is deterministic.** Given the same config and the same plan, the sequence of
  tiers tried is fully determined by index arithmetic (start, ceiling, attempts) — there's no
  model-in-the-loop step that decides whether to escalate. What *is* live is which chain entry
  within a tier answers its health check.
- **The log is what lets you swap a model without guessing.** Every attempt — model, verify
  exit code, tokens, duration, the patch itself — lands in SQLite. Changing which model backs
  a tier is a config edit; whether that was a good idea is a `delegate stats` query against
  history, not a feeling.

## Install

```sh
cargo install --path .
delegate config init      # writes ~/.config/delegate/config.yml and host.yml
delegate tiers             # resolved chains on this host, with live health
```

`config init` never overwrites an existing file. `tiers` is the fastest way to confirm a chain
entry's `health` URL actually resolves from this machine before you run anything against it.

## 60 seconds

```sh
delegate new \
  --class rust-mech \
  --goal "Add a total_secs() helper on the Segments type in src/timing.rs that sums segment durations" \
  --path src/timing.rs \
  --verify "cargo build && cargo test"
```

```
.delegate/packets/01J8X4R8N0V3Z9Q2K7T6M1P5W2.yml
```

```sh
delegate run .delegate/packets/01J8X4R8N0V3Z9Q2K7T6M1P5W2.yml -y
```

```
run 01J8X4RM7Y2H5T8K1N9Q3V6W0Z
packet 01J8X4R8N0V3Z9Q2K7T6M1P5W2 · class rust-mech · t1→t2 · mode normal
t1 = llama-swap/qwen38-nvfp4
t1 attempt 1 running
t1   read_file src/timing.rs
t1   write_file src/timing.rs
t1 ✓ attempt 1 1 file(s) (14.2s)
applied 1 file(s), 812 bytes
passed at t1 · 0 escalation(s) · 14.4s · 1 file(s): added total_secs() and a unit test
```

`src/timing.rs` now has the change, unstaged, in your real working tree — review it and commit
it yourself. `delegate show 01J8X4RM7Y2H5T8K1N9Q3V6W0Z` replays the same summary later;
`delegate log` lists recent runs.

If `t1` had failed (verify exit nonzero, timeout, or a file touched outside `--path`), delegate
would retry it up to the class's `attempts`, then escalate to `t2` with the previous failure's
verifier output folded into the next prompt — nothing above `t1` is charged unless `t1`
genuinely can't do it.

## Packet reference

A packet is one delegated unit of work. `delegate schema` prints the full JSON Schema; this is
the human version.

| field | type | required | meaning |
|---|---|---|---|
| `id` | string | yes | ULID, assigned by `delegate new`. |
| `class` | string | yes | Selects defaults from `classes.<class>` in config (falls back to `classes.default`). |
| `goal` | string | yes | What the worker must achieve, written for a reader with no other context — this is the brief. |
| `paths` | list of globs | no | Files the worker may create/modify. Empty means unrestricted; checked after the run by diffing the worktree, not by sandboxing the worker. |
| `verify` | shell command | no | Runs in the isolated worktree after the worker exits; exit 0 passes. Overrides the class's `verify`. |
| `read` | list of paths | no | Listed in the worker's prompt as files to read first; delegate does not read them itself. |
| `notes` | string | no | Free-form text appended to the prompt as context. |
| `tier` | string | no | Overrides the class's start tier. |
| `ceiling` | string | no | Overrides the class's highest tier. |
| `effort` | `low`\|`medium`\|`high` | no | Overrides the chain entry's `thinking` level for this run. |
| `timeout` | seconds | no | Budget for the worker, and separately for the verify command. Falls back to the class, then 900. |
| `attempts` | integer ≥ 1 | no | Attempts at a tier before escalating. Falls back to the class, then 2. |
| `mode` | `normal`\|`conserve`\|`rush` | no | Dispatch mode override; see [Tiers and modes](#tiers-and-modes). |
| `repo` | path | no | Repository the packet applies to; defaults to the cwd when run. Must be a git repo with at least one commit. |
| `created` | RFC 3339 timestamp | no | Set once by `delegate new`. |

Unknown fields are rejected. `goal`, `class` and `id` must be non-blank; `attempts: 0` and
`timeout: 0` are rejected explicitly.

## Config reference

Layering, in order (later wins, deep-merged — see the gotcha below):
`~/.config/delegate/config.yml` (required) → `~/.config/delegate/host.yml` (optional) →
each path in `DELEGATE_CONFIG` (colon-separated, each required) → each repeated `--config FILE`
(each required).

Every key, one line each:

**Top level**
- `mode` — `normal`\|`conserve`\|`rush`, default `normal`. Fallback when neither the packet nor the caller specifies one.
- `order` — tier names cheapest→strongest. Required, at least one; defines the ladder.
- `tiers` — map of tier name → `{label, chain}`. Required; must have exactly the same keys as `order`.
- `classes` — map of class name → class policy, default `{}`. A `default` entry backstops any class without its own.
- `modes` — `{conserve, rush}` policies, default shift `-1`/`+1` with no cap or approval gate.
- `runners.omp`, `runners.claude` — defaults for the omp and Claude Code runners (below).
- `server` — HTTP daemon settings (below).
- `packets_dir` — string, default `.delegate/packets`. Where `delegate new` writes packets, relative to the repo root.
- `data_dir` — path, default none (falls back to `$XDG_DATA_HOME/delegate` or `~/.local/share/delegate`). Holds `delegate.db`.
- `health_timeout_ms` — integer, default `3000`. Timeout for each chain entry's health probe.
- `scope_ignore` — filename globs, default the common lockfiles (`Cargo.lock`, `package-lock.json`, `bun.lock`, `bun.lockb`, `yarn.lock`, `pnpm-lock.yaml`, `Package.resolved`, `poetry.lock`, `uv.lock`, `Gemfile.lock`, `go.sum`). Exempt from the `paths` check at any depth; still shipped in the patch.

**`tiers.<name>`**
- `label` — string, default `""`. Cosmetic; shown by `tiers`/`log`.
- `chain` — chain entries, required, non-empty. Tried in order; the first with no `health` or a 2xx `health` is used for the whole tier.

**`tiers.<name>.chain[]`**
- `runner` — `omp`\|`claude`\|`command`, default `omp`. `omp` runs oh-my-pi headless, `claude` runs Claude Code headless (`claude -p --output-format stream-json --dangerously-skip-permissions`, on whatever login Claude Code has), `command` runs a shell command with the prompt on stdin.
- `model` — string, required for `omp`, optional for `claude`. Passed as `--model`.
- `thinking` — string, default none. Passed as `--thinking` (omp) or `--effort` (claude); a packet's `effort` overrides it.
- `health` — URL, default none. GET must answer 2xx for this entry to be eligible; entries without `health` are always eligible.
- `settings` — arbitrary YAML, default none. Written to a temp file and passed to omp as `--config` (an overlay on omp's own config).
- `command` — shell string, required for `command`. Runs via `sh -c`; the prompt arrives on stdin.
- `env` — map, default `{}`. Merged over the plan's environment for this entry.
- `args` — list, default `[]`. Extra CLI args appended after delegate's own.

**`classes.<name>`**
- `tier` — string, default none (falls back to the first tier in `order`).
- `ceiling` — string, default none (falls back to the last tier).
- `verify` — shell string, default none. Used only when the packet itself sets no `verify`.
- `attempts` — integer, default none (falls back to `2`).
- `timeout` — seconds, default none (falls back to `900`).
- `verified` — bool, default none (falls back to `verify.is_some()`). Marks the class as having an objective verifier even when a given packet has none; only classes that resolve `verified: true` are affected by `ceiling_verified`.
- `env` — map, default `{}`. Merged into the worker's and verifier's environment for this class.
- `scope_ignore` — globs, default `[]`. Appended to the top-level `scope_ignore` for this class only.

**`modes.conserve` / `modes.rush`**
- `shift` — integer rungs, default `-1` (conserve) / `+1` (rush). Added to the resolved start tier, then clamped to `[0, ceiling]`.
- `ceiling_verified` — tier name, default none. Caps (never raises) the ceiling when the class is verified.
- `ask_before` — tier name, default none. If that tier is strictly above the shifted start and at or below the ceiling, delegate pauses there for approval.

**`runners.omp`**
- `bin` — string, default `omp`. A path containing `/` is used literally; otherwise it's resolved on the same search path delegate uses for every child process (`$PATH` plus `~/.bun/bin`, `~/.cargo/bin`, `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`, `/bin`).
- `args` — list, default `[]`. Extra args appended to every omp invocation, after the chain entry's own.
- `no_lsp` — bool, default `true`. Passes `--no-lsp`.
- `no_extensions` — bool, default `false`. Passes `--no-extensions`.
- `no_skills` — bool, default `false`. Passes `--no-skills`.
- `no_rules` — bool, default `false`. Passes `--no-rules`.

**`runners.claude`**
- `bin` — string, default `claude`, resolved like `runners.omp.bin`.
- `args` — list, default `[--strict-mcp-config]` so workers start without MCP servers. Extra args are appended to every Claude Code invocation, after the chain entry's own.

**`server`**
- `listen` — string, default `0.0.0.0:4100`. Overridable with `serve --listen`.
- `user` — string, default `delegate`. HTTP Basic auth username.
- `password_env` — string, default `DELEGATE_PASSWORD`. Environment variable read for the Basic auth password.
- `env_file` — path or none, default `~/.config/delegate/serve.env`. `KEY=VALUE` fallback read when the env var is unset; `install-service` creates it with a random password if missing. Every other `KEY=VALUE` in that file (provider API keys, for example) is exported to workers and verifiers, so the daemon can reach cloud tiers without a login shell.

**Deep-merge gotcha:** merging is key-wise on mappings only. A sequence — most importantly a
tier's `chain` — is *replaced wholesale* by whichever layer last mentions it, never
concatenated. A host overlay that wants to add a fallback entry to `t1` must repeat `t1`'s
entire chain, not just the new entry (see `examples/host.mac.yml`).

## Tiers and modes

`Config::plan` resolves a packet + overrides into a `Plan` before anything runs:

- **start** = CLI/API override → packet `tier` → class `tier` → first tier in `order`.
- **ceiling** = CLI/API override → packet `ceiling` → class `ceiling` → last tier in `order`.
  Start above ceiling is a hard error; an unknown tier name anywhere is a hard error.
- **verify** = packet `verify` → class `verify`.
- **verified** = class `verified` if set, else `verify.is_some()`.
- **mode** = CLI/API override → packet `mode` → config `mode`.
- if mode is `conserve` or `rush`: when the class is verified, `ceiling_verified` (if set)
  caps the ceiling first; then `shift` is added to start and the result is clamped to
  `[0, ceiling]`; then, if `ask_before` names a tier strictly above the shifted start and at or
  below the (possibly capped) ceiling, that tier is flagged for approval.
- **attempts** = CLI/API override → packet → class → `2`.
- **timeout** = packet → class → `900` (no CLI/API override level).

At run time, tiers are walked from start to ceiling in order. For each tier, the first chain
entry with no `health` or a passing `health` check is used; if none pass, the tier is skipped
(`tier_skipped`) and the ladder moves up. A skipped or exhausted tier both count as
"escalation" — the event stream reports one `escalated` per tier boundary crossed, with the
previous failure's verifier tail feeding the next attempt's prompt. A tier flagged by
`ask_before` fires `approval_required` and blocks until approved: the CLI prompts on a TTY
(`-y`/`--yes` skips this; a non-interactive run with no `-y` holds), the HTTP API waits for
`POST /v1/runs/{id}/approve`.

Final run status is one of `passed`, `failed` (ladder exhausted), `held` (approval declined),
`cancelled`, or `error` (something outside the worker/verify loop broke, e.g. bad config).

## CLI reference

Global: `--config FILE` (repeatable, deep-merged last, applies to every subcommand).

- `delegate new --class C --goal G [--path P]... [--verify CMD] [--read F]... [--notes N] [--tier T] [--ceiling T] [--effort low|medium|high] [--timeout S] [--attempts N] [--mode M] [--repo DIR] [-o FILE] [--edit] [--run] [--json] [-y] [--keep-worktree]` — writes a packet, prints its path (unless `--json`); `--edit` opens `$VISUAL`/`$EDITOR`/`vi` first; `--run` runs it immediately with the same dispatch flags.
- `delegate run PACKET.yml [--tier T] [--ceiling T] [--mode M] [--attempts N] [--json] [-y] [--keep-worktree]` — runs the ladder. Human output by default, one JSON envelope per line with `--json` (`run_id`, `seq`, `ts`, `kind`, plus per-kind fields — kinds: `run_started`, `tier_selected`, `tier_skipped`, `attempt_started`, `progress`, `attempt_finished`, `approval_required`, `approval_resolved`, `escalated`, `applied`, `run_finished`). `--keep-worktree` leaves the temp worktree on disk for inspection.
- `delegate replay RUN_ID [--tier T] [--ceiling T] [--mode M] [--attempts N] ...` — re-runs a stored run's packet (same dispatch flags as `run`), typically on a different tier.
- `delegate log [--limit N] [--json]` — recent runs, newest first.
- `delegate show RUN_ID [--json]` — one run plus its attempts (accepts a unique ID prefix).
- `delegate stats [--class C] [--json]` — attempts, passes, pass rate, average duration and tokens, grouped by class and tier.
- `delegate tiers [--json]` — resolved chains for this host, each entry's health probed live.
- `delegate schema` — the packet JSON Schema.
- `delegate config init` — writes `config.yml`/`host.yml` if missing.
- `delegate config check` — loads and validates the merged config.
- `delegate config path` — lists the layer paths and whether each exists.
- `delegate serve [--listen ADDR]` — runs the HTTP daemon in the foreground.
- `delegate install-service` — writes and starts a systemd user unit (Linux) or launchd agent (macOS); creates `server.env_file` with a random password if it doesn't exist.

Exit codes: `0` passed, `1` failed/held/cancelled, `2` error (including config/packet errors
before a run starts).

## HTTP API reference

`delegate serve` binds `server.listen` (default `0.0.0.0:4100`). Every route requires HTTP
Basic auth (`server.user` / the resolved password) except `GET /health`.

| method | path | body | notes |
|---|---|---|---|
| GET | `/health` | — | No auth. `{ok, version}`. |
| GET | `/v1/capabilities` | — | Version, hostname, tier/class/mode names. |
| GET | `/v1/tiers` | — | Same shape as `delegate tiers --json`, health probed live. |
| GET | `/v1/runs?limit=` | — | Recent runs, default 50, max 500. |
| POST | `/v1/runs` | `{packet, tier?, ceiling?, mode?, attempts?}` | 202 `{run_id}`; runs in the background. |
| GET | `/v1/runs/{id}` | — | `{run, attempts, live}`; 404 if unknown. |
| GET | `/v1/runs/{id}/events?after=` | — | SSE, event name `run`. Replays stored events with `seq > after`, then streams live ones if the run is still active. |
| POST | `/v1/runs/{id}/approve` | `{approved}` | 204; 404 if the run isn't currently live. |
| POST | `/v1/runs/{id}/cancel` | — | 204; 404 if the run isn't currently live. Also resolves any pending approval as declined. |
| POST | `/v1/runs/{id}/replay` | `{tier?, ceiling?, mode?, attempts?}` | 202 `{run_id}`; re-runs that run's stored packet. |
| GET | `/v1/stats?class=` | — | Same shape as `delegate stats --json`. |

## How the golden set works

Keep a small set of representative packets — one or two per class, each with a real `verify`
— committed under `.delegate/packets/` in the target repo. That's the golden set: it's just
packets in git, nothing delegate-specific to set up.

To qualify a new or cheaper model before trusting it with a tier: point that tier's chain at
the candidate model, then run each golden packet against it directly —
`delegate run .delegate/packets/<id>.yml --tier <candidate>` for a fresh attempt, or
`delegate replay <run-id> --tier <candidate>` to force a previously-run packet through the
candidate tier without touching the class's default routing yet.

Check `delegate stats --class <c>` afterwards: pass rate, average duration and tokens per tier
for that class. When the candidate's numbers hold up across the golden set, promote it by
editing `classes.<c>.tier` (or `ceiling`) in config — a one-line, reviewable change, made from
evidence in the log rather than a hunch.

## Determinism

Routing and verification are deterministic: given the same packet, config, and stored plan,
the sequence of tiers tried, which chain entry within a tier is eligible (modulo live health),
and pass/fail (a shell exit code) never depend on model judgment. The escalation ladder itself
has no LLM in the loop.

Worker *output* is a different story. Cloud model output is not reproducible run to run even
at temperature 0 — provider-side batching and routing introduce noise outside your control. A
local llama.cpp-backed model is reproducible only when served single-slot (no concurrent
requests interleaving batches) with a fixed seed and `temperature: 0`; multi-slot serving
reintroduces batch-dependent floating-point reduction order. This is exactly why the SQLite
log matters more than reproducibility: you don't compare a candidate model's output
byte-for-byte against a baseline, you compare its pass rate across the golden set via
`delegate stats`.

## Chain failover

Each tier is a chain of entries. Entries with a `health` URL are skipped when it does not answer. An entry without one is
tried, and if the worker never gets going (bad credentials, no credits, unknown model, runner error: non-zero exit with no
output tokens and no changed files) the run moves to the next entry in the same tier without consuming an attempt. Such
attempts are stored with status `error` and excluded from `delegate stats`. Only a real failure (verifier or scope) counts
against `attempts` and escalates to the next tier. A packet with no `verify` passes only when the worker exits 0 and
changed at least one file; a clean exit with no changes is a failure, so a model that just talks cannot pass.

## Integrations

`delegate` itself is just the CLI and daemon above — it has no editor or agent integration
built in. Three thin wrappers around it live in the `claudeconfig` repo instead, one per
harness, so an assistant can write and dispatch packets without shelling out by hand: a
Claude Code skill (`skills/delegate/SKILL.md`), an opencode tool plus slash command
(`opencode/tools/delegate.ts`, `opencode/command/delegate.md`), and an omp extension
(`omp/extensions/delegate.ts`, a `/delegate` command plus an opt-in `delegate` tool behind `--delegate-tool`). All three do the same thing: expose `new`/`run`/`log`/`stats`/`tiers` as callable
actions and report delegate's own output back verbatim, rather than re-implementing any of the
logic in this crate.

## License

GPL-3.0. See `LICENSE`.
