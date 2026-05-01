use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Deref;

use anyhow::Result;
use renderdag::{Ancestor, GraphRowRenderer, Renderer};
use slotmap::{SecondaryMap, SlotMap, SparseSecondaryMap, new_key_type};

use crate::gh::{GhPr, PrNum};
use crate::graph_algorithms;
use crate::jj::{self, CommitId, JjLogEntry};

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

    /// Bookmarks with conflicting targets (user must resolve with `jj bookmark`).
    pub bookmarks_conflicted: BTreeSet<String>,
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

/// Build the repo state from raw jj and GitHub data.
///
/// `jj_entries` must be in reverse topological order (children to parents to `trunk()`).
pub fn build(jj_entries: &[JjLogEntry], prs: &BTreeMap<PrNum, GhPr>, default_branch: &str) -> Result<RepoState> {
    let mut nodes = SlotMap::with_key();
    let root_node = nodes.insert(Node::Root);
    let mut node_preds = SecondaryMap::new();
    node_preds.insert(root_node, Vec::new()); // `root()` has no parent nodes.

    let (node_succs, commit_node, pr_needs_push, node_needs_sync, bookmarks_conflicted) = Default::default();
    let mut repo_state = RepoState {
        nodes,
        root_node,
        node_preds,
        node_succs,
        topo_order: Vec::new(),
        commit_node,
        pr_needs_push,
        node_needs_sync,
        bookmarks_conflicted,
    };

    let (cid_pr_tip, pr_local) = {
        // For each commit_id, Which PR it belongs to, but only for the tip (last commit in the PR).
        let mut cid_pr_tip: HashMap<&CommitId<str>, Vec<PrNum>> = HashMap::new();

        // GH PR head ref name -> GH PR data.
        // TODO(mingwei): handle multiple PRs sharing the same bookmark (e.g. Open + Closed/Merged).
        let head_to_pr: HashMap<&str, &GhPr> = prs.values().map(|pr| (&*pr.head_ref_name, pr)).collect();

        let mut pr_local = BTreeSet::new();

        for jj_entry in jj_entries.iter() {
            for local_bookmark in jj_entry.local_bookmarks.iter() {
                let local_bookmark_name = &*local_bookmark.name;
                if let Some(pr) = head_to_pr.get(local_bookmark_name) {
                    // This is a PR bookmark.
                    if 1 < local_bookmark.target.len() {
                        // Conflict!
                        // TODO(mingwei): handle conflicts! They can be separate nodes.
                        repo_state.bookmarks_conflicted.insert(local_bookmark_name.to_owned());
                    } else {
                        // Note `local_bookmark.target == vec![jj_entry.commit.commit_id]` in the non-conflicted case.
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
    // TODO(mingwei): hardcodes "origin" — breaks with fork-based workflows.
    {
        let mut local_targets: HashMap<&str, &CommitId<str>> = HashMap::new();
        let mut remote_targets: HashMap<&str, &CommitId<str>> = HashMap::new();
        for jj_entry in jj_entries.iter() {
            for bm in &jj_entry.local_bookmarks {
                local_targets.insert(&bm.name, &jj_entry.commit.commit_id);
            }
            for bm in &jj_entry.remote_bookmarks {
                // Only consider the push remote, not @git (local tracking ref).
                if bm.remote.as_deref() == Some("origin") {
                    remote_targets.entry(&bm.name).or_insert(&jj_entry.commit.commit_id);
                }
            }
        }
        for gh_pr in prs.values() {
            let bookmark = &*gh_pr.head_ref_name;
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
        for jj_entry in jj_entries.iter() {
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
            let (node, node_key) = decide(is_trunk_tip, tip_pr_nums, child_nodes, trailer_pr_num, &pr_local);

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
                        repo_state.nodes.insert(new_node)
                    }
                    (Some(updated_node), Some(node_key)) => {
                        tracing::debug!("Assigning commit to existing and updating");
                        repo_state.nodes[node_key] = updated_node;
                        node_key
                    }
                };
                repo_state.commit_node.insert(cid.to_owned(), node_key);
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
    pr_local: &BTreeSet<PrNum>,
) -> (Option<Node>, Option<NodeKey>) {
    if is_trunk_tip {
        return (Some(Node::TrunkTip), None);
    }

    let (mut node, mut node_key) = match tip_pr_nums {
        // If no tip PR nums, inherit from the children.
        [] => {
            if let Some(trailer_pr_num) = trailer_pr_num
                && !pr_local.contains(&trailer_pr_num)
            {
                // Special case: if `trailer_prs` is set, but the PR is not tracked locally, we use it alone.
                // TODO(mingwei): maybe this should only be for merged PRs?
                (Some(Node::Pr(trailer_pr_num)), None)
            } else {
                decide_combine_child_nodes(child_nodes)
            }
        }
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
        (node, node_key) = if let Some(node) = node {
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
    pub fn expected_base(&self, nk: NodeKey, prs: &BTreeMap<PrNum, GhPr>, default_branch: &str) -> String {
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
}

// --- Rendering ---

/// Render the PR DAG as a graph.
pub fn render_show(state: &RepoState, prs: &BTreeMap<PrNum, GhPr>, out: &mut impl std::io::Write) -> Result<()> {
    let mut renderer = GraphRowRenderer::new()
        .output()
        .with_min_row_height(1)
        .build_box_drawing();

    for &node_key in state.topo_order.iter().rev() {
        let parents = state
            .node_preds
            .get(node_key)
            .unwrap()
            .iter()
            .map(|&dag_node| Ancestor::Parent(dag_node))
            .collect::<Vec<_>>();

        let (message, glyph) = match state.nodes.get(node_key).unwrap() {
            Node::Root => {
                let message = crate::style::root();
                (message, crate::style::GLYPH_IMMUTABLE.to_owned())
            }
            Node::TrunkTip => {
                let message = crate::style::trunk();
                (message, crate::style::GLYPH_IMMUTABLE.to_owned())
            }
            Node::Pr(pr_id) => {
                let gh_pr = prs.get(pr_id).unwrap();
                let sync_indicator = if state.node_needs_sync.contains_key(node_key) {
                    "*"
                } else {
                    ""
                };
                let message = format!(
                    "{}{sync_indicator}  {}  {}\n{}",
                    crate::style::pr_num(pr_id.get(), Some(&gh_pr.url)),
                    crate::style::status(gh_pr.state, gh_pr.is_draft),
                    crate::style::bookmark(&gh_pr.head_ref_name),
                    gh_pr.title,
                );
                (message, crate::style::GLYPH_MUTABLE.to_owned())
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
                let pr_links: Vec<String> = branch_prs
                    .iter()
                    .map(|pr_id| {
                        let url = prs.get(pr_id).map(|p| p.url.as_str());
                        crate::style::pr_num(pr_id.get(), url)
                    })
                    .collect();
                let mut line1 = format!(
                    "{}{sync_indicator} shared between {}",
                    crate::style::warn("ambiguous"),
                    pr_links.join(", "),
                );
                if !trailer_prs.is_empty() {
                    let trailer_strs: Vec<String> = trailer_prs
                        .iter()
                        .map(|pr_id| crate::style::pr_num(pr_id.get(), None))
                        .collect();
                    line1.push_str(&format!(" (trailer: {})", trailer_strs.join(", ")));
                }
                let has_shared = branch_prs.len() > 1;
                let has_trailer_mismatch = !trailer_prs.is_subset(branch_prs);
                let line2 = match (has_shared, has_trailer_mismatch) {
                    (true, true) => {
                        crate::style::dim("(restructure PRs to resolve, and edit commit trailers to fix PR: tags)")
                    }
                    (true, false) => crate::style::dim("(restructure PRs to resolve — stack one on the other)"),
                    (false, true) => crate::style::dim("(edit commit description to fix PR: trailer)"),
                    // Same PR claimed by multiple sources (branch + orphan trailer, or multiple orphan trailers).
                    (false, false) => crate::style::dim("(multiple sources for same PR — abandon stale commits or restructure)"),
                };
                let message = format!("{line1}\n{line2}");
                (message, crate::style::warn(crate::style::GLYPH_WARNING))
            }
        };

        let row = renderer.next_row(node_key, parents, glyph, message);
        write!(out, "{row}")?;
    }

    Ok(())
}

pub fn render_log(
    state: &RepoState,
    prs: &BTreeMap<PrNum, GhPr>,
    jj_entries: &[JjLogEntry],
    show_all: bool,
    out: &mut impl std::io::Write,
) -> Result<()> {
    let known_entries = jj_entries.iter().map(|e| &*e.commit.commit_id).collect::<BTreeSet<_>>();

    let mut renderer = GraphRowRenderer::new()
        .output()
        .with_min_row_height(1)
        .build_box_drawing();

    for jj_entry in jj_entries {
        let cid = &*jj_entry.commit.commit_id;
        let _cid_scope = tracing::info_span!("commit", %cid).entered();

        let node_key = state.commit_node.get(cid).copied();
        if !show_all && node_key.is_none() {
            tracing::trace!("Skipping commit not in any PRs.");
            continue;
        };

        // Build the glyph.
        let glyph = match node_key.map(|nk| state.nodes.get(nk).unwrap()) {
            Some(Node::Root | Node::TrunkTip) => crate::style::GLYPH_IMMUTABLE.to_owned(),
            Some(Node::Ambiguous { .. }) => crate::style::warn(crate::style::GLYPH_WARNING),
            Some(Node::Pr(_)) | None => crate::style::GLYPH_MUTABLE.to_owned(),
        };

        // First line: change_id commit_id [bookmarks] [PR info]
        let mut line1_parts = vec![
            crate::style::change_id(&jj_entry.commit.change_id),
            crate::style::commit_id_short(&cid.to_string()),
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
                        Some(pr) => format!(
                            "{}{sync_indicator} {}",
                            crate::style::pr_num(pr_id.get(), Some(&pr.url)),
                            crate::style::status(pr.state, pr.is_draft),
                        ),
                        None => format!("{}{sync_indicator}", crate::style::pr_num(pr_id.get(), None),),
                    };
                    line1_parts.push(pr_str);
                }
                Node::Ambiguous { branch_prs, .. } => {
                    let sync_indicator = if state.node_needs_sync.contains_key(node_key) {
                        "*"
                    } else {
                        ""
                    };
                    let pr_strs: Vec<String> = branch_prs
                        .iter()
                        .map(|pr_id| crate::style::pr_num(pr_id.get(), None))
                        .collect();
                    line1_parts.push(format!(
                        "{}{sync_indicator} {}{}{}",
                        crate::style::warn("ambiguous"),
                        crate::style::warn("["),
                        pr_strs.join(", "),
                        crate::style::warn("]"),
                    ));
                }
            }
        } else {
            line1_parts.push(crate::style::dim("(no PR)"));
        }

        let line1 = line1_parts.join(" ");

        // Second line: description.
        let line2 = crate::style::description_first_line(&jj_entry.commit.description);

        let message = format!("{line1}\n{line2}");

        let row = renderer.next_row(
            Some(cid),
            jj_entry
                .commit
                .parents
                .iter()
                .map(|cid| Ancestor::Parent(known_entries.contains(&**cid).then_some(&**cid)))
                .collect(),
            glyph,
            message,
        );
        write!(out, "{row}")?;
    }

    // Print root.
    let row = renderer.next_row(
        None,
        Vec::new(),
        crate::style::GLYPH_IMMUTABLE.to_owned(),
        crate::style::root(),
    );
    write!(out, "{row}")?;

    Ok(())
}

// --- Sync ---

use std::fmt;

use crate::gh;

#[derive(Debug)]
pub enum SyncAction {
    /// Stamp a missing `PR: #N` trailer on a commit.
    StampTrailer { change_id: String, pr: PrNum },
    /// Rebase children of a merged PR onto trunk.
    RebaseChildren { tip_commit_id: String, pr: PrNum },
    /// Abandon commits of a merged PR.
    AbandonMerged { tip_commit_id: String, pr: PrNum },
    /// Push bookmarks that differ from remote.
    Push { bookmarks: Vec<(PrNum, String)> },
    /// Update a PR's base branch on GitHub.
    UpdateBase {
        pr: PrNum,
        bookmark: String,
        new_base: String,
    },
}

impl fmt::Display for SyncAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncAction::StampTrailer { change_id, pr } => {
                let short = &change_id[..12.min(change_id.len())];
                write!(f, "stamp {pr} trailer on {short}")
            }
            SyncAction::RebaseChildren { pr, .. } => {
                write!(f, "rebase children of {pr} onto trunk()")
            }
            SyncAction::AbandonMerged { pr, .. } => {
                write!(f, "abandon merged {pr}")
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

/// Plan sync actions. Returns Err if blocking issues exist.
pub fn plan_sync(
    state: &RepoState,
    prs: &BTreeMap<PrNum, GhPr>,
    jj_entries: &[JjLogEntry],
    default_branch: &str,
) -> Result<Vec<SyncAction>> {
    // Block on conflicted bookmarks.
    if !state.bookmarks_conflicted.is_empty() {
        let names: Vec<_> = state.bookmarks_conflicted.iter().map(|s| s.as_str()).collect();
        anyhow::bail!(
            "Diverged bookmark(s): {}. Resolve with `jj bookmark` before syncing.",
            names.join(", ")
        );
    }

    let mut actions = Vec::new();

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

    // 2 & 3. Rebase children + abandon for merged PRs.
    for &nk in state.topo_order.iter() {
        let Some(Node::Pr(pr_num)) = state.nodes.get(nk) else {
            continue;
        };
        let Some(gh_pr) = prs.get(pr_num) else { continue };
        if gh_pr.state != gh::PrState::Merged {
            continue;
        }
        // Find the tip commit (first in jj_entries, which is reverse topo order) for this node.
        let tip_commit_id = jj_entries
            .iter()
            .filter(|e| state.commit_node.get(&*e.commit.commit_id) == Some(&nk))
            .map(|e| e.commit.commit_id.0.clone())
            .next();
        let Some(tip_commit_id) = tip_commit_id else { continue };
        let has_children = state.node_succs.get(nk).is_some_and(|succs| !succs.is_empty());
        if has_children {
            actions.push(SyncAction::RebaseChildren {
                tip_commit_id: tip_commit_id.clone(),
                pr: *pr_num,
            });
        }
        actions.push(SyncAction::AbandonMerged {
            tip_commit_id,
            pr: *pr_num,
        });
    }

    // 4. Push — collect all PR bookmarks that need pushing.
    {
        let mut push_bookmarks = Vec::new();
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

    Ok(actions)
}

/// Execute planned sync actions.
pub fn execute_sync(actions: &[SyncAction]) -> Result<()> {
    for action in actions {
        match action {
            SyncAction::StampTrailer { change_id, pr } => {
                eprintln!(
                    "Stamping {} on {}",
                    crate::style::pr_num(pr.get(), None),
                    crate::style::change_id(change_id),
                );
                let desc = jj::read_description(change_id)?;
                let new_desc = jj::set_pr_trailer(&desc, *pr);
                jj::describe_stdin(change_id, &new_desc)?;
            }
            SyncAction::RebaseChildren { tip_commit_id, pr } => {
                eprintln!(
                    "Rebasing children of {} onto trunk()",
                    crate::style::pr_num(pr.get(), None),
                );
                jj::rebase(&format!("commit_id({tip_commit_id})+"), "trunk()")?;
            }
            SyncAction::AbandonMerged { tip_commit_id, pr } => {
                eprintln!("Abandoning merged {}", crate::style::pr_num(pr.get(), None),);
                let revset = format!("trunk()..commit_id({tip_commit_id})");
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
                let refs: Vec<&str> = bookmarks.iter().map(|(_, s)| s.as_str()).collect();
                jj::git_push_bookmarks(&refs)?;
            }
            SyncAction::UpdateBase { pr, bookmark, new_base } => {
                eprintln!(
                    "Updating {} ({}) base -> {}",
                    crate::style::pr_num(pr.get(), None),
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
    prs: &BTreeMap<PrNum, GhPr>,
    jj_entries: &[JjLogEntry],
    bookmark: &str,
    default_branch: &str,
) -> String {
    // Find the tip commit for this bookmark.
    let tip_cid = jj_entries.iter().find_map(|e| {
        e.local_bookmarks
            .iter()
            .any(|bm| bm.name == bookmark)
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

/// Create a new PR for an existing bookmark.
pub fn cmd_create(
    state: &RepoState,
    prs: &BTreeMap<PrNum, GhPr>,
    jj_entries: &[JjLogEntry],
    default_branch: &str,
    bookmark: &str,
    title: Option<&str>,
    body: Option<&str>,
) -> Result<()> {
    // Verify bookmark exists.
    let tip_entry = jj_entries
        .iter()
        .find(|e| e.local_bookmarks.iter().any(|bm| bm.name == bookmark))
        .with_context(|| {
            format!(
                "bookmark '{}' not found — create it with `jj bookmark create {}`",
                bookmark, bookmark
            )
        })?;

    // Verify no PR already exists for this bookmark.
    if let Some(existing) = prs.values().find(|pr| pr.head_ref_name == bookmark) {
        anyhow::bail!(
            "bookmark '{}' already has {} — use `jj-pr sync` to update it",
            bookmark,
            existing.number,
        );
    }

    // Determine base branch.
    let base = find_base_branch(state, prs, jj_entries, bookmark, default_branch);

    // Determine title.
    let title = title.map(|s| s.to_owned()).unwrap_or_else(|| {
        tip_entry
            .commit
            .description
            .lines()
            .next()
            .unwrap_or("untitled")
            .to_owned()
    });
    let default_body;
    let body = match body {
        Some(b) => b,
        None => {
            // Default body: description body (everything after the first line).
            default_body = tip_entry
                .commit
                .description
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n");
            default_body.trim()
        }
    };

    // Track remote (ignore error — remote bookmark may not exist yet).
    // TODO(mingwei): hardcodes "origin" — breaks with fork-based workflows.
    if let Err(e) = jj::bookmark_track(bookmark, "origin") {
        tracing::debug!("bookmark track failed (expected if new): {e:#}");
    }

    // Push.
    eprintln!("Pushing {}", crate::style::bookmark(bookmark));
    jj::git_push_bookmark(bookmark)?;

    // Create PR.
    eprintln!(
        "Creating PR: {} ({} → {}) [draft]",
        title,
        crate::style::bookmark(bookmark),
        crate::style::bookmark(&base),
    );
    let (pr_number, pr_url) = gh::create_pr(bookmark, &base, &title, body, true)?;
    eprintln!("Created {}", crate::style::pr_num(pr_number.get(), Some(&pr_url)));

    // Stamp trailers on owned commits.
    let mut stamped = 0;
    for jj_entry in jj_entries {
        let cid = &*jj_entry.commit.commit_id;
        let Some(&nk) = state.commit_node.get(cid) else {
            continue;
        };
        let tip_nk = state.commit_node.get(&*tip_entry.commit.commit_id);
        if Some(&nk) != tip_nk {
            continue;
        }
        if jj::parse_pr_trailer(&jj_entry.commit.description) != Some(pr_number) {
            let new_desc = jj::set_pr_trailer(&jj_entry.commit.description, pr_number);
            jj::describe_stdin(&jj_entry.commit.change_id, &new_desc)?;
            stamped += 1;
        }
    }
    if stamped > 0 {
        eprintln!(
            "Stamped {} on {} commit(s)",
            crate::style::pr_num(pr_number.get(), None),
            stamped
        );
    }

    Ok(())
}
