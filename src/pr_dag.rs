use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Deref;

use anyhow::Result;
use ref_cast::RefCast;
use renderdag::{Ancestor, GraphRowRenderer, Renderer};
use slotmap::{SecondaryMap, SlotMap, SparseSecondaryMap, new_key_type};

use crate::gh::{GhPr, PrNum};
use crate::graph_algorithms;
use crate::jj::{self, JjLogEntry};
use crate::types::{AsRevset, Bookmark, ChangeId, CommitId};

new_key_type! {
    pub struct NodeKey;
}

/// The unified view of the repo state.
#[derive(Debug)]
pub struct RepoState {
    /// All the nodes in the repo state.
    pub nodes: SlotMap<NodeKey, Node>,
    /// The node key for `root()` (synthetic boundary below trunk).
    pub root_node: NodeKey,
    /// Node -> parent Nodes.
    pub node_preds: SecondaryMap<NodeKey, Vec<NodeKey>>,
    /// Node -> child Nodes.
    pub node_succs: SecondaryMap<NodeKey, Vec<NodeKey>>,

    /// Topological order of nodes (parents before children).
    pub topo_order: Vec<NodeKey>,

    /// Graph-computed: commit_id -> owning node.
    pub commit_node: HashMap<CommitId, NodeKey>,
    /// PRs whose local bookmark points to a different commit than the remote bookmark.
    pub pr_needs_push: HashSet<PrNum>,
    /// Nodes that have any pending sync action (push, rebase, base update), including
    /// transitive descendants of nodes that need rebase.
    /// Value: `true` = propagates to children (push/rebase), `false` = local only (base mismatch).
    pub node_needs_sync: SparseSecondaryMap<NodeKey, bool>,

    /// Bookmarks with conflicting targets that block sync (user must resolve with `jj bookmark`).
    pub bookmarks_blocking: BTreeSet<Bookmark>,

    /// Nodes containing at least one commit with content conflicts.
    pub node_conflicted: SparseSecondaryMap<NodeKey, ()>,

    /// Closed PR nodes that can be hidden from display (leaf-only: all successors also hidden).
    pub node_hidden: SparseSecondaryMap<NodeKey, ()>,

    /// The node containing the working copy (`@`), if any.
    pub node_current: Option<NodeKey>,
}

/// Represents a contiguous set of changes/commits within the JJ graph, usually for a PR.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Node {
    /// Represents the root - the synthetic "everything before trunk" boundary.
    Root,
    /// Represents the tip of `trunk()` - what top-level PRs should be rebased upon.
    TrunkTip,
    /// Represents a single PR.
    Pr(PrNum),
    /// Represents one or more ambiguous changes/commits.
    ///
    /// A node may be ambiguous either because its trailer PR num doesn't match the branch PR,
    /// or if there are multiple branch PRs that could own this commit (due to multiple children).
    Ambiguous {
        /// The set of branch PRs that could include this node.
        branch_prs: BTreeSet<PrNum>,
        /// The set of trailer PRs in commits in this node.
        trailer_prs: BTreeSet<PrNum>,
    },
}

impl Node {
    /// Whether this node contains the given PR.
    fn contains_pr(&self, pr_num: PrNum) -> bool {
        match self {
            Node::Root => false,
            Node::TrunkTip => false,
            Node::Pr(other_pr_num) => *other_pr_num == pr_num,
            Node::Ambiguous {
                branch_prs,
                trailer_prs,
            } => branch_prs.contains(&pr_num) || trailer_prs.contains(&pr_num),
        }
    }
}

