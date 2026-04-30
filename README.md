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

## Example Workflow

### 1. Fetch latest changes

```sh
jj git fetch
```

Note: `jj-pr` does not fetch for you — always fetch first to get the latest remote state.

### 2. View the PR DAG

```sh
jj-pr          # or: jj-pr show
```

```
○  PR #2804  Ready  mingwei/fix-inline-tasks
│  fix(dfir_rs): defer `_counter` task spawning via `Context::request_task` buffer
│ ○  PR #2746  Ready  mingwei/sqs-rebase
├─╯  feat(hydro_test): add example of AWS + SQS support
│ ◆  trunk()
├─╯
│ ○    PR #2812*  Draft  mingwei/delete-subgraph-id
│ ├─╮  refactor(dfir_rs): remove SubgraphId, simplify StateLifespan
│ │ ○  PR #2811*  Draft  mingwei/delete-schedule-subgraph-args
│ ○ │  PR #2810*  Draft  mingwei/delete-metrics-slotvec
│ ├─╯  refactor(dfir_rs): use slotmap SecondaryMap for metrics
│ ○  PR #2801*  Draft  mingwei/delete-state-api-usage
╭─┤  refactor(dfir_lang): operators capture local state
│ ○  PR #2798  Ready  mingwei/delete-intermediate-subgraph
├─╯  refactor(dfir_lang): remove intermediate subgraphs
◆  root()::
```

PR numbers are clickable hyperlinks in supported terminals. A `*` after the PR number means `sync` has pending actions for that PR — this propagates transitively, so if a parent needs rebase, all descendants are marked too.

If there are ambiguous commits (shared between multiple PRs), they show up as warnings with hints:

```
⚠  ambiguous* shared between PR #4, PR #5
│  (restructure PRs to resolve — stack one on the other)
```

### 3. Diagnose and fix issues

Use `jj-pr log` to see individual commits and their PR associations — this is mainly useful for diagnosing ambiguous commits:

```sh
jj-pr log          # commits associated with PRs
jj-pr log --all    # include commits not in any PR
```

Fix ambiguities by restructuring with `jj` (e.g., `jj rebase` to stack one PR on the other).

### 4. Sync with GitHub

```sh
jj-pr sync
```

This stamps missing trailers, rebases children of merged PRs onto `trunk()`, abandons merged commits, pushes affected bookmarks, and updates GitHub base branches. You'll be prompted before any changes are applied.

### 5. Create a new PR

```sh
jj bookmark create my-feature    # first create the bookmark in jj
jj-pr create my-feature          # then create a draft PR for it
```

The base branch is auto-detected by walking the parent graph. New PRs are always created as drafts. Use `-t "title"` to set a custom title (defaults to the first line of the tip commit's description).

## How It Works

PR membership is determined from the jj DAG structure: each commit belongs to the PR whose bookmark is its nearest descendant. Commits shared between multiple PRs become ambiguous nodes. Trailers (`PR: #N`) are stamped by the tool as a safety net and are used to identify commits from merged PRs whose branches have been deleted. See the `decide()` function in `pr_dag.rs` for the full membership algorithm.

See [DESIGN.md](DESIGN.md) for the full design rationale.

## Known Limitations

- **Remote hardcoded to `origin`** — fork-based workflows where PRs come from a different remote (e.g., `fork`) are not yet supported.
- **`gh pr list --limit 200`** — may miss PRs in repos with many PRs.
