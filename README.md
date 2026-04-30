# jj-pr

A CLI tool that syncs [Jujutsu (jj)](https://github.com/jj-vcs/jj) bookmarks with GitHub Pull Requests.

**Core philosophy:**
- **jj owns structure** — the DAG of changes
- **GitHub owns presentation** — PR titles, descriptions, review state
- **No hidden state** — everything derived at runtime from `jj`, `gh`, and commit trailers
- **Graph structure is the source of truth** — PR membership derived from bookmark positions, not metadata

## Installation

```sh
cargo install --path .
```

Requires `jj` and `gh` (GitHub CLI) on your PATH.

## Workflow

### View the PR DAG

```sh
jj-pr          # or: jj-pr show
```

```
○  PR #12  Draft  mingwei/delete-subgraph-id
│  cleanup: remove SubgraphId, simplify StateLifespan
├─╮
│ ○  PR #10  Draft  mingwei/delete-metrics-slotvec
│ │  refactor: use slotmap SecondaryMap for metrics
○ │  PR #11  Draft  mingwei/delete-schedule-subgraph-args
├─╯  refactor: remove SubgraphId argument
○    PR #1   Draft  mingwei/delete-state-api-usage
│    refactor: operators capture local state
◆  trunk()
```

PR numbers are clickable hyperlinks in supported terminals. A `*` after the PR number indicates pending sync actions.

### View individual commits

```sh
jj-pr log          # commits associated with PRs
jj-pr log --all    # all commits including unassociated ones
```

### Sync with GitHub

```sh
jj-pr sync              # stamp, rebase, push, update bases
jj-pr sync --dry-run    # preview only
```

Handles the full lifecycle:
1. Stamps missing `PR: #N` trailers on commits
2. Rebases children of merged PRs onto `trunk()`
3. Abandons merged PR commits
4. Pushes affected bookmarks
5. Updates GitHub base branches to match the DAG

Blocks if any bookmarks have conflicts (resolve with `jj bookmark` first).

### Create a new PR

```sh
jj bookmark create my-feature    # first create the bookmark in jj
jj-pr create my-feature          # then create a draft PR for it
jj-pr create my-feature -t "title"  # with custom title
```

The base branch is auto-detected by walking the parent graph. New PRs are always created as drafts.

## How It Works

### PR membership

Computed from the graph, not from trailers:

```
owned(bookmark) = ancestors(bookmark) & ~ancestors(other_bookmarks | trunk())
```

Each commit belongs to the PR whose bookmark is its nearest descendant. Trailers (`PR: #N`) are written as a safety net and used for recovery when merged PR branches are deleted.

### Ambiguous commits

Commits shared between multiple PRs (e.g., diamond shapes) or with mismatched trailers are shown as ambiguous nodes with context-sensitive hints:

```
⚠  ambiguous shared between PR #1, PR #2
│  (restructure PRs to resolve — stack one on the other)
```

### Sync indicator

Nodes marked with `*` have pending sync actions. This propagates transitively — if a parent needs rebase, all descendants are marked too.

## Development

### Project structure

```
src/
├── main.rs              # CLI entry point
├── cli.rs               # clap argument definitions
├── jj.rs                # jj CLI interaction, trailer parsing
├── gh.rs                # GitHub CLI interaction, PrNum newtype
├── pr_dag.rs            # Core: RepoState, build, rendering, sync, create
├── graph_algorithms.rs  # Generic topo sort and DFS
├── style.rs             # Terminal styling (ANSI colors, OSC 8 hyperlinks)
├── ui.rs                # Confirmation prompts
└── tests.rs             # Snapshot tests (insta)
```

### Key types

- **`PrNum`** — newtype around `NonZeroU64` for PR numbers
- **`NodeKey`** — slotmap key for nodes in the DAG
- **`Node`** — `Root`, `TrunkTip`, `Pr(PrNum)`, or `Ambiguous`
- **`RepoState`** — immutable world view built from jj + GitHub state
- **`SyncAction`** — planned mutation for sync

### Dependencies

- `slotmap` — arena-allocated node DAG
- `sapling-renderdag` — graph rendering (same as jj)
- `clap` — CLI
- `serde` / `serde_json` — JSON parsing
- `anstyle` / `anstream` — terminal colors

### Running tests

```sh
cargo test
```

### Design document

See [DESIGN.md](DESIGN.md) for the full design rationale.

## Known Limitations

- **Remote hardcoded to `origin`** — fork-based workflows where PRs come from a different remote (e.g., `fork`) are not yet supported.
- **`gh pr list --limit 200`** — may miss PRs in repos with many PRs.