/// Build the repo state from raw jj and GitHub data.
///
/// `jj_entries` must be in reverse topological order (children to parents to `trunk()`).
pub fn build(
    jj_entries: &[JjLogEntry],
    prs: &BTreeMap<PrNum, &GhPr>,
    default_branch: &Bookmark<str>,
    tracked_bookmarks: Option<&BTreeSet<Bookmark>>,
    push_remote: &str,
) -> Result<RepoState> {
    let mut nodes = SlotMap::with_key();
    let root_node = nodes.insert(Node::Root);
    let mut node_preds = SecondaryMap::new();
    node_preds.insert(root_node, Vec::new()); // `root()` has no parent nodes.

    let (
        node_succs,
        commit_node,
        pr_needs_push,
        node_needs_sync,
        bookmarks_blocking,
        node_conflicted,
        node_current,
        node_hidden,
    ) = Default::default();
    let mut repo_state = RepoState {
        nodes,
        root_node,
        node_preds,
        node_succs,
        topo_order: Vec::new(),
        commit_node,
        pr_needs_push,
        node_needs_sync,
        bookmarks_blocking,
        node_conflicted,
        node_hidden,
        node_current,
    };

    let (cid_pr_tip, pr_local) = {
        // For each commit_id, Which PR it belongs to, but only for the tip (last commit in the PR).
        let mut cid_pr_tip: HashMap<&CommitId<str>, Vec<PrNum>> = HashMap::new();

        // GH PR head ref name -> GH PR data.
        // TODO(mingwei): handle multiple PRs sharing the same bookmark (e.g. Open + Closed/Merged).
        let head_to_pr: HashMap<&Bookmark<str>, &GhPr> = prs.values().map(|&pr| (&*pr.head_ref_name, pr)).collect();

        let mut pr_local = HashSet::new();

        for jj_entry in jj_entries.iter() {
            for local_bookmark in jj_entry.local_bookmarks.iter() {
                let local_bookmark_name = &*local_bookmark.name;
                if let Some(pr) = head_to_pr.get(local_bookmark_name) {
                    // Only consider this a PR bookmark if it's tracked on the push remote.
                    // `None` means all bookmarks are considered tracked (legacy/old fixtures).
                    let is_tracked = tracked_bookmarks.is_none_or(|tb| tb.contains(local_bookmark_name));
                    if !is_tracked {
                        continue;
                    }
                    // This is a PR bookmark.
                    if 1 < local_bookmark.target.len() {
                        // Conflict! Check if this is a merged PR with a deleted remote
                        // (one side is null in an add slot). If so, we can auto-resolve
                        // by deleting the bookmark.
                        // Only process on the local side (index 0) to avoid duplicate
                        // handling — conflicted bookmarks appear on all non-null add commits.
                        let is_local_side = local_bookmark
                            .target
                            .first()
                            .is_some_and(|t| t.as_ref() == Some(&jj_entry.commit.commit_id));
                        // Check if any non-local add slot is null, indicating the
                        // remote side deleted the branch. Index 0 (local add) is
                        // checked separately via is_local_side.
                        let has_null_add = local_bookmark
                            .target
                            .iter()
                            .step_by(2) // even indices only (add slots)
                            .skip(1) // skip index 0 (local add)
                            .any(|t| t.is_none());
                        if is_local_side && has_null_add && pr.state == gh::PrState::Merged {
                            // Treat as the tip of a merged PR — bookmark will be deleted during abandon.
                            cid_pr_tip
                                .entry(&*jj_entry.commit.commit_id)
                                .or_default()
                                .push(pr.number);
                            pr_local.insert(pr.number);
                        } else if is_local_side {
                            // Only block on the local side to avoid duplicate insertions.
                            repo_state.bookmarks_blocking.insert(local_bookmark_name.to_owned());
                            pr_local.insert(pr.number);
                        }
                    } else if local_bookmark.target.as_slice() == [Some(jj_entry.commit.commit_id.clone())] {
                        // Note `local_bookmark.target == vec![Some(jj_entry.commit.commit_id)]`
                        // in the non-conflicted case.
                        cid_pr_tip
                            .entry(&*jj_entry.commit.commit_id)
                            .or_default()
                            .push(pr.number);
                        pr_local.insert(pr.number);
                    }
                }
            }
        }

        (cid_pr_tip, pr_local)
    };

    // Compute pr_needs_push: compare local vs remote bookmark targets for PR bookmarks.
    {
        let mut local_targets: HashMap<&Bookmark<str>, &CommitId<str>> = HashMap::new();
        let mut remote_targets: HashMap<&Bookmark<str>, &CommitId<str>> = HashMap::new();
        for jj_entry in jj_entries.iter() {
            for bm in &jj_entry.local_bookmarks {
                local_targets.insert(&bm.name, &jj_entry.commit.commit_id);
            }
            for bm in &jj_entry.remote_bookmarks {
                // Only consider the push remote, not @git (local tracking ref).
                if bm.remote.as_deref() == Some(push_remote) {
                    remote_targets.entry(&bm.name).or_insert(&jj_entry.commit.commit_id);
                }
            }
        }
        for gh_pr in prs.values() {
            let bookmark = &*gh_pr.head_ref_name;
            // Only consider bookmarks we own (tracked on push remote).
            let is_tracked = tracked_bookmarks.is_none_or(|tb| tb.contains(bookmark));
            if !is_tracked {
                continue;
            }
            let local = local_targets.get(bookmark);
            let remote = remote_targets.get(bookmark);
            if local != remote {
                repo_state.pr_needs_push.insert(gh_pr.number);
            }
        }
    }

    // First pass: compute membership.
    {
        let _scope = tracing::info_span!("membership").entered();

        // Map from each commit to its children (instead of parents).
        let mut cid_children = HashMap::<_, Vec<_>>::new();
        for jj_entry in jj_entries.iter() {
            for parent_cid in jj_entry.commit.parents.iter() {
                cid_children
                    .entry(&**parent_cid)
                    .or_default()
                    .push(&*jj_entry.commit.commit_id);
            }
        }

        // Traverse in reverse topological order (children to parents to `trunk()`).
        let mut already_found_prs = HashSet::new();
        for jj_entry in jj_entries.iter() {
            // Skip immutable commits that aren't trunk tip — these are historical/foreign
            // commits (e.g., other people's PRs fetched from origin) that shouldn't produce nodes.
            if jj_entry.immutable && !jj_entry.is_trunk_tip {
                continue;
            }

            let cid = &*jj_entry.commit.commit_id;
            let _cid_scope = tracing::info_span!("commit", %cid).entered();

            // At this point we gather all relevant information to decide what `Node` this should be:
            // - If the entry is the tip of `trunk()`
            // - The tip PR num, if this is the tip of a PR
            //   (or multiple if there are multiple PRs that have the same tip, although that is silly).
            // - All child `Node` types (and `NodeKey`s corresponding).
            // - The trailer PR num, if set.
            //
            // We also consider global info:
            // - If the trailer PR num has a local branch tracking it.
            // - If there is already a node for the PR.
            // - The PR state (e.g. merged) from GitHub data.
            //
            let is_trunk_tip = jj_entry.is_trunk_tip;
            let tip_pr_nums = cid_pr_tip.get(cid).map(Deref::deref).unwrap_or_default();
            let child_nodes = cid_children
                .get(cid)
                .into_iter() // Will be `None` at leaf children.
                .flatten()
                .filter_map(|&child_cid| {
                    let &child_nk = repo_state.commit_node.get(child_cid)?;
                    let child_node = repo_state.nodes.get(child_nk).unwrap();
                    Some((child_nk, child_node))
                })
                .collect::<SparseSecondaryMap<_, _>>();
            let trailer_pr_num = jj::parse_pr_trailer(&jj_entry.commit.description);

            // Main logic triggered here: decide how to assign the node.
            let (node, node_key) = decide(
                is_trunk_tip,
                tip_pr_nums,
                child_nodes,
                trailer_pr_num,
                &pr_local,
                &already_found_prs,
                prs,
            );

            // Insert or update
            {
                let _update_scope = tracing::info_span!("update", ?node, ?node_key).entered();
                let node_key = match (node, node_key) {
                    (None, None) => {
                        tracing::trace!("Commit appears to not belong to any node");
                        continue;
                    }
                    (None, Some(node_key)) => {
                        tracing::debug!("Assigning commit to existing");
                        node_key
                    }
                    (Some(new_node), None) => {
                        tracing::debug!("Assigning commit to new node");
                        if let Node::Pr(pr_num) = new_node {
                            let new = already_found_prs.insert(pr_num);
                            assert!(
                                new,
                                "bug: attempted to create a new PR node for an already-found PR: {}",
                                pr_num
                            );
                        }
                        repo_state.nodes.insert(new_node)
                    }
                    (Some(updated_node), Some(node_key)) => {
                        tracing::debug!("Assigning commit to existing and updating");
                        if let Node::Pr(pr_num) = updated_node {
                            assert!(
                                already_found_prs.contains(&pr_num),
                                "bug: attempted to update an *existing* node to be a *new* PR node: {}",
                                pr_num
                            );
                        }
                        repo_state.nodes[node_key] = updated_node;
                        node_key
                    }
                };
                repo_state.commit_node.insert(cid.to_owned(), node_key);
                if jj_entry.conflict {
                    repo_state.node_conflicted.insert(node_key, ());
                }
            }
        }
    }

    // Second pass: compute node DAG.
    {
        let _scope = tracing::info_span!("dag").entered();

        // Ensure all nodes have pred/succ entries.
        for nk in repo_state.nodes.keys() {
            repo_state.node_preds.entry(nk).unwrap().or_default();
            repo_state.node_succs.entry(nk).unwrap().or_default();
        }

        // Traverse in reverse topological order (children to parents to `trunk()`).
        for jj_entry in jj_entries.iter() {
            let cid = &*jj_entry.commit.commit_id;
            let _cid_scope = tracing::info_span!("commit", %cid).entered();

            let Some(&prev_nk) = repo_state.commit_node.get(cid) else {
                // This is a random commit not in any PRs, an branch off of the commits we actually care about.
                tracing::trace!("Skipping commit not in any PRs.");
                continue;
            };

            // If `jj_entries.get(prev_cid)` is `None`, then we should set `next_nk` to `trunk()`.
            let next_nks = jj_entry
                .commit
                .parents
                .iter()
                .map(|cid| repo_state.commit_node.get(cid).unwrap_or(&repo_state.root_node))
                .copied()
                .collect::<BTreeSet<_>>();

            for next_nk in next_nks {
                if prev_nk == next_nk {
                    // No self-references.
                    continue;
                }
                repo_state.node_preds.entry(prev_nk).unwrap().or_default().push(next_nk);
                repo_state.node_succs.entry(next_nk).unwrap().or_default().push(prev_nk);
            }
        }
    }

    // Third pass: topo sort and compute node_needs_sync.
    {
        let _scope = tracing::info_span!("topo_sync").entered();

        repo_state.topo_order = graph_algorithms::topo_sort(repo_state.nodes.keys(), |nk| {
            repo_state
                .node_preds
                .get(nk)
                .unwrap_or_else(|| panic!("bug: missing preds for node key {nk:?}"))
                .iter()
                .copied()
        })
        .unwrap_or_else(|cycle| {
            panic!(
                "bug: cycle detected in node DAG: {:?}",
                cycle
                    .iter()
                    .map(|&nk| repo_state.nodes.get(nk).unwrap())
                    .collect::<Vec<_>>()
            )
        });

        // Forward pass (parents before children) to propagate node_needs_sync.
        for &nk in repo_state.topo_order.iter() {
            let (needs_push_or_rebase, needs_base_update) = if let Some(Node::Pr(pr_num)) = repo_state.nodes.get(nk) {
                let gh_pr = prs.get(pr_num).expect("bug: PR node without GH PR data");
                if gh_pr.state == gh::PrState::Merged {
                    (false, false)
                } else {
                    let push_or_rebase =
                        repo_state.pr_needs_push.contains(pr_num)
                        || repo_state.node_preds.get(nk).unwrap().iter().any(|&pred_nk| {
                            matches!(repo_state.nodes.get(pred_nk), Some(Node::Pr(parent_pr)) if prs.get(parent_pr).is_some_and(|p| p.state == gh::PrState::Merged))
                        });
                    // Base mismatch (only for open PRs — can't change base of closed PRs).
                    let base_update = gh_pr.state == gh::PrState::Open && {
                        let expected = repo_state.expected_base(nk, prs, default_branch);
                        gh_pr.base_ref_name != expected
                    };
                    (push_or_rebase, base_update)
                }
            } else {
                (false, false)
            };

            let propagates = needs_push_or_rebase
                || repo_state
                    .node_preds
                    .get(nk)
                    .unwrap()
                    .iter()
                    .any(|&pred_nk| repo_state.node_needs_sync.get(pred_nk).copied() == Some(true));

            if propagates || needs_base_update {
                repo_state.node_needs_sync.insert(nk, propagates);
            }
        }
    }

    // Set node_current from the working copy entry.
    repo_state.node_current = jj_entries
        .iter()
        .find(|e| e.is_working_copy)
        .and_then(|e| repo_state.commit_node.get(&*e.commit.commit_id).copied());

    // Compute node_hidden: closed PRs without sync needs whose children are all hidden.
    // Reverse topo order = children before parents, so we know child visibility first.
    for &nk in repo_state.topo_order.iter().rev() {
        // Keep visible if there are pending sync actions.
        if repo_state.node_needs_sync.contains_key(nk) {
            continue;
        }
        // Only PR nodes can be hidden.
        let Some(Node::Pr(pr_id)) = repo_state.nodes.get(nk) else {
            continue;
        };
        // Only closed PRs are hidden.
        if !prs.get(pr_id).is_some_and(|p| p.state == gh::PrState::Closed) {
            continue;
        }
        // Keep visible if any child is visible.
        if repo_state
            .node_succs
            .get(nk)
            .into_iter()
            .flatten()
            .any(|&s| !repo_state.node_hidden.contains_key(s))
        {
            continue;
        }
        // Otherwise hide it.
        repo_state.node_hidden.insert(nk, ());
    }

    Ok(repo_state)
}

