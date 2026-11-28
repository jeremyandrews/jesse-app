//! A Studio run's unsaved work is saved before any removal, and continue resumes from it
//! (DJ37). The "origin" is always a local bare repository under the scratch folder.

use super::*;

/// The scratch "GitHub" bare repository of `o/n`.
fn origin(s: &Scratch) -> PathBuf {
    s.base.join("remote/o/n.git")
}

/// A worktree of `o/n` on a new branch, as a run's first round makes it.
fn worktree(s: &Scratch, st: &HostRunSettings, id: &str, branch: &str) -> PathBuf {
    let wt = st.run_dir(id).join(WORKTREE_DIR);
    git::add_worktree(&st.root, "o/n", &st.repo_url("o/n"), &wt, branch).unwrap();
    let _ = s;
    wt
}

/// A commit of `file` (never pushed) and an uncommitted, untracked `loose` beside it: the
/// commit's sha.
fn unpushed_commit_and_loose_file(wt: &Path) -> String {
    std::fs::write(wt.join("done.txt"), "committed\n").unwrap();
    sh(wt, &["add", "done.txt"]);
    sh(wt, &["commit", "-q", "-m", "part one"]);
    std::fs::write(wt.join("loose.txt"), "uncommitted\n").unwrap();
    sh(wt, &["rev-parse", "HEAD"]).trim().to_string()
}

fn has_ref(bare: &Path, r: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(bare)
        .args(["rev-parse", "--verify", "--quiet", r])
        .output()
        .unwrap()
        .status
        .success()
}

fn break_origin(st: &HostRunSettings) {
    let bare = git::bare_path(&st.root, "o/n");
    sh(
        &bare,
        &[
            "remote",
            "set-url",
            "origin",
            "/nonexistent/jesse-dj37/n.git",
        ],
    );
}

#[test]
fn removal_saves_an_unpushed_commit_and_an_uncommitted_file_to_a_wip_ref_first() {
    let s = Scratch::new();
    remote_fixture(&s);
    let st = settings(&s);
    let branch = "r1-work";
    let wt = worktree(&s, &st, "R1", branch);
    let commit = unpushed_commit_and_loose_file(&wt);
    let bare = git::bare_path(&st.root, "o/n");
    assert!(git::unsaved_work(&bare, &wt, branch).unwrap().is_some());

    let saved = git::remove_worktree(&st.root, "o/n", &wt, branch, "R1")
        .unwrap()
        .expect("the work was saved");
    assert!(!wt.exists());
    assert_eq!(saved.name, format!("jesse/{branch}-wip"));
    let wip = format!("refs/heads/jesse/{branch}-wip");
    // Both the commit and the uncommitted file are reachable from origin's -wip ref.
    sh(&origin(&s), &["merge-base", "--is-ancestor", &commit, &wip]);
    assert_eq!(
        sh(&origin(&s), &["show", &format!("{wip}:loose.txt")]),
        "uncommitted\n"
    );
    assert_eq!(
        sh(&origin(&s), &["show", &format!("{wip}:done.txt")]),
        "committed\n"
    );
    assert!(sh(&origin(&s), &["log", "-1", "--format=%s", &wip]).starts_with("[R1] wip snapshot "));
    // The branch went only because its commits are saved.
    assert!(!has_branch(&bare, branch));
    assert!(has_ref(&bare, &saved.local));
}

