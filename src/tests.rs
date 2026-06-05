use std::collections::{BTreeMap, BTreeSet};

use super::InputData;
use crate::gh::{CheckStatus, GhPr, PrNum, PrStatus, ReviewDecision};
use crate::jj::{JjBookmark, JjCommit, JjLogEntry, JjRemoteBookmark};
use crate::pr_dag;
use crate::types::{Bookmark, ChangeId, CommitId};

fn render_show(input: &InputData, show_all: bool, reversed: bool) -> String {
    render_show_with_statuses(input, &BTreeMap::new(), show_all, reversed)
}

fn render_show_with_statuses(
    input: &InputData,
    pr_statuses: &BTreeMap<PrNum, PrStatus>,
    show_all: bool,
    reversed: bool,
) -> String {
    crate::style::set_force_color(true);
    let prs = input.prs_map();
    let state = pr_dag::build(
        &input.jj_entries,
        &prs,
        &input.default_branch,
        input.tracked_bookmarks.as_ref(),
    )
    .unwrap();
    let mut buf = Vec::new();
    pr_dag::render_show(&state, &prs, pr_statuses, show_all, reversed, &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn render_log(input: &InputData, show_all: bool, reversed: bool) -> String {
    crate::style::set_force_color(true);
    let prs = input.prs_map();
    let state = pr_dag::build(
        &input.jj_entries,
        &prs,
        &input.default_branch,
        input.tracked_bookmarks.as_ref(),
    )
    .unwrap();
    let pr_statuses = BTreeMap::<PrNum, PrStatus>::new();
    let mut buf = Vec::new();
    pr_dag::render_log(
        &state,
        &prs,
        &pr_statuses,
        &input.jj_entries,
        show_all,
        reversed,
        &mut buf,
    )
    .unwrap();
    String::from_utf8(buf).unwrap()
}

fn plan_sync(input: &InputData) -> String {
    let prs = input.prs_map();
    let state = pr_dag::build(
        &input.jj_entries,
        &prs,
        &input.default_branch,
        input.tracked_bookmarks.as_ref(),
    )
    .unwrap();
    match pr_dag::plan_sync(
        &state,
        &prs,
        &input.jj_entries,
        &input.default_branch,
        input.existing_merge_commits.as_ref(),
    ) {
        Ok(plan) => {
            let mut lines: Vec<String> = plan.warnings.clone();
            lines.extend(plan.actions.iter().map(|a| a.to_string()));
            lines.join("\n")
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

fn plan_create(input: &InputData, bookmark: &str) -> String {
    let prs = input.prs_map();
    let state = pr_dag::build(
        &input.jj_entries,
        &prs,
        &input.default_branch,
        input.tracked_bookmarks.as_ref(),
    )
    .unwrap();
    match pr_dag::plan_create(
        &state,
        &prs,
        &input.jj_entries,
        &input.default_branch,
        bookmark,
        None,
        None,
    ) {
        Ok(plan) => plan.to_string(),
        Err(e) => format!("ERROR: {e}"),
    }
}

// --- File-based fixture tests ---

#[test]
fn fixture_files() {
    insta::glob!("fixtures/*.json.gz", |path| {
        let file = std::fs::File::open(path).unwrap();
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut json = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut json).unwrap();
        let f: InputData = serde_json::from_str(&json).expect("failed to parse fixture JSON");
        insta::assert_snapshot!("show", render_show(&f, false, false));
        insta::assert_snapshot!("show-all", render_show(&f, true, false));
        insta::assert_snapshot!("log", render_log(&f, false, false));
        insta::assert_snapshot!("log-all", render_log(&f, true, false));
        insta::assert_snapshot!("sync", plan_sync(&f));
    });
}

// --- Fixture helpers ---

fn entry(cid: &str, chid: &str, parents: &[&str], desc: &str, bookmarks: &[&str], is_trunk_tip: bool) -> JjLogEntry {
    JjLogEntry {
        commit: JjCommit {
            commit_id: CommitId(cid.to_owned()),
            change_id: ChangeId(chid.to_owned()),
            parents: parents.iter().map(|s| CommitId(s.to_string())).collect(),
            description: desc.to_owned(),
        },
        local_bookmarks: bookmarks
            .iter()
            .map(|name| JjBookmark {
                name: Bookmark(name.to_string()),
                target: vec![Some(CommitId(cid.to_owned()))],
            })
            .collect(),
        remote_bookmarks: vec![],
        immutable: is_trunk_tip,
        is_trunk_tip,
        empty: false,
        is_working_copy: false,
        conflict: false,
    }
}

fn with_remote(mut e: JjLogEntry, name: &str) -> JjLogEntry {
    e.remote_bookmarks.push(JjRemoteBookmark {
        name: Bookmark(name.to_owned()),
        remote: Some("origin".to_owned()),
        target: vec![Some(e.commit.commit_id.clone())],
    });
    e
}

fn with_empty(mut e: JjLogEntry) -> JjLogEntry {
    e.empty = true;
    e
}

fn with_working_copy(mut e: JjLogEntry) -> JjLogEntry {
    e.is_working_copy = true;
    e
}

fn with_git_remote(mut e: JjLogEntry, name: &str) -> JjLogEntry {
    e.remote_bookmarks.push(JjRemoteBookmark {
        name: Bookmark(name.to_owned()),
        remote: Some("git".to_owned()),
        target: vec![Some(e.commit.commit_id.clone())],
    });
    e
}

fn gh_pr(number: u64, head: &str, base: &str) -> GhPr {
    use crate::gh::PrState;
    GhPr {
        number: PrNum::new(number).unwrap(),
        head_ref_name: Bookmark(head.to_owned()),
        base_ref_name: Bookmark(base.to_owned()),
        state: PrState::Open,
        is_draft: true,
        url: format!("https://github.com/test/repo/pull/{number}"),
        title: format!("PR #{number}"),
        merge_commit_oid: None,
    }
}

fn gh_pr_merged(number: u64, head: &str, base: &str) -> GhPr {
    use crate::gh::PrState;
    GhPr {
        number: PrNum::new(number).unwrap(),
        head_ref_name: Bookmark(head.to_owned()),
        base_ref_name: Bookmark(base.to_owned()),
        state: PrState::Merged,
        is_draft: false,
        url: format!("https://github.com/test/repo/pull/{number}"),
        title: format!("PR #{number}"),
        merge_commit_oid: Some(CommitId(format!("merge_commit_{number}"))),
    }
}

fn gh_pr_closed(number: u64, head: &str, base: &str) -> GhPr {
    use crate::gh::PrState;
    GhPr {
        number: PrNum::new(number).unwrap(),
        head_ref_name: Bookmark(head.to_owned()),
        base_ref_name: Bookmark(base.to_owned()),
        state: PrState::Closed,
        is_draft: false,
        url: format!("https://github.com/test/repo/pull/{number}"),
        title: format!("PR #{number}"),
        merge_commit_oid: None,
    }
}

fn fixture(entries: Vec<JjLogEntry>, prs: Vec<GhPr>, tracked_bookmarks: Option<BTreeSet<Bookmark>>) -> InputData {
    InputData {
        jj_entries: entries,
        prs,
        default_branch: Bookmark("main".to_owned()),
        tracked_bookmarks,
        existing_merge_commits: None, // Legacy: all merge commits considered present.
    }
}

// --- Snapshot tests ---

#[test]
fn single_pr() {
    let f = fixture(
        vec![
            with_remote(
                entry("c2", "ch2", &["c1"], "feat\n\nPR: #1\n", &["feat"], false),
                "feat",
            ),
            entry("c1", "ch1", &["c2_parent"], "first\n\nPR: #1\n", &[], false),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat", "main")],
        None,
    );
    insta::assert_snapshot!("single_pr_show", render_show(&f, true, false));
    insta::assert_snapshot!("single_pr_log", render_log(&f, false, false));
    insta::assert_snapshot!("single_pr_sync", plan_sync(&f));
}

#[test]
fn current_change() {
    // @ is on the tip commit of PR #2 in a stack.
    let f = fixture(
        vec![
            with_working_copy(with_remote(
                entry("b1", "chb1", &["a1"], "b\n\nPR: #2\n", &["feat-b"], false),
                "feat-b",
            )),
            with_remote(
                entry("a1", "cha1", &["trunk"], "a\n\nPR: #1\n", &["feat-a"], false),
                "feat-a",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat-a", "main"), gh_pr(2, "feat-b", "feat-a")],
        None,
    );
    insta::assert_snapshot!("current_change_show", render_show(&f, true, false));
    insta::assert_snapshot!("current_change_log", render_log(&f, false, false));
}

#[test]
fn stacked_prs() {
    let f = fixture(
        vec![
            with_remote(
                entry("b1", "chb1", &["a1"], "b\n\nPR: #2\n", &["feat-b"], false),
                "feat-b",
            ),
            with_remote(
                entry("a1", "cha1", &["trunk"], "a\n\nPR: #1\n", &["feat-a"], false),
                "feat-a",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat-a", "main"), gh_pr(2, "feat-b", "feat-a")],
        None,
    );
    insta::assert_snapshot!("stacked_prs_show", render_show(&f, true, false));
    insta::assert_snapshot!("stacked_prs_show_reversed", render_show(&f, true, true));
    insta::assert_snapshot!("stacked_prs_log", render_log(&f, false, false));
    insta::assert_snapshot!("stacked_prs_log_reversed", render_log(&f, false, true));
    insta::assert_snapshot!("stacked_prs_sync", plan_sync(&f));
}

/// Regression test: reversed log with non-PR siblings of trunk should not produce
/// dangling graph edges. The non-PR commits (wip1, wip2) are children of trunk but
/// have no PR association, so they get filtered when show_all=false. In reversed mode,
/// trunk's children_map must not include these filtered commits.
#[test]
fn log_reversed_with_non_pr_siblings() {
    let f = fixture(
        vec![
            entry("wip2", "chwip2", &["trunk"], "unrelated wip 2\n", &[], false),
            entry("wip1", "chwip1", &["trunk"], "unrelated wip 1\n", &[], false),
            with_remote(
                entry("b1", "chb1", &["a1"], "b\n\nPR: #2\n", &["feat-b"], false),
                "feat-b",
            ),
            with_remote(
                entry("a1", "cha1", &["trunk"], "a\n\nPR: #1\n", &["feat-a"], false),
                "feat-a",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat-a", "main"), gh_pr(2, "feat-b", "feat-a")],
        None,
    );
    insta::assert_snapshot!("log_reversed_non_pr_siblings", render_log(&f, false, true));
    insta::assert_snapshot!("log_non_pr_siblings", render_log(&f, false, false));
}

#[test]
fn diamond_ambiguous() {
    let f = fixture(
        vec![
            with_remote(entry("a1", "cha1", &["xyz"], "a\n", &["feat-a"], false), "feat-a"),
            with_remote(entry("b1", "chb1", &["xyz"], "b\n", &["feat-b"], false), "feat-b"),
            entry("xyz", "chxyz", &["trunk"], "shared\n", &[], false),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat-a", "main"), gh_pr(2, "feat-b", "main")],
        None,
    );
    insta::assert_snapshot!("diamond_ambiguous_show", render_show(&f, true, false));
    insta::assert_snapshot!("diamond_ambiguous_log", render_log(&f, true, false));
}

#[test]
fn merged_parent() {
    let f = fixture(
        vec![
            with_remote(
                entry("b1", "chb1", &["a1"], "child\n\nPR: #2\n", &["feat-b"], false),
                "feat-b",
            ),
            entry("a1", "cha1", &["trunk"], "parent\n\nPR: #1\n", &["feat-a"], false),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr_merged(1, "feat-a", "main"), gh_pr(2, "feat-b", "feat-a")],
        None,
    );
    insta::assert_snapshot!("merged_parent_show", render_show(&f, true, false));
    insta::assert_snapshot!("merged_parent_sync", plan_sync(&f));
}

#[test]
fn needs_push() {
    let f = fixture(
        vec![
            entry("c2", "ch2", &["c1"], "update\n\nPR: #1\n", &["feat"], false),
            with_remote(entry("c1", "ch1", &["trunk"], "feat\n\nPR: #1\n", &[], false), "feat"),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat", "main")],
        None,
    );
    insta::assert_snapshot!("needs_push_show", render_show(&f, true, false));
    insta::assert_snapshot!("needs_push_sync", plan_sync(&f));
}

#[test]
fn no_push_when_only_git_remote() {
    // Bookmark has @git but no @origin — not tracked on origin, should NOT push.
    let f = InputData {
        jj_entries: vec![
            with_git_remote(
                entry("c2", "ch2", &["c1"], "update\n\nPR: #1\n", &["feat"], false),
                "feat",
            ),
            entry("c1", "ch1", &["trunk"], "feat\n\nPR: #1\n", &[], false),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        prs: vec![gh_pr(1, "feat", "main")],
        default_branch: Bookmark("main".to_owned()),
        tracked_bookmarks: Some(BTreeSet::new()),
        existing_merge_commits: None, // Not tracked on origin.
    };
    insta::assert_snapshot!("no_push_when_only_git_remote", plan_sync(&f));
}

#[test]
fn needs_push_tracked_but_no_origin_in_revset() {
    // Bookmark is tracked on origin but @origin commit is outside the revset
    // (e.g. after amending). Should still detect push needed.
    let f = InputData {
        jj_entries: vec![
            with_git_remote(
                entry("c2", "ch2", &["c1"], "update\n\nPR: #1\n", &["feat"], false),
                "feat",
            ),
            entry("c1", "ch1", &["trunk"], "feat\n\nPR: #1\n", &[], false),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        prs: vec![gh_pr(1, "feat", "main")],
        default_branch: Bookmark("main".to_owned()),
        tracked_bookmarks: Some([Bookmark("feat".to_owned())].into()), // Tracked on origin.
        existing_merge_commits: None,
    };
    insta::assert_snapshot!("needs_push_tracked_but_no_origin_in_revset", plan_sync(&f));
}

#[test]
fn base_mismatch() {
    let f = fixture(
        vec![
            with_remote(
                entry("b1", "chb1", &["a1"], "b\n\nPR: #2\n", &["feat-b"], false),
                "feat-b",
            ),
            with_remote(
                entry("a1", "cha1", &["trunk"], "a\n\nPR: #1\n", &["feat-a"], false),
                "feat-a",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat-a", "main"), gh_pr(2, "feat-b", "main")], // wrong base for #2
        None,
    );
    insta::assert_snapshot!("base_mismatch_show", render_show(&f, true, false));
    insta::assert_snapshot!("base_mismatch_sync", plan_sync(&f));
}

#[test]
fn closed_pr_base_mismatch_skips_update() {
    // Closed PR #2 stacked on #1 with wrong base — should NOT get a base update.
    let f = fixture(
        vec![
            with_remote(
                entry("b1", "chb1", &["a1"], "b\n\nPR: #2\n", &["feat-b"], false),
                "feat-b",
            ),
            with_remote(
                entry("a1", "cha1", &["trunk"], "a\n\nPR: #1\n", &["feat-a"], false),
                "feat-a",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat-a", "main"), gh_pr_closed(2, "feat-b", "main")], // wrong base, but closed
        None,
    );
    insta::assert_snapshot!("closed_pr_base_mismatch_sync", plan_sync(&f));
}

#[test]
fn nothing_to_sync() {
    let f = fixture(
        vec![
            with_remote(
                entry("c1", "ch1", &["trunk"], "feat\n\nPR: #1\n", &["feat"], false),
                "feat",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat", "main")],
        None,
    );
    insta::assert_snapshot!("nothing_to_sync", plan_sync(&f));
}

#[test]
fn merge_child() {
    // PR #3 merges PR #1 and PR #2.
    let f = fixture(
        vec![
            with_remote(
                entry("c1", "chc1", &["a1", "b1"], "merge\n", &["feat-c"], false),
                "feat-c",
            ),
            with_remote(entry("a1", "cha1", &["trunk"], "a\n", &["feat-a"], false), "feat-a"),
            with_remote(entry("b1", "chb1", &["trunk"], "b\n", &["feat-b"], false), "feat-b"),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![
            gh_pr(1, "feat-a", "main"),
            gh_pr(2, "feat-b", "main"),
            gh_pr(3, "feat-c", "feat-a"),
        ],
        None,
    );
    insta::assert_snapshot!("merge_child_show", render_show(&f, true, false));
    insta::assert_snapshot!("merge_child_log", render_log(&f, false, false));
}

#[test]
fn empty_and_no_description() {
    // Tests all four combinations of empty/non-empty × description/no-description.
    let f = fixture(
        vec![
            with_remote(
                entry("c4", "ch4", &["c3"], "has desc\n\nPR: #1\n", &["feat"], false),
                "feat",
            ),
            with_empty(entry("c3", "ch3", &["c2"], "empty with desc\n\nPR: #1\n", &[], false)),
            entry("c2", "ch2", &["c1"], "\n\nPR: #1\n", &[], false),
            with_empty(entry("c1", "ch1", &["trunk"], "\n\nPR: #1\n", &[], false)),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat", "main")],
        None,
    );
    insta::assert_snapshot!("empty_and_no_description_log", render_log(&f, false, false));
}

#[test]
fn conflicted_bookmark_merged_pr_with_null() {
    // Merged PR whose local bookmark is conflicted: [local_commit, base_commit, null].
    // This happens when the remote branch was deleted (squash-merge) while the local
    // side was rebased. Should auto-resolve by deleting the bookmark and abandoning.
    let mut tip = entry(
        "local",
        "ch_local",
        &["trunk"],
        "my change\n\nPR: #1\n",
        &["feat"],
        false,
    );
    // Set up conflicted target: [local_commit, base_commit, null]
    tip.local_bookmarks[0].target = vec![
        Some(CommitId("local".to_owned())),
        Some(CommitId("base".to_owned())),
        None,
    ];
    let f = fixture(
        vec![tip, entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true)],
        vec![gh_pr_merged(1, "feat", "main")],
        None,
    );
    insta::assert_snapshot!("conflicted_merged_null_show", render_show(&f, true, false));
    insta::assert_snapshot!("conflicted_merged_null_sync", plan_sync(&f));
}

#[test]
fn conflicted_bookmark_open_pr_blocks_sync() {
    // Open PR with conflicted bookmark should still block sync.
    let mut tip = entry(
        "local",
        "ch_local",
        &["trunk"],
        "my change\n\nPR: #1\n",
        &["feat"],
        false,
    );
    tip.local_bookmarks[0].target = vec![
        Some(CommitId("local".to_owned())),
        Some(CommitId("base".to_owned())),
        None,
    ];
    let f = fixture(
        vec![tip, entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true)],
        vec![gh_pr(1, "feat", "main")],
        None,
    );
    insta::assert_snapshot!("conflicted_open_show", render_show(&f, true, false));
    insta::assert_snapshot!("conflicted_open_log", render_log(&f, false, false));
    insta::assert_snapshot!("conflicted_open_blocks_sync", plan_sync(&f));
}

#[test]
fn conflicted_working_copy() {
    // Conflicted commit that is also the working copy should show red @.
    let mut tip = entry(
        "local",
        "ch_local",
        &["trunk"],
        "my change\n\nPR: #1\n",
        &["feat"],
        false,
    );
    tip.conflict = true;
    tip.is_working_copy = true;
    let f = fixture(
        vec![tip, entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true)],
        vec![gh_pr(1, "feat", "main")],
        None,
    );
    insta::assert_snapshot!("conflicted_working_copy_show", render_show(&f, true, false));
    insta::assert_snapshot!("conflicted_working_copy_log", render_log(&f, false, false));
}

#[test]
fn conflicted_bookmark_no_null_blocks_sync() {
    // Conflicted bookmark without null (both sides point to commits) should block sync
    // even for merged PRs.
    let mut tip = entry(
        "local",
        "ch_local",
        &["trunk"],
        "my change\n\nPR: #1\n",
        &["feat"],
        false,
    );
    tip.local_bookmarks[0].target = vec![
        Some(CommitId("local".to_owned())),
        Some(CommitId("base".to_owned())),
        Some(CommitId("remote".to_owned())),
    ];
    let f = fixture(
        vec![tip, entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true)],
        vec![gh_pr_merged(1, "feat", "main")],
        None,
    );
    insta::assert_snapshot!("conflicted_no_null_blocks_sync", plan_sync(&f));
}

#[test]
fn ci_review_status_indicators() {
    let f = fixture(
        vec![
            with_remote(
                entry("b1", "chb1", &["a1"], "b\n\nPR: #2\n", &["feat-b"], false),
                "feat-b",
            ),
            with_remote(
                entry("a1", "cha1", &["trunk"], "a\n\nPR: #1\n", &["feat-a"], false),
                "feat-a",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat-a", "main"), gh_pr(2, "feat-b", "feat-a")],
        None,
    );
    let statuses = BTreeMap::from([
        (
            PrNum::new(1).unwrap(),
            PrStatus {
                review_decision: Some(ReviewDecision::Approved),
                checks_status: Some(CheckStatus::Pass),
            },
        ),
        (
            PrNum::new(2).unwrap(),
            PrStatus {
                review_decision: Some(ReviewDecision::ChangesRequested),
                checks_status: Some(CheckStatus::Fail),
            },
        ),
    ]);
    insta::assert_snapshot!(
        "ci_review_status_show",
        render_show_with_statuses(&f, &statuses, true, false)
    );
}

#[test]
fn create_new_pr() {
    // Bookmark "feat" with two unstamped commits, no existing PR.
    let f = fixture(
        vec![
            entry("c2", "ch2", &["c1"], "second commit\n", &["feat"], false),
            entry("c1", "ch1", &["trunk"], "first commit\n", &[], false),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![],
        None,
    );
    insta::assert_snapshot!("create_new_pr", plan_create(&f, "feat"));
}

#[test]
fn create_stacked_pr() {
    // Bookmark "feat-b" stacked on existing PR #1 ("feat-a").
    let f = fixture(
        vec![
            entry("b1", "chb1", &["a1"], "child\n", &["feat-b"], false),
            with_remote(
                entry("a1", "cha1", &["trunk"], "parent\n\nPR: #1\n", &["feat-a"], false),
                "feat-a",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat-a", "main")],
        None,
    );
    insta::assert_snapshot!("create_stacked_pr", plan_create(&f, "feat-b"));
}

#[test]
fn create_already_exists() {
    // Bookmark "feat" already has PR #1 — should error.
    let f = fixture(
        vec![
            with_remote(
                entry("c1", "ch1", &["trunk"], "feat\n\nPR: #1\n", &["feat"], false),
                "feat",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat", "main")],
        None,
    );
    insta::assert_snapshot!("create_already_exists", plan_create(&f, "feat"));
}

#[test]
fn create_conflicted_bookmark_rejected() {
    // Conflicted bookmark should be rejected by plan_create.
    let mut tip = entry("c1", "ch1", &["trunk"], "feat\n", &["feat"], false);
    tip.local_bookmarks[0].target = vec![
        Some(CommitId("c1".to_owned())),
        Some(CommitId("other".to_owned())),
        Some(CommitId("remote".to_owned())),
    ];
    let f = fixture(
        vec![tip, entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true)],
        vec![],
        None,
    );
    insta::assert_snapshot!("create_conflicted_bookmark", plan_create(&f, "feat"));
}

#[test]
fn bookmark_name_collision_no_remote() {
    // User has a local bookmark "fix-typo" that coincidentally matches someone else's PR.
    // No remote tracking, no trailer. Should NOT plan a push (not our PR).
    let f = InputData {
        jj_entries: vec![
            entry("c1", "ch1", &["trunk"], "my unrelated work\n", &["fix-typo"], false),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        prs: vec![gh_pr(42, "fix-typo", "main")],
        default_branch: Bookmark("main".to_owned()),
        tracked_bookmarks: Some(BTreeSet::new()),
        existing_merge_commits: None, // No bookmarks tracked.
    };
    insta::assert_snapshot!("bookmark_name_collision_no_remote", plan_sync(&f));
}

#[test]
fn stale_trunk_skips_abandon() {
    // PR #1 is merged but its merge commit is NOT in the local repo (trunk stale).
    // Should warn and skip the abandon, not prompt for action.
    let f = InputData {
        jj_entries: vec![
            with_remote(
                entry("c1", "ch1", &["trunk"], "feat\n\nPR: #1\n", &["feat"], false),
                "feat",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        prs: vec![gh_pr_merged(1, "feat", "main")],
        default_branch: Bookmark("main".to_owned()),
        tracked_bookmarks: None,
        existing_merge_commits: Some(std::collections::HashSet::new()), // Empty = nothing fetched.
    };
    insta::assert_snapshot!("stale_trunk_skips_abandon", plan_sync(&f));
}

#[test]
fn closed_pr_hidden_by_default() {
    // Closed leaf PR #2 should be hidden; open PR #1 stays visible.
    let f = fixture(
        vec![
            with_remote(
                entry("b1", "chb1", &["a1"], "b\n\nPR: #2\n", &["feat-b"], false),
                "feat-b",
            ),
            with_remote(
                entry("a1", "cha1", &["trunk"], "a\n\nPR: #1\n", &["feat-a"], false),
                "feat-a",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat-a", "main"), gh_pr_closed(2, "feat-b", "feat-a")],
        None,
    );
    insta::assert_snapshot!("closed_pr_hidden_show", render_show(&f, false, false));
    insta::assert_snapshot!("closed_pr_hidden_log", render_log(&f, false, false));
    // With --all, closed PR is visible.
    insta::assert_snapshot!("closed_pr_visible_show", render_show(&f, true, false));
    insta::assert_snapshot!("closed_pr_visible_log", render_log(&f, true, false));
}

#[test]
fn closed_pr_with_open_child_stays_visible() {
    // Closed PR #1 has open child PR #2 — should NOT be hidden.
    let f = fixture(
        vec![
            with_remote(
                entry("b1", "chb1", &["a1"], "b\n\nPR: #2\n", &["feat-b"], false),
                "feat-b",
            ),
            with_remote(
                entry("a1", "cha1", &["trunk"], "a\n\nPR: #1\n", &["feat-a"], false),
                "feat-a",
            ),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr_closed(1, "feat-a", "main"), gh_pr(2, "feat-b", "feat-a")],
        None,
    );
    insta::assert_snapshot!("closed_pr_with_open_child_show", render_show(&f, false, false));
    insta::assert_snapshot!("closed_pr_with_open_child_log", render_log(&f, false, false));
}