/// The main descision logic for assigning a commit to a `Node`.
///
/// Return value is `(new_node_value, existing_node_key)`, both options.
/// If the existing node key is `Some`, that key will be used, and possibly updated, in-place.
/// If the new node value is `Some`, that value will be used (inserted or updated based on key).
/// Returns `None` if the commit should be skipped.
fn decide(
    is_trunk_tip: bool,
    tip_pr_nums: &[PrNum],
    child_nodes: SparseSecondaryMap<NodeKey, &Node>,
    trailer_pr_num: Option<PrNum>,
    pr_local: &HashSet<PrNum>,
    already_found_prs: &HashSet<PrNum>,
    prs: &BTreeMap<PrNum, &GhPr>,
) -> (Option<Node>, Option<NodeKey>) {
    if is_trunk_tip {
        return (Some(Node::TrunkTip), None);
    }

    let (mut node, mut node_key) = match tip_pr_nums {
        // If no tip PR nums, inherit from the children.
        [] => decide_combine_child_nodes(child_nodes),
        // If single tip PR num, use it.
        &[single] => (Some(Node::Pr(single)), None),
        // If there are multiple, ambiguous.
        // (Cannot have multiple PRs with same PR num).
        multiple => (
            Some(Node::Ambiguous {
                branch_prs: multiple.iter().copied().collect(),
                trailer_prs: BTreeSet::new(),
            }),
            None,
        ),
    };
    // Finally, incorporate the trailer PR num.
    if let Some(trailer_pr_num) = trailer_pr_num {
        (node, node_key) = 'a: {
            // Ignore stale trailers: closed PRs with no local bookmark are dead.
            // The trailer will be overwritten by StampTrailer during sync.
            if !pr_local.contains(&trailer_pr_num)
                && prs.get(&trailer_pr_num).is_some_and(|p| p.state == gh::PrState::Closed)
            {
                break 'a (node, node_key);
            }

            // Special case: if the `trailer_pr_num` is set but the PR is not tracked locally, try to treat this as the tip
            // of a merged PR with a deleted branch (needs abandoning).
            if !pr_local.contains(&trailer_pr_num)
                && prs.get(&trailer_pr_num).is_some_and(|p| p.state == gh::PrState::Merged)
            {
                let _maybe_deleted_scope = tracing::info_span!("maybe_deleted", %trailer_pr_num).entered();
                tracing::debug!("Found trailer PR num for change which may be the tip of a deleted branch");

                if already_found_prs.contains(&trailer_pr_num) {
                    tracing::debug!("PR num is already tracked, ignoring");
                } else if node.as_ref().is_some_and(|n| n.contains_pr(trailer_pr_num)) {
                    tracing::debug!("PR num was already found for this commit otherwise, ignoring");
                } else {
                    tracing::debug!("PR num is not tracked, treating as tip of PR");
                    break 'a (Some(Node::Pr(trailer_pr_num)), None);
                }
            }

            if let Some(node) = node {
                let (node, node_key) = match node {
                    Node::Root => unreachable!(),
                    Node::TrunkTip => unreachable!(),
                    Node::Pr(same_pr_num) if same_pr_num == trailer_pr_num => (node, node_key),
                    Node::Pr(other_pr_num) => (
                        Node::Ambiguous {
                            branch_prs: [other_pr_num].into(),
                            trailer_prs: [trailer_pr_num].into(),
                        },
                        None,
                    ),
                    Node::Ambiguous {
                        branch_prs,
                        mut trailer_prs,
                    } => {
                        trailer_prs.insert(trailer_pr_num);
                        (
                            Node::Ambiguous {
                                branch_prs,
                                trailer_prs,
                            },
                            node_key, // Update existing.
                        )
                    }
                };
                (Some(node), node_key)
            } else {
                // A trailer PR num alone is not enough to start a PR node.
                (
                    Some(Node::Ambiguous {
                        branch_prs: BTreeSet::new(),
                        trailer_prs: [trailer_pr_num].into(),
                    }),
                    None,
                )
            }
        };
    }
    (node, node_key)
}

