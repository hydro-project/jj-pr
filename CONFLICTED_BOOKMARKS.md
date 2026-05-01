# How jj Represents Conflicted Bookmarks

## Target Array Format

The `target` field in `json(local_bookmarks)` and `json(remote_bookmarks)` uses jj's conflict representation — an array of alternating adds and removes:

- Even indices (0, 2, 4, …) are **adds**
- Odd indices (1, 3, 5, …) are **removes**
- Always an **odd number** of elements (adds = removes + 1)
- `null` at any position means **"absent" / "deleted"**

Semantically: `add₁ - remove₁ + add₂ - remove₂ + add₃ …`

### Common Cases

| State | `target` | Meaning |
|---|---|---|
| Normal | `["<commit>"]` | Single add, points to that commit |
| Deleted | `[null]` | Single add of "absent" |
| Simple conflict | `["<A>", "<B>", "<C>"]` | Two sides diverged from base B; one went to A, the other to C |
| N-way conflict | `["<A>", "<B>", "<C>", "<D>", "<E>"]` | Compounded conflict, 3 adds, 2 removes |

## Ordering Within the Conflict Array

The ordering is **deterministic**, not arbitrary. From `merge_ref_targets(left=self, base, right=other)`:

- **Index 0** (first add): always the **local** side
- **Index 1** (first remove): the **base** (common ancestor)
- **Index 2** (second add): the **incoming remote** side

For multiple remotes, each fetch merges the previous result (now "local") with the next remote, so the latest remote's value ends up at the rightmost add position.

Each additional conflicting remote adds 2 more elements: `2N + 1` total for N conflicting remotes.

## `local_bookmarks` vs `remote_bookmarks`

`remote_bookmarks` provides more information:

- Each remote has its own `target` field showing that remote's non-conflicted position
- `tracking_target` shows the local bookmark's (possibly conflicted) state
- `@git` in a colocated repo represents the backing git ref — effectively the local side

`local_bookmarks` only gives you the merged conflict state.

### Deleted Remote Bookmarks Are Missing

If a remote bookmark was deleted, it **won't appear** in `json(remote_bookmarks)` output because there's no commit to attach it to. Example:

```
jj bookmark list --all fix-deleted-branches
fix-deleted-branches (conflicted):
  - xrmxlvwx/3 a5c81a79 (hidden) hand written logic
  + xrmxlvwx c8730757 hand written logic
  @git: xrmxlvwx c8730757 hand written logic
  @origin (not created yet)
```

But `json(remote_bookmarks)` only shows `@git`, not `@origin`. The `null` in the local conflict's target array is the only trace of the deleted remote side.

## `@git` vs Local Bookmark

In a colocated repo, `@git` is a pseudo-remote mirroring the backing git repo's refs. It stays in sync via import/export, but can diverge:

1. **Conflicted bookmarks are not exported** to git — `@git` gets stuck at its last successfully-exported value
2. **External git operations** can modify `.git/refs` without jj knowing until the next import
3. **Export failures** (e.g. invalid git ref names) leave `@git` behind

## Determining the Local Contribution to a Conflict

In priority order:

1. **`@git` target** — in a colocated repo, this is the local git ref value. Works when export succeeded.
2. **Index 0 of the conflict array** — always the local contribution by construction of `merge_ref_targets(self, base, other)`.
3. **Subtraction** — take the local bookmark's conflict adds and remove ones matching known remote targets. Most defensive but most work.

## `tracking_target` on Remote Bookmarks

The `tracking_target` field on a remote bookmark shows the state of the local bookmark that tracks it:

- `[null]` means the local tracking bookmark has been deleted (shows as `(deleted)` in `jj bookmark list`)
- A conflict array means the local bookmark is conflicted
- Matches `target` when synced