#[test]
fn removal_whose_push_fails_removes_nothing_and_names_the_failure() {
    let s = Scratch::new();
    remote_fixture(&s);
    let st = settings(&s);
    let branch = "r2-work";
    let wt = worktree(&s, &st, "R2", branch);
    let commit = unpushed_commit_and_loose_file(&wt);
    break_origin(&st);

    let e = git::remove_worktree(&st.root, "o/n", &wt, branch, "R2").unwrap_err();
    assert!(e.contains("was not removed"), "{e}");
    assert!(
        e.contains(&format!("the push to jesse/{branch}-wip failed")),
        "{e}"
    );
    assert!(e.contains("/nonexistent/jesse-dj37"), "{e}");
    // Nothing went: the file, the commit, the branch.
    assert_eq!(
        std::fs::read_to_string(wt.join("loose.txt")).unwrap(),
        "uncommitted\n"
    );
    let bare = git::bare_path(&st.root, "o/n");
    assert!(has_branch(&bare, branch));
    assert_eq!(sh(&wt, &["rev-parse", "HEAD"]).trim(), commit);
    // And nothing claims it is saved.
    assert!(git::unsaved_work(&bare, &wt, branch).unwrap().is_some());
}

#[tokio::test]
async fn the_space_pass_keeps_a_worktree_it_cannot_save_and_removes_a_pushed_one() {
    let s = Scratch::new();
    remote_fixture(&s);
    let mut st = settings(&s);
    st.max_per_repo = 2;
    let r = rig_with(&s, st.clone());
    r.svc.start(free("unsaved")).await.unwrap();
    r.svc.start(free("pushed")).await.unwrap();
    let (du, dp) = (st.run_dir("s-unsaved"), st.run_dir("s-pushed"));
    unpushed_commit_and_loose_file(&du.join(WORKTREE_DIR));
    let bp = r.svc.get("s-pushed").unwrap().branch;
    sh(&dp.join(WORKTREE_DIR), &["push", "-q", "origin", &bp]);
    // The network goes before either stops, so not even the save at the stop gets out.
    break_origin(&st);
    finish_round(&r, "s-unsaved", 0, Some("ok\n"), "");
    finish_round(&r, "s-pushed", 0, Some("ok\n"), "");
    r.svc.pass(now_ms()).await;
    assert!(r.svc.get("s-unsaved").unwrap().saved_refs.is_empty());

    // Free space under the floor whatever the disk holds.
    let mut tight = st.clone();
    tight.min_free_bytes = u64::MAX;
    let t = rig_with(&s, tight);
    let fx = t.svc.space_pass(now_ms());
    assert!(du.join(WORKTREE_DIR).join("loose.txt").is_file(), "kept");
    assert!(!dp.join(WORKTREE_DIR).exists(), "the pushed one goes");
    assert_eq!(
        fx,
        vec![Effect::Push {
            title: "Disk low".into(),
            body: "Disk low: 1 run worktrees hold unsaved work and were kept: s-unsaved".into(),
            run_id: "s-unsaved".into(),
        }]
    );
}

#[tokio::test]
async fn an_agent_made_sibling_worktree_is_saved_before_it_is_removed() {
    let s = Scratch::new();
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(free("multi")).await.unwrap();
    let dir = r.svc.settings.run_dir("s-multi");
    let wt4 = dir.join("wt4");
    // The agent makes a worktree of its own for the next pull request.
    sh(
        &dir.join(WORKTREE_DIR),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "part4",
            &wt4.to_string_lossy(),
        ],
    );
    finish_round(&r, "s-multi", 0, Some("ok\n"), "");
    r.svc.pass(now_ms()).await;
    std::fs::write(wt4.join("half.txt"), "pr 4, half done\n").unwrap();

    // Past keep_days: both worktrees go, wt4 only once its work is on origin.
    r.svc.cleanup(now_ms() + 8 * DAY_MS);
    assert!(!dir.join(WORKTREE_DIR).exists());
    assert!(!wt4.exists());
    assert_eq!(
        sh(
            &origin(&s),
            &["show", "refs/heads/jesse/part4-wip:half.txt"]
        ),
        "pr 4, half done\n"
    );
    let rec = r.svc.get("s-multi").unwrap();
    assert_eq!(rec.cleanup.saved_ref.as_deref(), Some("jesse/part4-wip"));
    assert!(rec
        .saved_refs
        .iter()
        .any(|x| x.name == "jesse/part4-wip" && x.worktree == "wt4"));

    // With no way to save it, a sibling stays.
    let s = Scratch::new();
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(free("multi")).await.unwrap();
    let dir = r.svc.settings.run_dir("s-multi");
    let wt4 = dir.join("wt4");
    sh(
        &dir.join(WORKTREE_DIR),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "part4",
            &wt4.to_string_lossy(),
        ],
    );
    finish_round(&r, "s-multi", 0, Some("ok\n"), "");
    r.svc.pass(now_ms()).await;
    std::fs::write(wt4.join("half.txt"), "pr 4, half done\n").unwrap();
    break_origin(&r.svc.settings);
    r.svc.cleanup(now_ms() + 8 * DAY_MS);
    assert!(!dir.join(WORKTREE_DIR).exists(), "nothing unsaved there");
    assert_eq!(
        std::fs::read_to_string(wt4.join("half.txt")).unwrap(),
        "pr 4, half done\n"
    );
}