/// [`decide`] helper for combining child nodes.
fn decide_combine_child_nodes(child_nodes: SparseSecondaryMap<NodeKey, &Node>) -> (Option<Node>, Option<NodeKey>) {
    // Merge multiple children together.
    let mut node_and_nk = None;
    #[expect(
        clippy::disallowed_methods,
        reason = "iteration order doesn't affect result — merging into sets"
    )]
    for (next_nk, &next_node) in child_nodes.iter() {
        let Some((prev_node, prev_nk)) = &mut node_and_nk else {
            // Simple/initial case: inherit from one child:
            node_and_nk = Some((next_node.clone(), Some(next_nk)));
            continue;
        };
        match (&*prev_node, next_node) {
            (Node::Root, _) | (_, &Node::Root) => unreachable!(),
            (Node::TrunkTip, _) | (_, &Node::TrunkTip) => unreachable!(),
            (&Node::Pr(pr_num_prev), &Node::Pr(pr_num_next)) if pr_num_prev == pr_num_next => {
                // Matches, keep current.
            }
            (&Node::Pr(pr_num_prev), &Node::Pr(pr_num_next)) => {
                *prev_node = Node::Ambiguous {
                    branch_prs: [pr_num_prev, pr_num_next].into(),
                    trailer_prs: BTreeSet::new(),
                };
                *prev_nk = None;
            }
            (
                &Node::Pr(pr_num),
                Node::Ambiguous {
                    branch_prs,
                    trailer_prs,
                },
            )
            | (
                Node::Ambiguous {
                    branch_prs,
                    trailer_prs,
                },
                &Node::Pr(pr_num),
            ) => {
                let mut branch_prs = branch_prs.clone();
                let same = !branch_prs.insert(pr_num);
                *prev_node = Node::Ambiguous {
                    branch_prs,
                    trailer_prs: if same { trailer_prs.clone() } else { BTreeSet::new() },
                };
                *prev_nk = same.then_some(next_nk);
            }
            (
                Node::Ambiguous {
                    branch_prs,
                    trailer_prs,
                },
                Node::Ambiguous {
                    branch_prs: branch_prs_next,
                    trailer_prs: trailer_prs_next,
                },
            ) => {
                let same = branch_prs == branch_prs_next;
                *prev_node = Node::Ambiguous {
                    branch_prs: branch_prs.iter().chain(branch_prs_next).copied().collect(),
                    trailer_prs: if same {
                        trailer_prs.iter().chain(trailer_prs_next).copied().collect()
                    } else {
                        BTreeSet::new()
                    },
                };
                *prev_nk = same.then_some(next_nk);
            }
        }
    }
    node_and_nk
        .map(|(node, node_key)| (Some(node), node_key))
        .unwrap_or((None, None))
}

// --- Sync status helpers ---

impl RepoState {
    /// Returns the expected GitHub base branch name for a node, derived from the DAG.
    /// If the parent is another open PR, returns its head_ref_name.
    /// Otherwise returns the default branch name (trunk).
    pub fn expected_base(&self, nk: NodeKey, prs: &BTreeMap<PrNum, &GhPr>, default_branch: &Bookmark<str>) -> Bookmark {
        let Some(preds) = self.node_preds.get(nk) else {
            return default_branch.to_owned();
        };
        // If there's exactly one parent that is an open PR, use its branch.
        let parent_prs: Vec<_> = preds
            .iter()
            .filter_map(|&pred_nk| {
                if let Some(Node::Pr(parent_pr)) = self.nodes.get(pred_nk) {
                    prs.get(parent_pr)
                        .filter(|p| p.state == gh::PrState::Open)
                        .map(|p| &p.head_ref_name)
                } else {
                    None
                }
            })
            .collect();
        if let [single] = parent_prs.as_slice() {
            (*single).clone()
        } else {
            default_branch.to_owned()
        }
    }

    /// Whether a node is conflicted (content conflicts or bookmark conflicts).
    pub fn is_node_conflicted(&self, nk: NodeKey, prs: &BTreeMap<PrNum, &GhPr>) -> bool {
        if self.node_conflicted.contains_key(nk) {
            return true;
        }
        matches!(self.nodes.get(nk), Some(Node::Pr(pr_id)) if prs
            .get(pr_id)
            .is_some_and(|pr| self.bookmarks_blocking.contains(&*pr.head_ref_name)))
    }
}
pub fn extract_pr_nums(jj_entries: &[JjLogEntry]) -> Vec<PrNum> {
    let mut nums = BTreeSet::new();
    for entry in jj_entries {
        if let Some(pr_num) = jj::parse_pr_trailer(&entry.commit.description) {
            nums.insert(pr_num);
        }
    }
    nums.into_iter().collect()
}

// --- Rendering ---

/// Build the CI and review status indicator string for a PR.
///
/// Returns a string with a leading space containing CI and review status indicators.
fn ci_review_indicators(status: &gh::PrStatus) -> String {
    let mut parts = Vec::new();
    if let Some(cs) = status.checks_status {
        parts.push(crate::style::ci_status(cs));
    }
    parts.push(crate::style::review_status(status.review_decision));
    format!(" {}", parts.join(" "))
}

/// Render the PR DAG as a graph.
pub fn render_show(
    state: &RepoState,
    prs: &BTreeMap<PrNum, &GhPr>,
    pr_statuses: &BTreeMap<PrNum, gh::PrStatus>,
    show_all: bool,
    reversed: bool,
    out: &mut impl std::io::Write,
) -> Result<()> {
    let mut renderer = GraphRowRenderer::new()
        .output()
        .with_min_row_height(1)
        .build_box_drawing();

    let iter: Box<dyn Iterator<Item = &NodeKey>> = if reversed {
        Box::new(state.topo_order.iter())
    } else {
        Box::new(state.topo_order.iter().rev())
    };
    let edge_map = if reversed { &state.node_succs } else { &state.node_preds };

    for &node_key in iter {
        if !show_all && state.node_hidden.contains_key(node_key) {
            continue;
        }

        let parents = edge_map
            .get(node_key)
            .unwrap()
            .iter()
            .map(|&dag_node| Ancestor::Parent(dag_node))
            .collect::<Vec<_>>();

        let is_current = state.node_current == Some(node_key);

        let node = state.nodes.get(node_key).unwrap();
        let conflicted = state.is_node_conflicted(node_key, prs);
        let glyph = match (is_current, conflicted, node) {
            (true, true, _) => crate::style::glyph_current_conflicted(),
            (true, false, _) => crate::style::glyph_current(),
            (false, _, Node::Root) => crate::style::glyph_elided(),
            (false, _, Node::TrunkTip) => crate::style::glyph_immutable(),
            (false, true, Node::Pr(_)) => crate::style::glyph_conflicted(),
            (false, false, Node::Pr(_)) => crate::style::GLYPH_MUTABLE.to_owned(),
            (false, true, Node::Ambiguous { .. }) => crate::style::glyph_warning_conflicted(),
            (false, false, Node::Ambiguous { .. }) => crate::style::warn(crate::style::GLYPH_WARNING),
        };

        let message = match node {
            Node::Root => crate::style::root(),
            Node::TrunkTip => crate::style::trunk(),
            Node::Pr(pr_id) => {
                let gh_pr = prs.get(pr_id).unwrap();
                let sync_indicator = if state.node_needs_sync.contains_key(node_key) {
                    "*"
                } else {
                    ""
                };
                let ci_review = pr_statuses.get(pr_id).map(ci_review_indicators).unwrap_or_default();
                format!(
                    "{}{sync_indicator}  {}{ci_review}  {}\n{}",
                    crate::style::pr_num(*pr_id, Some(&gh_pr.url)),
                    crate::style::status(gh_pr.state, gh_pr.is_draft),
                    crate::style::bookmark(&gh_pr.head_ref_name),
                    gh_pr.title,
                )
            }
            Node::Ambiguous {
                branch_prs,
                trailer_prs,
            } => {
                let sync_indicator = if state.node_needs_sync.contains_key(node_key) {
                    "*"
                } else {
                    ""
                };
                let mut parts: Vec<String> = Vec::new();
                if !branch_prs.is_empty() {
                    let pr_links: Vec<String> = branch_prs
                        .iter()
                        .map(|pr_id| {
                            let url = prs.get(pr_id).map(|p| p.url.as_str());
                            crate::style::pr_num(*pr_id, url)
                        })
                        .collect();
                    parts.push(format!("branches: {}", pr_links.join(", ")));
                }
                if !trailer_prs.is_empty() {
                    let trailer_strs: Vec<String> = trailer_prs
                        .iter()
                        .map(|pr_id| {
                            let url = prs.get(pr_id).map(|p| p.url.as_str());
                            crate::style::pr_num(*pr_id, url)
                        })
                        .collect();
                    parts.push(format!("trailers: {}", trailer_strs.join(", ")));
                }
                let line1 = format!(
                    "{}{sync_indicator} {}",
                    crate::style::warn("ambiguous"),
                    parts.join(", "),
                );
                let has_shared = branch_prs.len() > 1;
                let has_trailer_mismatch = !trailer_prs.is_subset(branch_prs);
                let line2 = match (has_shared, has_trailer_mismatch) {
                    (true, true) => {
                        crate::style::dim("(restructure PRs to resolve, and edit commit trailers to fix PR: tags)")
                    }
                    (true, false) => crate::style::dim("(restructure PRs to resolve — stack one on the other)"),
                    (false, true) => crate::style::dim("(edit commit description to fix PR: trailer)"),
                    // Same PR claimed by multiple sources (branch + orphan trailer, or multiple orphan trailers).
                    (false, false) => {
                        crate::style::dim("(multiple sources for same PR — abandon stale commits or restructure)")
                    }
                };
                format!("{line1}\n{line2}")
            }
        };

        let row = renderer.next_row(node_key, parents, glyph, message);
        write!(out, "{row}")?;
    }

    Ok(())
}

