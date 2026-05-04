# jj-pr

A CLI tool that syncs [Jujutsu (jj)](https://github.com/jj-vcs/jj) bookmarks with GitHub Pull Requests.

**Core philosophy:**
- **No hidden state** — everything is derived at runtime from `jj`, `gh`, and change description trailers like
  `PR: #1234`
- **jj owns structure** — the DAG of changes
- **GitHub owns presentation** — PR titles, descriptions, review state
- **Invalid states are first class** — ambiguities and inconsistencies are clearly displayed to you and don't halt your
  work — fix them when you feel like it

## Installation

```sh
cargo install --git https://github.com/hydro-project/jj-pr --locked
jj-pr util install-aliases
```
The second line installs `jj pr` as a subcommand alias and revset aliases (`pr(n)`, `pr_root(n)`) into your user config.


### Dependencies

Requires `jj` (of course) and `gh` (GitHub CLI) on your PATH.

To install `jj`:
```sh
cargo install jj-cli --bin jj --locked
```

[To install `gh`, see here](https://cli.github.com/).


## Example Workflow

### 1. Fetch latest changes

```sh
jj git fetch
```

Note: `jj-pr` does not fetch for you — fetch first to update the local repo to the latest remote state.

### 2. View the PR DAG

```sh
jj pr          # or: jj-pr show
```

```apl
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
~  (elided revisions)
```

PR numbers are clickable hyperlinks in supported terminals. A `*` after the PR number means there are pending actions
for that PR.

If there are ambiguous commits, they show up as warnings with hints.
```apl
○  PR #2798*  Ready  mingwei/delete-intermediate-subgraph
│  refactor(dfir_lang): double-buffer defer_tick handoffs, remove intermediate subgraphs [ci-bench]
│ ○  PR #2801*  Draft  mingwei/delete-state-api-usage
├─╯  refactor(dfir_lang): operators capture local state instead of using the state API [ci-bench]
⚠  ambiguous shared between PR #2798, PR #2801 (trailer: PR #2798)
│  (restructure PRs to resolve — stack one on the other)
◆  trunk()
```

### 3. Diagnose and fix issues

Use `jj pr log` to see individual commits and their PR associations.
```sh
jj pr log          # commits associated with PRs
jj pr log --all    # include commits not in any PR
```

This is mainly useful for diagnosing ambiguous commits.
```apl
○  lqxnymrprnpl a6f5cee01ecb PR #2801* Draft
│  refactor(dfir_lang): add write_tick_end to OperatorWriteOutput, convert fold to inline state
│ ○  wnkkzpnlkwzt ca43c533b6f6 mingwei/delete-intermediate-subgraph sandbox-d11a1381-9aa3-49f8-abf1-16585609c8a6 PR #2798* Ready
├─╯  Address PR #2798 review comments: extract helper methods, fix stale comments
⚠  prupozyltvpz cf49be1b8fc8 ambiguous [PR #2798, PR #2801]
│  refactor(dfir_lang): double-buffer defer_tick handoffs, remove intermediate subgraphs
⚠  xqxnxvxoyltz 7a71c32b196c ambiguous [PR #2798, PR #2801]
│  test: add test_mutual_defer_tick reproducing as_code topo sort cycle
◆  trunk()
```

Fix ambiguities by restructuring with `jj` (e.g., `jj rebase` to stack one PR on the other, or `jj describe` to edit
"PR: #1234" trailers).

### 4. Sync with GitHub

```sh
jj pr sync
```

This stamps missing trailers, rebases children of merged PRs onto `trunk()`, abandons merged commits, pushes affected
bookmarks, and updates GitHub base branches. You'll be prompted before any changes are applied.

```apl
  stamp #15 trailer on knynvqkoypsy
  stamp #14 trailer on olqtvyvtvzyq
  push: #15 (my-feature-a)
Apply 3 action(s)? [Y/n]
Stamping PR #15 on knynvqkoypsy
Stamping PR #14 on olqtvyvtvzyq
Pushing 1 bookmark(s): my-feature-a
```

### 5. Create a new PR

```sh
jj bookmark create my-feature-b  # first create the bookmark in jj
jj pr create my-feature-b        # then create a draft PR for it
```

The base branch is auto-detected by walking the parent graph.
```sh
Pushing my-feature-b
Creating PR: feat: my new features (my-feature-b → main) [draft]
Created PR #16
```

## How It Works

PR membership is determined from the jj DAG structure: each commit belongs to the PR whose bookmark is its nearest
descendant. Commits shared between multiple PRs become ambiguous nodes. Trailers (`PR: #1234`) are appended to change
descriptions as a safety net. See the `decide()` function in `pr_dag.rs` for the full membership algorithm.

See [DESIGN.md](DESIGN.md) for the full design rationale.

## Known Limitations

- **Remote hardcoded to `origin`** — fork-based workflows where PRs come from a different remote (e.g., `fork`) are not yet supported.
- **`gh pr list --limit 200`** — may miss PRs in repos with many PRs.
- **Multiple PRs sharing the same bookmark** — if you have e.g. an open and a closed/merged PR both using the same branch name, only one will be tracked. See the `head_to_pr` TODO in `pr_dag.rs`.
- Some other things, probably