#[tokio::test]
async fn a_run_that_stops_reported_with_unpushed_work_saves_it_and_says_so() {
    let s = Scratch::new();
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(free("stops")).await.unwrap();
    let dir = r.svc.settings.run_dir("s-stops");
    let wt = dir.join(WORKTREE_DIR);
    let commit = unpushed_commit_and_loose_file(&wt);
    finish_round(&r, "s-stops", 0, Some("Done for now.\n"), "");
    r.svc.pass(now_ms()).await;
    let rec = r.svc.get("s-stops").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    assert_eq!(rec.saved_refs.len(), 1, "{:?}", rec.saved_refs);
    let saved = &rec.saved_refs[0];
    assert_eq!(saved.name, format!("jesse/{}-wip", rec.branch));
    assert_eq!(saved.worktree, WORKTREE_DIR);
    // The snapshot sits on the agent's commit; the worktree is as the agent left it.
    sh(
        &origin(&s),
        &["merge-base", "--is-ancestor", &commit, &saved.sha],
    );
    assert_eq!(sh(&wt, &["rev-parse", "HEAD"]).trim(), commit);
    assert!(wt.join("loose.txt").is_file());
    let text = std::fs::read_to_string(s.notes().join(format!("Inbox/{}-run-s-stops.md", today())))
        .unwrap();
    assert!(
        text.contains(&format!(
            "\nIts work is saved on jesse/{}-wip at {}.\n",
            rec.branch, saved.sha
        )),
        "{text}"
    );
}