pub fn render_log(
    state: &RepoState,
    prs: &BTreeMap<PrNum, &GhPr>,
    pr_statuses: &BTreeMap<PrNum, gh::PrStatus>,
    jj_entries: &[JjLogEntry],
    show_all: bool,
    reversed: bool,
    out: &mut impl std::io::Write,
) -> Result<()> {
    // Compute the set of entries that will actually be rendered (for edge filtering).
    let visible_entries: BTreeSet<&CommitId<str>> = jj_entries
        .iter()
        .filter(|e| {
            let cid = &*e.commit.commit_id;
            let node_key = state.commit_node.get(cid).copied();
            show_all || node_key.is_some_and(|nk| !state.node_hidden.contains_key(nk))
        })
        .map(|e| &*e.commit.commit_id)
        .collect();

    // Build child map for reversed edge lookup. In the forward case, parents are
    // already stored per-entry in `jj_entry.commit.parents`, but the inverse
    // (children) must be pre-computed. Only include children that will be rendered.
    let children_map: HashMap<&CommitId<str>, Vec<&CommitId<str>>> = if reversed {
        let mut map: HashMap<&CommitId<str>, Vec<&CommitId<str>>> = HashMap::new();
        for e in jj_entries {
            let cid = &*e.commit.commit_id;
            if !visible_entries.contains(cid) {
                continue;
            }
            for parent in &e.commit.parents {
                map.entry(&**parent).or_default().push(cid);
            }
        }
        map
    } else {
        HashMap::new()
    };

    let mut renderer = GraphRowRenderer::new()
        .output()
        .with_min_row_height(1)
        .build_box_drawing();

    let iter: Box<dyn Iterator<Item = &JjLogEntry>> = if reversed {
        Box::new(jj_entries.iter().rev())
    } else {
        Box::new(jj_entries.iter())
    };

    // Print root at the top in reversed mode.
    if reversed {
        let row = renderer.next_row(None, Vec::new(), crate::style::glyph_elided(), crate::style::root());
        write!(out, "{row}")?;
    }

    for jj_entry in iter {
        let cid = &*jj_entry.commit.commit_id;
        let _cid_scope = tracing::info_span!("commit", %cid).entered();

        let node_key = state.commit_node.get(cid).copied();
        if !show_all && node_key.is_none() {
            tracing::trace!("Skipping commit not in any PRs.");
            continue;
        };
        if !show_all && node_key.is_some_and(|nk| state.node_hidden.contains_key(nk)) {
            continue;
        }

        // Build the glyph.
        let node = node_key.map(|nk| state.nodes.get(nk).unwrap());
        let glyph = match (jj_entry.is_working_copy, jj_entry.conflict, node) {
            (true, true, _) => crate::style::glyph_current_conflicted(),
            (true, false, _) => crate::style::glyph_current(),
            (false, true, Some(Node::Ambiguous { .. })) => crate::style::glyph_warning_conflicted(),
            (false, true, _) => crate::style::glyph_conflicted(),
            (false, false, Some(Node::Root | Node::TrunkTip)) => crate::style::glyph_immutable(),
            (false, false, Some(Node::Ambiguous { .. })) => crate::style::warn(crate::style::GLYPH_WARNING),
            (false, false, Some(Node::Pr(_)) | None) => crate::style::GLYPH_MUTABLE.to_owned(),
        };

        // First line: change_id commit_id [bookmarks] [PR info]
        let mut line1_parts = vec![
            crate::style::change_id(&jj_entry.commit.change_id),
            crate::style::commit_id_short(cid),
        ];

        // Add bookmark labels.
        for bm in &jj_entry.local_bookmarks {
            line1_parts.push(crate::style::bookmark_label(&bm.name));
        }

        if let Some(node_key) = node_key {
            match state.nodes.get(node_key).unwrap() {
                Node::Root => {
                    panic!("bug: root node should not appear in jj entries");
                }
                Node::TrunkTip => {
                    line1_parts.push(crate::style::trunk());
                }
                Node::Pr(pr_id) => {
                    let gh_pr = prs.get(pr_id);
                    let sync_indicator = if state.node_needs_sync.contains_key(node_key) {
                        "*"
                    } else {
                        ""
                    };
                    let pr_str = match gh_pr {
                        Some(pr) => {
                            let ci_review = pr_statuses.get(pr_id).map(ci_review_indicators).unwrap_or_default();
                            format!(
                                "{}{sync_indicator} {}{ci_review}",
                                crate::style::pr_num(*pr_id, Some(&pr.url)),
                                crate::style::status(pr.state, pr.is_draft),
                            )
                        }
                        None => format!("{}{sync_indicator}", crate::style::pr_num(*pr_id, None),),
                    };
                    line1_parts.push(pr_str);
                }
                Node::Ambiguous {
                    branch_prs,
                    trailer_prs,
                } => {
                    let sync_indicator = if state.node_needs_sync.contains_key(node_key) {
                        "*"
                    } else {
                        ""
                    };
                    let mut parts: Vec<String> = Vec::new();
                    if !branch_prs.is_empty() {
                        let pr_strs: Vec<String> = branch_prs
                            .iter()
                            .map(|pr_id| {
                                let url = prs.get(pr_id).map(|p| p.url.as_str());
                                crate::style::pr_num(*pr_id, url)
                            })
                            .collect();
                        parts.push(pr_strs.join(", "));
                    }
                    if !trailer_prs.is_empty() {
                        let trailer_strs: Vec<String> = trailer_prs
                            .iter()
                            .map(|pr_id| {
                                let url = prs.get(pr_id).map(|p| p.url.as_str());
                                crate::style::pr_num(*pr_id, url)
                            })
                            .collect();
                        parts.push(format!("trailers: {}", trailer_strs.join(", ")));
                    }
                    let ambig = format!(
                        "{}{sync_indicator} {}{}{}",
                        crate::style::warn("ambiguous"),
                        crate::style::warn("["),
                        parts.join("; "),
                        crate::style::warn("]"),
                    );
                    line1_parts.push(ambig);
                }
            }
        } else {
            line1_parts.push(crate::style::dim("(no PR)"));
        }

        let line1 = line1_parts.join(" ");

        // Second line: description (with empty marker if applicable).
        let empty_prefix = if jj_entry.empty {
            format!("{} ", crate::style::empty_marker())
        } else {
            String::new()
        };
        let line2 = format!(
            "{empty_prefix}{}",
            crate::style::description_first_line(&jj_entry.commit.description)
        );

        let message = format!("{line1}\n{line2}");

        let edges = if reversed {
            children_map
                .get(cid)
                .into_iter()
                .flatten()
                .map(|&child_cid| Ancestor::Parent(visible_entries.contains(child_cid).then_some(child_cid)))
                .collect()
        } else {
            jj_entry
                .commit
                .parents
                .iter()
                .map(|cid| Ancestor::Parent(visible_entries.contains(&**cid).then_some(&**cid)))
                .collect()
        };

        let row = renderer.next_row(Some(cid), edges, glyph, message);
        write!(out, "{row}")?;
    }

    // Print root.
    if !reversed {
        let row = renderer.next_row(None, Vec::new(), crate::style::glyph_elided(), crate::style::root());
        write!(out, "{row}")?;
    }

    Ok(())
}

