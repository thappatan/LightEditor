# Contributing

Workflow + conventions for this repository.

## Branching

Trunk-based:

- `main` is the integration branch. **Direct pushes are prohibited** (see [Branch protection](#branch-protection)). Every change lands via pull request.
- Feature branches are short-lived (hours-to-days, not weeks). Name them with a prefix:
  - `feat/<slug>` — new features
  - `fix/<slug>` — bug fixes
  - `chore/<slug>` — tooling, deps, refactors
  - `docs/<slug>` — docs/ADRs only
- Rebase onto `main` before opening a PR; squash-merge on land. The merge commit subject must follow [commit convention](#commit-convention).

## Commit convention

[Conventional Commits 1.0](https://www.conventionalcommits.org/) — strict.

```
<type>(<scope>)<!>: <subject>

[optional body]

[optional footer(s)]
```

**Types:**

| Type | When |
|---|---|
| `feat` | New user-facing capability |
| `fix` | Bug fix |
| `perf` | Performance improvement with measurable impact |
| `refactor` | Behavior-preserving code change |
| `docs` | Docs / ADR / spec only |
| `test` | Tests only (or test infrastructure) |
| `build` | Build system, Cargo profile, dependencies |
| `ci` | CI workflows |
| `chore` | Other maintenance |
| `revert` | Reverts a previous commit |

**Scopes** match crate names where applicable: `buffer`, `editor-core`, `syntax`, `lsp-client`, `dap-client`, `git`, `terminal`, `ui-render`, `ui-text`, `ui-scene`, `ui-widgets`, `ui-window`, `ai-providers`, `ai-completion`, `ai-chat`, `ai-agent`, `ai-rag`, `ai-mcp`, `config`, `workspace`, `theme`, `app`. Cross-cutting scopes: `adr`, `spec`, `deps`.

**Breaking changes** use `!` after type/scope and `BREAKING CHANGE:` in the footer:

```
feat(config)!: switch settings file to .editor/settings.toml

BREAKING CHANGE: existing settings.json files are no longer read; migration tool ships in v0.2.
```

**Subject line rules:**

- Imperative mood: "add", "fix", "remove" — not "added", "fixes"
- Lowercase first letter; no trailing period
- ≤72 chars
- No emoji, no AI co-author trailers, no "Generated with…" footer

## Pull requests

- One logical change per PR. Multi-purpose PRs are split before review.
- PR title follows commit convention — it becomes the squash-merge subject.
- Body uses the PR template (auto-loaded from `.github/pull_request_template.md`).
- All CI checks must pass: `cargo check`, `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo nextest run`.
- Link the milestone task file when applicable: `Refs: tasks/milestone-N-*.md`.

## Releases

Versioning: [SemVer 2.0](https://semver.org/). Pre-1.0 we relax minor/patch distinction.

Tag format: `v<major>.<minor>.<patch>` (e.g. `v0.1.0`). Tags trigger `.github/workflows/release.yml`.

Use [`cargo-release`](https://github.com/crate-ci/cargo-release):

```bash
cd editor
cargo release patch --execute   # bump, commit, tag, push
# or: cargo release minor --execute
# or: cargo release 0.2.0-rc.1 --execute
```

Config lives in `editor/release.toml`. The tool creates a release commit, tags `vX.Y.Z`, and pushes — CI takes over from there to build binaries and publish a GitHub Release.

## Branch protection

After the first push to GitHub, the repo owner must enable on `main` (Settings → Branches → Branch protection rules):

- ✅ Require a pull request before merging
- ✅ Require approvals (1 reviewer for team mode; can be left at 0 for solo dev)
- ✅ Require status checks to pass: `ci / check`, `ci / fmt`, `ci / clippy`, `ci / test`
- ✅ Require branches to be up to date before merging
- ✅ Require linear history
- ✅ Do not allow bypassing the above settings (applies to admins too)
- ❌ Allow force pushes (off)
- ❌ Allow deletions (off)

These rules cannot be set from the CLI without GitHub Pro on private repos. Configure them through the web UI immediately after the first push.

## Local development

```bash
# from repo root
cd editor
cargo check --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Performance benchmarks (added during M0):

```bash
cargo bench
cargo flamegraph --bin app   # requires sudo on macOS / dtrace privileges
```
