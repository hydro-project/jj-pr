# JJ-PR Design (v3)

## Overview

jj-pr maps a Jujutsu (jj) change graph onto GitHub Pull Requests.

Core philosophy:
- **jj owns structure** (DAG of changes)
- **GitHub owns presentation** (PR titles, descriptions, review state)
- **No hidden state** — everything is derived at runtime from `jj`, `gh`, and commit trailers
- **Robust to invalid states** — ambiguities and inconsistencies are surfaced, never crash

## Data Model

### Inputs

Three readonly data sources, loaded once per invocation:

1. **`jj_entries: Vec<JjLogEntry>`** — raw commit data from `jj log` over `trunk().. | trunk()`, including commit/change IDs, parents, descriptions, local/remote bookmarks, and trunk membership.
2. **`prs: BTreeMap<PrNum, GhPr>`** — GitHub PR metadata from `gh pr list --state all`, including number, head/base ref names, state (Open/Closed/Merged), draft status, URL, and title.
3. **`default_branch: String`** — the GitHub repo's default branch name from `gh repo view`, used as the base for top-level PRs.

### RepoState

The unified derived view, built once from the three inputs. Immutable after construction.

```rust
pub struct RepoState {
    pub nodes: SlotMap<NodeKey, Node>,
    pub root_node: NodeKey,
    pub node_preds: SecondaryMap<NodeKey, Vec<NodeKey>>,
    pub node_succs: SecondaryMap<NodeKey, Vec<NodeKey>>,
    pub topo_order: Vec<NodeKey>,
    pub commit_node: HashMap<CommitId, NodeKey>,
    pub needs_push: HashSet<PrNum>,
    pub needs_sync: SparseSecondaryMap<NodeKey, ()>,
    pub bookmarks_conflicted: HashSet<String>,
}
```

### Node

Each node represents a contiguous group of commits in the DAG:

```rust
pub enum Node {
    Root,                    // synthetic boundary below trunk
    TrunkTip,                // tip of trunk()
    Pr(PrNum),               // a single PR
    Ambiguous {              // commits with unclear ownership
        branch_prs: BTreeSet<PrNum>,
        trailer_prs: BTreeSet<PrNum>,
    },
}
```

## PR Membership

### Algorithm

PR membership is computed from bookmark positions and the jj DAG in a single reverse-topological pass (children → parents):