// --- Sync ---

use std::fmt;

use crate::gh;

#[derive(Debug)]
pub enum SyncAction {
    /// Stamp a missing `PR: #N` trailer on a commit.
    StampTrailer { change_id: ChangeId, pr: PrNum },
    /// Abandon commits of a merged PR and delete its bookmark if present.
    /// First rebases the merged commits onto trunk so that abandoning them
    /// reparents children to trunk while preserving other parent edges.
    AbandonMerged {
        /// Change IDs of all commits in this PR (stable across rewrites).
        change_ids: Vec<ChangeId>,
        pr: PrNum,
        bookmark: Bookmark,
        bookmark_exists: bool,
    },
    /// Push bookmarks that differ from remote.
    Push { bookmarks: Vec<(PrNum, Bookmark)> },
    /// Update a PR's base branch on GitHub.
    UpdateBase {
        pr: PrNum,
        bookmark: Bookmark,
        new_base: Bookmark,
    },
}

impl fmt::Display for SyncAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncAction::StampTrailer { change_id, pr } => {
                write!(f, "stamp {pr} trailer on {change_id:.12}")
            }
            SyncAction::AbandonMerged {
                pr,
                bookmark,
                bookmark_exists,
                ..
            } => {
                if *bookmark_exists {
                    write!(f, "abandon merged {pr} (delete {bookmark})")
                } else {
                    write!(f, "abandon merged {pr} ({bookmark} already deleted)")
                }
            }
            SyncAction::Push { bookmarks } => {
                let details: Vec<_> = bookmarks.iter().map(|(pr, bm)| format!("{pr} ({bm})")).collect();
                write!(f, "push: {}", details.join(", "))
            }
            SyncAction::UpdateBase { pr, bookmark, new_base } => {
                write!(f, "update {pr} ({bookmark}) base -> {new_base}")
            }
        }
    }
}

/// The result of planning a sync: actions to execute and warnings to display.
#[derive(Debug)]
pub struct SyncPlan {
    pub actions: Vec<SyncAction>,
    pub warnings: Vec<String>,
}

/// Plan sync actions. Returns Err if blocking issues exist.
pub fn plan_sync(
    state: &RepoState,
    prs: &BTreeMap<PrNum, &GhPr>,
    jj_entries: &[JjLogEntry],
    default_branch: &Bookmark<str>,
    // Merge commit OIDs that exist in the local repo (for stale trunk detection).
    // `None` means all merge commits are considered present (legacy behavior).
    existing_merge_commits: Option<&HashSet<CommitId>>,
) -> Result<SyncPlan> {
    // Block on unresolvable conflicted bookmarks.
    if !state.bookmarks_blocking.is_empty() {
        let names: Vec<_> = state.bookmarks_blocking.iter().map(|s| s.to_string()).collect();
        anyhow::bail!(
            "Conflicted bookmark(s): {}. Resolve with `jj bookmark` before syncing.",
            names.join(", ")
        );
    }

    let mut actions = Vec::new();
    let mut warnings = Vec::new();

    // Bookmark names that exist locally (for determining if we need to delete during abandon).
    let local_bookmark_names: HashSet<&Bookmark<str>> = jj_entries
        .iter()
        .flat_map(|e| e.local_bookmarks.iter().map(|bm| &*bm.name))
        .collect();

    // 1. Stamp missing trailers.
    for jj_entry in jj_entries {
        let cid = &*jj_entry.commit.commit_id;
        let Some(&nk) = state.commit_node.get(cid) else {
            continue;
        };
        let Some(Node::Pr(pr_num)) = state.nodes.get(nk) else {
            continue;
        };
        let existing_trailer = jj::parse_pr_trailer(&jj_entry.commit.description);
        if existing_trailer != Some(*pr_num) {
            actions.push(SyncAction::StampTrailer {
                change_id: jj_entry.commit.change_id.clone(),
                pr: *pr_num,
            });
        }
    }

    // 2. Abandon merged PRs (rebase onto trunk first to preserve sibling parent edges).
    // Skip merged PRs whose merge commit isn't in the local repo (trunk is stale — needs fetch).
    for &nk in state.topo_order.iter() {
        let Some(Node::Pr(pr_num)) = state.nodes.get(nk) else {
            continue;
        };
        let Some(gh_pr) = prs.get(pr_num) else { continue };
        if gh_pr.state != gh::PrState::Merged {
            continue;
        }
        // Skip if the merge commit isn't in the local repo (trunk is stale — needs fetch).
        if existing_merge_commits.is_some_and(|existing| {
            gh_pr
                .merge_commit_oid
                .as_deref()
                .is_some_and(|oid| !existing.contains(oid))
        }) {
            warnings.push(format!(
                "skip merged {} ({}) — trunk stale, run `jj git fetch`",
                pr_num, gh_pr.head_ref_name,
            ));
            continue;
        }
        // Collect commits in this node (tip first, since jj_entries is reverse topo order).
        let node_entries = jj_entries
            .iter()
            .filter(|e| state.commit_node.get(&*e.commit.commit_id) == Some(&nk))
            .collect::<Vec<_>>();
        if node_entries.is_empty() {
            continue;
        }
        let change_ids: Vec<ChangeId> = node_entries.iter().map(|e| e.commit.change_id.clone()).collect();
        actions.push(SyncAction::AbandonMerged {
            change_ids,
            pr: *pr_num,
            bookmark: gh_pr.head_ref_name.clone(),
            bookmark_exists: local_bookmark_names.contains(&*gh_pr.head_ref_name),
        });
    }

    // 4. Push — collect all PR bookmarks that need pushing.
    // Skip nodes that are conflicted or have a conflicted ancestor (can't push children
    // of a conflicted intermediate).
    {
        let mut push_bookmarks = Vec::new();
        let mut blocked_by_conflict: HashSet<NodeKey> = HashSet::new();
        for &nk in state.topo_order.iter() {
            let conflicted_self = state.node_conflicted.contains_key(nk);
            let conflicted_pred = state
                .node_preds
                .get(nk)
                .unwrap()
                .iter()
                .any(|pred_nk| blocked_by_conflict.contains(pred_nk));

            // Propagate blocked status from self & predecessors.
            if conflicted_self || conflicted_pred {
                blocked_by_conflict.insert(nk);
            }

            if !state.node_needs_sync.contains_key(nk) {
                continue;
            }
            let Some(Node::Pr(pr_num)) = state.nodes.get(nk) else {
                continue;
            };
            let Some(gh_pr) = prs.get(pr_num) else { continue };
            assert_ne!(
                gh::PrState::Merged,
                gh_pr.state,
                "bug: merged PR {pr_num} in node_needs_sync",
            );
            if conflicted_self {
                warnings.push(format!(
                    "skip push {} ({}) — has content conflicts",
                    pr_num, gh_pr.head_ref_name,
                ));
                continue;
            }
            if conflicted_pred {
                warnings.push(format!(
                    "skip push {} ({}) — ancestor has content conflicts",
                    pr_num, gh_pr.head_ref_name,
                ));
                continue;
            }
            push_bookmarks.push((*pr_num, gh_pr.head_ref_name.clone()));
        }
        if !push_bookmarks.is_empty() {
            actions.push(SyncAction::Push {
                bookmarks: push_bookmarks,
            });
        }
    }

    // 5. Update GitHub base branches (open PRs only — can't change base of closed PRs).
    for &nk in state.topo_order.iter() {
        if !state.node_needs_sync.contains_key(nk) {
            continue;
        }
        let Some(Node::Pr(pr_num)) = state.nodes.get(nk) else {
            continue;
        };
        let Some(gh_pr) = prs.get(pr_num) else { continue };
        assert_ne!(
            gh::PrState::Merged,
            gh_pr.state,
            "bug: merged PR {pr_num} in node_needs_sync",
        );
        if gh_pr.state != gh::PrState::Open {
            continue;
        }
        let expected = state.expected_base(nk, prs, default_branch);
        if gh_pr.base_ref_name != expected {
            actions.push(SyncAction::UpdateBase {
                pr: *pr_num,
                bookmark: gh_pr.head_ref_name.clone(),
                new_base: expected,
            });
        }
    }

    Ok(SyncPlan { actions, warnings })
}

