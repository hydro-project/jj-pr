# jj-pr

A CLI tool that syncs [Jujutsu (jj)](https://github.com/jj-vcs/jj) bookmarks with GitHub Pull Requests.

**Core philosophy:**
- **jj owns structure** — the DAG of changes
- **GitHub owns presentation** — PR titles, descriptions, review state
- **jj-pr synchronizes the two** with no hidden state

PR membership is tracked via `PR: #N` trailers in jj change descriptions. The PR DAG is derived from the jj graph, not stored separately.

## Installation

```sh
cargo install --path jj-pr
```

Requires `jj` and `gh` (GitHub CLI) on your PATH.

## Workflow

### 1. Import existing PRs

If you already have open PRs on GitHub with matching local bookmarks:

```sh
jj-pr import --dry-run   # preview what would be stamped
jj-pr import              # stamp PR trailers on all matching commits
```

This walks each PR's bookmark back to trunk and stamps `PR: #N` on every commit. PRs are processed in topological order (parents first) so boundaries are correct.

### 2. View the PR DAG

```sh
jj-pr log
```

Renders a graph of your PRs with status, bookmark names, and titles:

```
○  PR #2812  (draft)  mingwei/delete-subgraph-id
│  cleanup: remove SubgraphId, SubgraphTag, simplify StateLifespan
├─╮
│ ○  PR #2810  (draft)  mingwei/delete-metrics-slotvec
│ │  refactor(dfir_rs): use slotmap SecondaryMap for metrics
○ │  PR #2811  (draft)  mingwei/delete-schedule-subgraph-args
├─╯  refactor: remove SubgraphId argument from schedule_subgraph
○    PR #2801  (draft)  mingwei/delete-state-api-usage
│    refactor: operators capture local state instead of using the state API
◆  trunk
```

PR numbers are clickable hyperlinks (OSC 8) in supported terminals.

### 3. Create a new PR

```sh
# From the current working copy, with an existing bookmark:
jj-pr track -b my-feature

# With a specific revision:
jj-pr track -b my-feature -r <rev>

# With a custom title:
jj-pr track -b my-feature -t "feat: add widget support"
```

New PRs are always created as drafts. The base branch is automatically set to the parent PR's bookmark (or `main` if no parent PR).

### 4. Update an existing PR

After adding commits to a PR's bookmark:

```sh
# Move PR to current working copy (@):
jj-pr track --pr 2814

# Move PR to a specific revision:
jj-pr track --pr 2814 -r <rev>
```

This moves the bookmark, pushes, and re-stamps `PR: #N` trailers on the new commit range.

### 5. Sync with GitHub

```sh
jj-pr sync --dry-run   # preview
jj-pr sync              # push bookmarks, update base branches
```

Pushes any bookmarks that differ from the remote and updates GitHub base branches to match the local DAG structure.

## How it works

### PR identification

Each commit in a PR has a trailer in its description:

```
feat: add widget support

Co-authored-by: Alice <alice@example.com>
PR: #1234
```

This provides stable identity that survives rebases, with no external state file.

### DAG structure

The PR DAG is derived from the jj commit graph:
- Walk from each PR bookmark's tip
- Commits with matching `PR: #N` trailers belong to that PR
- The first ancestor commits from a *different* PR (or trunk) are the parent PRs

### Draft/ready status

Shown in `jj-pr log` based on GitHub's current state. New PRs created via `track` are always drafts.

## Development

### Project structure

```
jj-pr/
├── Cargo.toml
└── src/
    ├── main.rs       # CLI entry point, command dispatch
    ├── cli.rs        # clap argument definitions
    ├── jj.rs         # jj CLI interaction: state loading, trailer parsing, mutations
    ├── gh.rs         # GitHub CLI interaction: PR listing, creation, editing
    ├── pr_dag.rs     # Core logic: DAG building, graph rendering, sync/import planning
    ├── style.rs      # Terminal styling: ANSI colors, OSC 8 hyperlinks
    └── tests.rs      # Integration tests for DAG building, sync, and import
```

### Key dependencies

- **sapling-renderdag** — graph rendering (same renderer jj uses)
- **clap** — CLI argument parsing
- **serde/serde_json** — parse jj and gh CLI JSON output
- **anstyle/anstream** — terminal color support (from clap's dep tree)

### Reading jj state

Single `jj log` call with a composite JSONL template:

```
jj log --no-graph -r 'trunk().. | trunk()' -T '
  "{\"commit\": " ++ json(self)
  ++ ", \"local_bookmarks\": " ++ json(local_bookmarks)
  ++ ", \"remote_bookmarks\": " ++ json(remote_bookmarks)
  ++ ", \"immutable\": " ++ json(self.immutable())
  ++ "}\n"
'
```

All mutations go through jj/gh CLI commands — no library dependencies on either.

### Running tests

```sh
cargo test -p jj-pr
```

22 tests covering trailer parsing, DAG building, sync planning, and import planning.

### Design document

See [DESIGN.md](DESIGN.md) for the full design rationale.