1. Collect PR bookmark tip commits (local bookmark name matches a GitHub PR's `headRefName`).
2. For each commit, the `decide()` function determines its node based on:
   - Whether it's the trunk tip
   - Whether it's the tip of one or more PR bookmarks
   - What node(s) its children belong to (inherited downward)
   - Its `PR: #N` trailer, if any
3. Commits with a single clear owner join that PR's node.
4. Commits with multiple possible owners become `Ambiguous` nodes.
5. Commits with a trailer for a PR that has no local bookmark (e.g., merged and branch deleted) use the trailer as the primary source.

### Node DAG

A second pass computes edges between nodes. If a commit's parent belongs to a different node, an edge is added. Parents not in any node map to `root_node`.

A third pass topologically sorts the node DAG (panics on cycle — would be a bug) and computes `needs_sync` via forward propagation.

### Examples

**Simple stack:**
```
trunk <- a <- b (feat-b, PR #2) <- c (feat-c, PR #1)
```
- PR #1 owns `{c}`, PR #2 owns `{a, b}`.

**Diamond (ambiguous):**
```
trunk <- xyz <- a1 (feat-a, PR #1)
            <- b1 (feat-b, PR #2)
```
- PR #1 owns `{a1}`, PR #2 owns `{b1}`, `xyz` is ambiguous.

## Trailers

Trailers (`PR: #N`) in commit descriptions serve two roles:

1. **Validation** — when bookmarks exist, trailers are written by the tool to match graph membership. Mismatches produce `Ambiguous` nodes.
2. **Recovery** — when a PR is merged and its branch deleted, the trailer is the primary source for identifying the merged PR's commits.

## needs_sync

Computed during `build()` via a single forward pass over the topo order. A node is in `needs_sync` if:

- It's a non-Merged PR that needs push (local ≠ remote bookmark target)
- It has a merged parent (needs rebase)
- Its GitHub base branch doesn't match the expected base from the DAG
- Any of its ancestor nodes is in `needs_sync` (transitive propagation)

Merged PR nodes are never in `needs_sync` — they are the trigger for sync actions on their descendants, not targets themselves.

Nodes in `needs_sync` display a `*` indicator in both `show` and `log` views.

## Commands

### `jj-pr` / `jj-pr show`

Renders the node DAG as a graph. Shows PR number, state (Draft/Ready/Closed/Merged), bookmark name, title, and `*` sync indicator. Ambiguous nodes show context-sensitive hints:
- Shared commits → "restructure PRs"
- Wrong trailer → "edit commit description"
- Both → mentions both fixes

### `jj-pr log`

Renders individual jj commits with their PR associations. Shows change ID, commit ID, bookmarks, PR info, and `*` sync indicator. `--all` includes commits not associated with any PR.

### `jj-pr sync`

Reconciles local + remote state. Actions in order:

1. **Stamp missing trailers** — for commits owned by a PR node whose description lacks the correct `PR: #N` trailer. Uses `change_id` (stable across rewrites) for `jj describe`.
2. **Rebase children of merged PRs** — `jj rebase -s <bookmark>+ -d trunk()` for each merged PR with children.
3. **Abandon merged PR commits** — `jj abandon <bookmark>`.
4. **Push** — single `jj git push --bookmark ...` for all affected bookmarks (Open and Closed, not Merged).
5. **Update GitHub base branches** — `gh pr edit --base` for PRs whose base doesn't match the DAG.

Blocks if `bookmarks_conflicted` is non-empty. Supports `--dry-run` and `[Y/n]` confirmation (skippable with `-y`).

### `jj-pr create <bookmark>`

Creates a new draft PR for an existing bookmark:

1. Verify bookmark exists and has no existing PR.
2. Walk parent graph to determine base branch (nearest ancestor PR's bookmark, or `default_branch`).
3. Track remote bookmark (`jj bookmark track`, hardcoded to `origin`).
4. Push (`jj git push --bookmark`).
5. Create draft PR (`gh pr create --draft`).
6. Stamp `PR: #N` trailers on owned commits.

Title defaults to the first line of the tip commit's description. Override with `-t`.

## Project Structure

```
src/
├── main.rs              # CLI entry, loads inputs, dispatches commands
├── cli.rs               # clap argument definitions
├── jj.rs                # jj CLI interaction, trailer parsing
├── gh.rs                # GitHub CLI interaction, PrNum newtype
├── pr_dag.rs            # Core: RepoState, build, rendering, sync, create
├── graph_algorithms.rs  # Generic topo sort and DFS
├── style.rs             # Terminal styling (ANSI colors, OSC 8 hyperlinks)
├── ui.rs                # Confirmation prompts
└── tests.rs             # Snapshot tests (insta)
```

## Key Design Decisions

- **SlotMap-based DAG** — nodes are arena-allocated with `SlotMap<NodeKey, Node>`, edges in `SecondaryMap`. This avoids lifetime issues and allows O(1) node lookup.
- **No hidden state files** — everything derived from `jj` + `gh` + trailers at runtime. No `.jj-pr/` directory or config.
- **Ambiguous instead of error** — invalid states (shared commits, wrong trailers) produce `Ambiguous` nodes with user-facing hints rather than hard errors.
- **Merged PRs excluded from needs_sync** — they trigger actions on descendants but are not sync targets themselves. Enforced by `assert!` in plan_sync.
- **Closed treated as Open** — closed PRs are pushed and have bases updated, since the user may re-open them.
- **`default_branch` from GitHub** — uses `gh repo view` to get the actual default branch name instead of hardcoding `main`.

## Known Limitations

- **Remote hardcoded to `origin`** — `bookmark_track` and `needs_push` assume the PR remote is `origin`. Fork-based workflows (e.g., `origin` = upstream, `fork` = user's fork) will not work correctly. Fix requires detecting which remote `gh` uses for PRs.
- **Multiple PRs per bookmark** — if multiple PRs (e.g., Open + Closed/Merged) share the same `headRefName`, one is silently dropped during `build()`. Needs priority-based resolution.
- **`gh pr list --limit 200`** — may miss PRs in repos with many PRs.