/// Execute planned sync actions.
pub fn execute_sync(actions: &[SyncAction]) -> Result<()> {
    for action in actions {
        match action {
            SyncAction::StampTrailer { change_id, pr } => {
                eprintln!(
                    "Stamping {} on {}",
                    crate::style::pr_num(*pr, None),
                    crate::style::change_id(change_id),
                );
                let rev = change_id.as_revset();
                let desc = jj::read_description(&rev)?;
                let new_desc = jj::set_pr_trailer(&desc, *pr);
                jj::describe_stdin(&rev, &new_desc)?;
            }
            SyncAction::AbandonMerged {
                change_ids,
                pr,
                bookmark,
                bookmark_exists,
            } => {
                if *bookmark_exists {
                    eprintln!(
                        "Abandoning merged {} (delete {})",
                        crate::style::pr_num(*pr, None),
                        crate::style::bookmark(bookmark),
                    );
                    jj::bookmark_delete(bookmark)?;
                } else {
                    eprintln!(
                        "Abandoning merged {} ({} already deleted)",
                        crate::style::pr_num(*pr, None),
                        crate::style::bookmark(bookmark),
                    );
                }
                // Rebase merged commits onto trunk first, then abandon.
                //
                // Why not just abandon directly? Because `jj abandon` reparents
                // children to the abandoned commit's *current* parents. If the
                // merged PR's root is based on an older trunk commit, children
                // would be reparented there instead of the current trunk tip.
                //
                // Why not rebase children directly onto trunk? Because that would
                // destroy other parent edges. If a child has two parents (this
                // merged PR + another open PR), rebasing the child onto trunk
                // would lose its relationship with the open PR.
                //
                // By rebasing the merged PR onto trunk and then abandoning,
                // `jj abandon` reparents children to trunk (the new parent of
                // the abandoned commits) while preserving the children's other
                // parent edges intact.
                //
                // We use change_ids for both steps (stable across rewrites).
                // The revset targets only this PR's commits (not ancestors from
                // other PRs that may still be open).
                let revset = crate::types::revset_union(change_ids.iter());
                jj::rebase(&format!("roots({revset})"), "trunk()")?;
                jj::abandon(&revset)?;
            }
            SyncAction::Push { bookmarks } => {
                eprintln!(
                    "Pushing {} bookmark(s): {}",
                    bookmarks.len(),
                    bookmarks
                        .iter()
                        .map(|(_, b)| crate::style::bookmark(b))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                let refs: Vec<&Bookmark<str>> = bookmarks.iter().map(|(_, s)| &**s).collect();
                jj::git_push_bookmarks(&refs)?;
            }
            SyncAction::UpdateBase { pr, bookmark, new_base } => {
                eprintln!(
                    "Updating {} ({}) base -> {}",
                    crate::style::pr_num(*pr, None),
                    crate::style::bookmark(bookmark),
                    crate::style::bookmark(new_base),
                );
                gh::edit_base(pr.get(), new_base)?;
            }
        }
    }
    Ok(())
}

// --- Create ---

use anyhow::Context;

/// Find the base branch for a bookmark by walking the parent graph.
/// Returns the head_ref_name of the nearest ancestor PR, or `default_branch` if none.
fn find_base_branch(
    state: &RepoState,
    prs: &BTreeMap<PrNum, &GhPr>,
    jj_entries: &[JjLogEntry],
    bookmark: &Bookmark<str>,
    default_branch: &Bookmark<str>,
) -> Bookmark {
    // Find the tip commit for this bookmark.
    let tip_cid = jj_entries.iter().find_map(|e| {
        e.local_bookmarks
            .iter()
            .any(|bm| *bm.name == *bookmark)
            .then_some(&*e.commit.commit_id)
    });
    let Some(tip_cid) = tip_cid else {
        return default_branch.to_owned();
    };

    // Build a parent lookup from jj_entries.
    let parent_map: HashMap<&CommitId<str>, &[CommitId]> = jj_entries
        .iter()
        .map(|e| (&*e.commit.commit_id, e.commit.parents.as_slice()))
        .collect();

    // BFS up through parents looking for a commit owned by a different PR node.
    let tip_node = state.commit_node.get(tip_cid);
    let mut queue = std::collections::VecDeque::new();
    let mut visited = HashSet::new();
    if let Some(parents) = parent_map.get(tip_cid) {
        for p in *parents {
            queue.push_back(&**p);
        }
    }
    while let Some(cid) = queue.pop_front() {
        if !visited.insert(cid) {
            continue;
        }
        if let Some(&nk) = state.commit_node.get(cid) {
            // Skip if same node as the tip (same PR).
            if tip_node != Some(&nk) {
                if let Some(Node::Pr(pr_num)) = state.nodes.get(nk)
                    && let Some(gh_pr) = prs.get(pr_num)
                    && gh_pr.state != gh::PrState::Merged
                {
                    return gh_pr.head_ref_name.clone();
                }
                // Hit trunk/root/ambiguous — use default.
                return default_branch.to_owned();
            }
        }
        if let Some(parents) = parent_map.get(cid) {
            for p in *parents {
                queue.push_back(&**p);
            }
        } else {
            // Parent not in entries — beyond revset, treat as trunk.
            return default_branch.to_owned();
        }
    }
    default_branch.to_owned()
}

/// The planned actions for creating a new PR.
#[derive(Debug)]
pub struct CreatePlan {
    pub bookmark: Bookmark,
    pub base: Bookmark,
    pub title: String,
    pub body: String,
    /// Change IDs of commits that will be stamped with the PR trailer.
    pub stamp_change_ids: Vec<ChangeId>,
}

impl fmt::Display for CreatePlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "push {}", self.bookmark)?;
        writeln!(
            f,
            "create PR: \"{}\" ({} → {}) [draft]",
            self.title, self.bookmark, self.base,
        )?;
        if !self.stamp_change_ids.is_empty() {
            writeln!(f, "stamp trailer on {} commit(s)", self.stamp_change_ids.len())?;
        }
        Ok(())
    }
}

