use super::InputData;
use crate::gh::{GhPr, PrNum};
use crate::jj::{CommitId, JjBookmark, JjCommit, JjLogEntry, JjRemoteBookmark};
use crate::pr_dag;

fn render_show(input: &InputData) -> String {
    crate::style::set_force_color(true);
    let prs = input.prs_map();
    let state = pr_dag::build(&input.jj_entries, &prs, &input.default_branch).unwrap();
    let mut buf = Vec::new();
    pr_dag::render_show(&state, &prs, &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn render_log(input: &InputData, show_all: bool) -> String {
    crate::style::set_force_color(true);
    let prs = input.prs_map();
    let state = pr_dag::build(&input.jj_entries, &prs, &input.default_branch).unwrap();
    let mut buf = Vec::new();
    pr_dag::render_log(&state, &prs, &input.jj_entries, show_all, &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn plan_sync(input: &InputData) -> String {
    let prs = input.prs_map();
    let state = pr_dag::build(&input.jj_entries, &prs, &input.default_branch).unwrap();
    match pr_dag::plan_sync(&state, &prs, &input.jj_entries, &input.default_branch) {
        Ok(actions) => actions.iter().map(|a| a.to_string()).collect::<Vec<_>>().join("\n"),
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
        insta::assert_snapshot!("show", render_show(&f));
        insta::assert_snapshot!("log", render_log(&f, false));
        insta::assert_snapshot!("sync", plan_sync(&f));
    });
}

// --- Fixture helpers ---

fn entry(cid: &str, chid: &str, parents: &[&str], desc: &str, bookmarks: &[&str], is_trunk_tip: bool) -> JjLogEntry {
    JjLogEntry {
        commit: JjCommit {
            commit_id: CommitId(cid.to_owned()),
            change_id: chid.to_owned(),
            parents: parents.iter().map(|s| CommitId(s.to_string())).collect(),
            description: desc.to_owned(),
        },
        local_bookmarks: bookmarks
            .iter()
            .map(|name| JjBookmark {
                name: name.to_string(),
                target: vec![Some(CommitId(cid.to_owned()))],
            })
            .collect(),
        remote_bookmarks: vec![],
        immutable: is_trunk_tip,
        is_trunk_tip,
        empty: false,
    }
}

fn with_remote(mut e: JjLogEntry, name: &str) -> JjLogEntry {
    e.remote_bookmarks.push(JjRemoteBookmark {
        name: name.to_owned(),
        remote: Some("origin".to_owned()),
        target: vec![Some(e.commit.commit_id.clone())],
    });
    e
}

fn with_empty(mut e: JjLogEntry) -> JjLogEntry {
    e.empty = true;
    e
}

fn with_git_remote(mut e: JjLogEntry, name: &str) -> JjLogEntry {
    e.remote_bookmarks.push(JjRemoteBookmark {
        name: name.to_owned(),
        remote: Some("git".to_owned()),
        target: vec![Some(e.commit.commit_id.clone())],
    });
    e
}

fn gh_pr(number: u64, head: &str, base: &str) -> GhPr {
    use crate::gh::PrState;
    GhPr {
        number: PrNum::new(number).unwrap(),
        head_ref_name: head.to_owned(),
        base_ref_name: base.to_owned(),
        state: PrState::Open,
        is_draft: true,
        url: format!("https://github.com/test/repo/pull/{number}"),
        title: format!("PR #{number}"),
    }
}

fn gh_pr_merged(number: u64, head: &str, base: &str) -> GhPr {
    use crate::gh::PrState;
    GhPr {
        number: PrNum::new(number).unwrap(),
        head_ref_name: head.to_owned(),
        base_ref_name: base.to_owned(),
        state: PrState::Merged,
        is_draft: false,
        url: format!("https://github.com/test/repo/pull/{number}"),
        title: format!("PR #{number}"),
    }
}

fn gh_pr_closed(number: u64, head: &str, base: &str) -> GhPr {
    use crate::gh::PrState;
    GhPr {
        number: PrNum::new(number).unwrap(),
        head_ref_name: head.to_owned(),
        base_ref_name: base.to_owned(),
        state: PrState::Closed,
        is_draft: false,
        url: format!("https://github.com/test/repo/pull/{number}"),
        title: format!("PR #{number}"),
    }
}

fn fixture(entries: Vec<JjLogEntry>, prs: Vec<GhPr>) -> InputData {
    InputData {
        jj_entries: entries,
        prs,
        default_branch: "main".to_owned(),
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
    );
    insta::assert_snapshot!("single_pr_show", render_show(&f));
    insta::assert_snapshot!("single_pr_log", render_log(&f, false));
    insta::assert_snapshot!("single_pr_sync", plan_sync(&f));
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
    );
    insta::assert_snapshot!("stacked_prs_show", render_show(&f));
    insta::assert_snapshot!("stacked_prs_log", render_log(&f, false));
    insta::assert_snapshot!("stacked_prs_sync", plan_sync(&f));
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
    );
    insta::assert_snapshot!("diamond_ambiguous_show", render_show(&f));
    insta::assert_snapshot!("diamond_ambiguous_log", render_log(&f, true));
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
    );
    insta::assert_snapshot!("merged_parent_show", render_show(&f));
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
    );
    insta::assert_snapshot!("needs_push_show", render_show(&f));
    insta::assert_snapshot!("needs_push_sync", plan_sync(&f));
}

#[test]
fn needs_push_git_remote_not_origin() {
    // Bookmark moved locally — @git matches local but @origin is absent.
    // Should still detect push needed.
    let f = fixture(
        vec![
            with_git_remote(
                entry("c2", "ch2", &["c1"], "update\n\nPR: #1\n", &["feat"], false),
                "feat",
            ),
            entry("c1", "ch1", &["trunk"], "feat\n\nPR: #1\n", &[], false),
            entry("trunk", "chtrunk", &[], "trunk\n", &["main"], true),
        ],
        vec![gh_pr(1, "feat", "main")],
    );
    insta::assert_snapshot!("needs_push_git_not_origin_sync", plan_sync(&f));
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
    );
    insta::assert_snapshot!("base_mismatch_show", render_show(&f));
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
    );
    insta::assert_snapshot!("merge_child_show", render_show(&f));
    insta::assert_snapshot!("merge_child_log", render_log(&f, false));
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
    );
    insta::assert_snapshot!("empty_and_no_description_log", render_log(&f, false));
}