#[tokio::test]
async fn continue_restores_a_removed_worktree_from_its_wip_ref() {
    let s = Scratch::new();
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(free("resume")).await.unwrap();
    let dir = r.svc.settings.run_dir("s-resume");
    let wt = dir.join(WORKTREE_DIR);
    let commit = unpushed_commit_and_loose_file(&wt);
    finish_round(&r, "s-resume", 0, Some("ok\n"), "");
    r.svc.pass(now_ms()).await;
    r.svc.cleanup(now_ms() + 8 * DAY_MS);
    assert!(!wt.exists());
    let rec = r.svc.get("s-resume").unwrap();
    // The branch itself never reached origin.
    assert!(!has_ref(&origin(&s), &format!("refs/heads/{}", rec.branch)));

    let rec = r.svc.reply_and_pump("s-resume", "go on").await.unwrap();
    assert_eq!(rec.status, RunStatus::Running);
    let text = rec.round().unwrap().text.clone().unwrap();
    assert!(
        text.ends_with(&format!(
            "go on\n\nYour previous worktree was removed; it was restored from jesse/{}-wip. \
             The top commit is a snapshot of uncommitted work: review it and fold it into \
             proper commits.",
            rec.branch
        )),
        "{text}"
    );
    // Back on its branch: the commit, and the uncommitted file as the top commit.
    assert_eq!(
        sh(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        rec.branch
    );
    assert_eq!(sh(&wt, &["rev-parse", "HEAD~1"]).trim(), commit);
    assert_eq!(
        std::fs::read_to_string(wt.join("loose.txt")).unwrap(),
        "uncommitted\n"
    );
    assert!(git::worktree_clean(&wt));
}

#[test]
fn restore_with_nothing_saved_anywhere_names_every_place_it_looked() {
    let s = Scratch::new();
    remote_fixture(&s);
    let st = settings(&s);
    let branch = "r3-work";
    let wt = worktree(&s, &st, "R3", branch);
    // Nothing done: nothing to save, so it simply goes.
    assert_eq!(
        git::remove_worktree(&st.root, "o/n", &wt, branch, "R3").unwrap(),
        None
    );
    let e = git::restore_worktree(&st.root, "o/n", &st.repo_url("o/n"), &wt, branch, "R3", &[])
        .unwrap_err();
    for place in [
        format!("origin/{branch}"),
        format!("origin/jesse/{branch}-wip*"),
        "refs/jesse/saved/R3/*".to_string(),
        "the run record's saved refs (0)".to_string(),
        format!("the local branch {branch}"),
    ] {
        assert!(e.contains(&place), "{place} missing from: {e}");
    }
    assert!(!wt.exists());
}

#[test]
fn restore_prefers_saved_work_built_on_the_pushed_branch_and_never_goes_backwards() {
    let s = Scratch::new();
    remote_fixture(&s);
    let st = settings(&s);
    let branch = "r4-work";
    let wt = worktree(&s, &st, "R4", branch);
    // Pushed once, then more work that never was.
    std::fs::write(wt.join("a.txt"), "a\n").unwrap();
    sh(&wt, &["add", "a.txt"]);
    sh(&wt, &["commit", "-q", "-m", "a"]);
    sh(&wt, &["push", "-q", "origin", branch]);
    let later = unpushed_commit_and_loose_file(&wt);
    git::remove_worktree(&st.root, "o/n", &wt, branch, "R4")
        .unwrap()
        .expect("saved");
    let p = git::restore_worktree(&st.root, "o/n", &st.repo_url("o/n"), &wt, branch, "R4", &[])
        .unwrap();
    assert_eq!(p.from, format!("jesse/{branch}-wip"));
    assert!(p.saved && p.snapshot);
    assert_eq!(sh(&wt, &["rev-parse", "HEAD~1"]).trim(), later);
}

#[test]
fn a_squash_merged_branch_whose_head_is_a_pull_request_needs_no_wip_ref() {
    let s = Scratch::new();
    remote_fixture(&s);
    let st = settings(&s);
    let branch = "r5-work";
    let wt = worktree(&s, &st, "R5", branch);
    std::fs::write(wt.join("a.txt"), "a\n").unwrap();
    sh(&wt, &["add", "a.txt"]);
    sh(&wt, &["commit", "-q", "-m", "a"]);
    let tip = sh(&wt, &["rev-parse", "HEAD"]).trim().to_string();
    sh(&wt, &["push", "-q", "origin", branch]);
    // GitHub after a squash merge: the branch is deleted, its head kept as the pull request's.
    sh(&origin(&s), &["update-ref", "refs/pull/7/head", &tip]);
    sh(&origin(&s), &["branch", "-q", "-D", branch]);
    let bare = git::bare_path(&st.root, "o/n");
    sh(&bare, &["fetch", "-q", "--prune", "origin"]);
    assert_eq!(
        git::remove_worktree(&st.root, "o/n", &wt, branch, "R5").unwrap(),
        None
    );
    assert!(!wt.exists());
    assert!(!has_branch(&bare, branch));
    assert!(sh(&origin(&s), &["for-each-ref", "refs/heads/jesse/"])
        .trim()
        .is_empty());
    assert!(has_ref(&bare, "refs/jesse/pull/7"));
}