/// Plan the creation of a new PR for an existing bookmark. Pure — no side effects.
pub fn plan_create(
    state: &RepoState,
    prs: &BTreeMap<PrNum, &GhPr>,
    jj_entries: &[JjLogEntry],
    default_branch: &Bookmark<str>,
    bookmark: &str,
    title: Option<&str>,
    body: Option<&str>,
) -> Result<CreatePlan> {
    let bookmark_ref = Bookmark::ref_cast(bookmark);

    // Verify bookmark exists.
    let tip_entry = jj_entries
        .iter()
        .find(|e| e.local_bookmarks.iter().any(|bm| *bm.name == *bookmark_ref))
        .with_context(|| {
            format!(
                "bookmark '{}' not found — create it with `jj bookmark create {}`",
                bookmark, bookmark
            )
        })?;

    // Reject conflicted bookmarks.
    let bm = tip_entry
        .local_bookmarks
        .iter()
        .find(|bm| *bm.name == *bookmark_ref)
        .expect("bookmark must exist — already verified above");
    anyhow::ensure!(
        bm.target.len() == 1,
        "bookmark '{}' is conflicted — resolve with `jj bookmark set {}` before creating a PR",
        bookmark,
        bookmark,
    );

    // Verify no PR already exists for this bookmark.
    if let Some(existing) = prs.values().find(|pr| *pr.head_ref_name == *bookmark_ref) {
        anyhow::bail!(
            "bookmark '{}' already has {} — use `jj-pr sync` to update it",
            bookmark,
            existing.number,
        );
    }

    let base = find_base_branch(state, prs, jj_entries, bookmark_ref, default_branch);

    let title = title.map(|s| s.to_owned()).unwrap_or_else(|| {
        tip_entry
            .commit
            .description
            .lines()
            .next()
            .unwrap_or("untitled")
            .to_owned()
    });
    let body = body.map(|s| s.to_owned()).unwrap_or_else(|| {
        tip_entry
            .commit
            .description
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned()
    });

    // Walk backwards from tip to find commits that need stamping.
    // We can't use `state.commit_node` alone here because `state` was built before this PR existed — the node
    // assignments are stale for the new PR. However, `state` is still authoritative for *existing* PRs/trunk, so we
    // also use it as a hard boundary to avoid stamping commits that belong to another PR but happen to lack a trailer.
    //
    // Special case: claiming from an existing PR — if the tip commit currently belongs to an existing PR node, we are
    // claiming commits from that PR into a new one. In that case, commits belonging to the `claim_from_pr` should
    // be re-stamped with the new trailer.
    let claim_from_pr = state.commit_node.get(&*tip_entry.commit.commit_id).and_then(|&nk| {
        if let Node::Pr(pr_num) = state.nodes.get(nk)? {
            Some(*pr_num)
        } else {
            None
        }
    });

    let parent_map: HashMap<&CommitId<str>, &JjLogEntry> =
        jj_entries.iter().map(|e| (&*e.commit.commit_id, e)).collect();

    let mut stamp_change_ids = Vec::new();
    let mut queue = std::collections::VecDeque::new();
    let mut visited = HashSet::new();
    queue.push_back(&*tip_entry.commit.commit_id);
    while let Some(cid) = queue.pop_front() {
        if !visited.insert(cid) {
            continue;
        }
        let Some(entry) = parent_map.get(cid) else {
            continue;
        };
        if entry.immutable {
            continue;
        }
        // Check if state already assigns this commit to another PR or trunk/root.
        if let Some(&nk) = state.commit_node.get(&*entry.commit.commit_id) {
            match &state.nodes[nk] {
                Node::Pr(pr) if claim_from_pr == Some(*pr) => {} // Claiming: re-stamp.
                Node::Pr(_) | Node::Root | Node::TrunkTip => continue,
                Node::Ambiguous { .. } => {} // May belong to the new PR.
            }
        }
        let existing = jj::parse_pr_trailer(&entry.commit.description);
        if existing.is_some() && existing != claim_from_pr {
            continue; // Has a trailer for a different PR — don't overwrite.
        }
        stamp_change_ids.push(entry.commit.change_id.clone());
        for parent in &entry.commit.parents {
            queue.push_back(&**parent);
        }
    }

    Ok(CreatePlan {
        bookmark: Bookmark(bookmark.to_owned()),
        base,
        title,
        body,
        stamp_change_ids,
    })
}

/// Execute a planned PR creation. Performs side effects: push, create PR, stamp trailers.
pub fn execute_create(plan: &CreatePlan, push_remote: &str) -> Result<()> {
    // Track remote (ignore error — remote bookmark may not exist yet).
    if let Err(e) = jj::bookmark_track(&plan.bookmark, push_remote) {
        tracing::debug!("bookmark track failed (expected if new): {e:#}");
    }

    eprintln!("Pushing {}", crate::style::bookmark(&plan.bookmark));
    jj::git_push_bookmark(&plan.bookmark)?;

    eprintln!(
        "Creating PR: {} ({} → {}) [draft]",
        plan.title,
        crate::style::bookmark(&plan.bookmark),
        crate::style::bookmark(&plan.base),
    );
    let (pr_number, pr_url) = gh::create_pr(&plan.bookmark, &plan.base, &plan.title, &plan.body, true)?;
    eprintln!("Created {}", crate::style::pr_num(pr_number, Some(&pr_url)));

    if !plan.stamp_change_ids.is_empty() {
        for change_id in &plan.stamp_change_ids {
            let rev = change_id.as_revset();
            let desc = jj::read_description(&rev)?;
            let new_desc = jj::set_pr_trailer(&desc, pr_number);
            jj::describe_stdin(&rev, &new_desc)?;
        }
        eprintln!(
            "Stamped {} on {} commit(s)",
            crate::style::pr_num(pr_number, None),
            plan.stamp_change_ids.len(),
        );
    }

    Ok(())
}
