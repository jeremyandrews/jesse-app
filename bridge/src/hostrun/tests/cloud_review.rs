//! One run pipeline, Part B: a cloud run is reviewed as a Studio run is. The same reviewers
//! resolved by the same rule, the same review of the work (for a cloud run its pushed branch,
//! read from a scratch checkout), the same hand off text, and the answer as the chain's next
//! Job on the same branch and pull request, which alone may merge. The Job side is the real Job
//! builder; what the Job does on kc1 is a push to a local origin.

use super::review::{findings, settle, FakeReviewer, TWO_FINDINGS};
use super::*;
use crate::devjob::cloud::{self as dc, MergeDecision};
use crate::hostrun::review::{handoff_reply, ReviewStatus, HANDOFF_FINISH, REVIEWER_PROMPT};
use std::collections::BTreeMap;

fn rig_reviewing(s: &Scratch, defaults: &[&str], reviewer: Arc<FakeReviewer>) -> Rig {
    let mut st = settings(s);
    st.max_concurrent = 10;
    st.max_per_repo = 10;
    st.default_reviewers = defaults.iter().map(|d| d.to_string()).collect();
    let r = rig_with(s, st);
    r.svc.attach_reviewer(reviewer);
    r
}

/// A cloud start that asks to merge at green.
fn merging() -> Option<CloudPart> {
    Some(CloudPart {
        merge: Some(true),
        ..CloudPart::default()
    })
}

fn free_reviewed(slug: &str, reviewer: &str) -> StartRequest {
    StartRequest {
        reviewer: Some(reviewer.into()),
        ..free_in("o/n", slug)
    }
}

/// The Job a run record executes as now, built by the cloud's own builder: its env and its
/// annotations.
fn job_of(
    r: &Rig,
    s: &Scratch,
    rec: &RunRecord,
) -> (dc::CloudRun, BTreeMap<String, String>, Value) {
    let section = r.svc.section_of(&rec.run_id).unwrap();
    let merge = dc::merge_decision(&rec.repo, dc::asked_merge(&s.notes(), rec), &[]);
    let run = dc::record_job(
        rec,
        &section,
        merge,
        crate::devjob::Agent::Claude,
        120,
        None,
    )
    .unwrap();
    let extras = crate::devjob::CloudJobExtras {
        run_id: run.run_id.clone(),
        merge: run.merge.clone(),
        merge_deny: String::new(),
        resources: dc::CloudSettings::default().job_resources(),
        services: false,
        chain: run.chain.clone(),
        review: run.review,
    };
    let job =
        crate::devjob::build_cloud_job("jesse-dev", "job", "cm", "img", &run.spec, &extras, "turn");
    let job = serde_json::to_value(job).unwrap();
    let env = job["spec"]["template"]["spec"]["containers"][0]["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap().to_string(),
                e["value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let ann = job["metadata"]["annotations"].clone();
    (run, env, ann)
}

/// What the Job does on kc1, here: a commit on `branch` pushed to the local origin `o/n`, and,
/// with `pull`, the pull request's head ref as GitHub keeps it. Returns the commit.
fn push_work(
    s: &Scratch,
    branch: &str,
    file: &str,
    text: &str,
    message: &str,
    pull: Option<u64>,
) -> String {
    let remote = s.base.join("remote/o/n.git");
    let c = s.base.join(format!("pod-{}", branch.replace('/', "-")));
    if !c.exists() {
        sh(
            &s.base,
            &[
                "clone",
                "-q",
                &remote.to_string_lossy(),
                &c.to_string_lossy(),
            ],
        );
        sh(&c, &["checkout", "-q", "-b", branch]);
    }
    std::fs::create_dir_all(c.join(Path::new(file).parent().unwrap())).unwrap();
    std::fs::write(c.join(file), text).unwrap();
    sh(&c, &["add", "-A"]);
    sh(&c, &["commit", "-q", "-m", message]);
    sh(
        &c,
        &[
            "push",
            "-q",
            "-f",
            "origin",
            &format!("HEAD:refs/heads/{branch}"),
        ],
    );
    if let Some(n) = pull {
        sh(
            &c,
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("HEAD:refs/pull/{n}/head"),
            ],
        );
    }
    sh(&c, &["rev-parse", "HEAD"]).trim().to_string()
}

fn ended(job: &str, status: RunStatus, report: &str) -> CloudEnd {
    CloudEnd {
        job: job.into(),
        status,
        report_path: Some(report.into()),
        continued_as: None,
        error: None,
        leaks: Vec::new(),
    }
}

fn started(r: &Rig) -> usize {
    lock(&r.launcher.bootstrapped).len()
}

// ---- THE REVIEWED CLOUD RUN, START TO MERGE ---------------------------------------------------

#[tokio::test]
async fn a_reviewed_cloud_run_is_not_merged_before_its_review_is_answered_then_is() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let reviewer = FakeReviewer::answering(vec![Ok(TWO_FINDINGS.into())]);
    let r = rig_reviewing(&s, &[], reviewer.clone());
    let req = StartRequest {
        review_focus: Some("the loop bounds".into()),
        ..free_reviewed("calc", "codex-write")
    };
    let id = r.svc.create_on(&req, merging()).unwrap().run_id;
    let rec = r.svc.get(&id).unwrap();
    assert_eq!(rec.reviewer_ids(), ["codex-write"]);
    assert_eq!(rec.review_focus.as_deref(), Some("the loop bounds"));

    // 1. Its first Job: the run asks to merge, and its Job holds the merge for the review.
    let rec = r.svc.take_for_cloud(&s.notes()).unwrap();
    let (run, env, ann) = job_of(&r, &s, &rec);
    assert_eq!(run.run_id, id);
    assert_eq!(run.merge, MergeDecision::AtGreen);
    assert_eq!(env["REVIEW"], "hold");
    assert_eq!(ann[dc::ANN_REVIEW], "hold");
    // Not even an entrypoint that ignores `REVIEW` merges it; the decision rides on for later.
    assert_eq!(env["AUTO_MERGE"], "");
    assert_eq!(
        ann[dc::ANN_MERGE],
        MergeDecision::AtGreen.annotation().as_str()
    );
    assert!(
        run.spec.prompt.contains("this run does not merge"),
        "{}",
        run.spec.prompt
    );
    r.svc.note_job(&id, "job-1", "jesse/calc").unwrap();
    let base = sh(&s.base.join("remote/o/n.git"), &["rev-parse", "main"])
        .trim()
        .to_string();
    let head = push_work(
        &s,
        "jesse/calc",
        "src/calc.rs",
        "fn sum() {}\n",
        "Add the sum",
        Some(1),
    );

    // 2. It ends with its draft pull request: the run is reviewed, not reported, nothing sent.
    let rec = r
        .svc
        .cloud_ended(&ended(
            "job-1",
            RunStatus::Reported,
            "Inbox/x-run-s-calc.md",
        ))
        .unwrap();
    assert_eq!(rec.status, RunStatus::Reviewing);
    assert_eq!(rec.rounds[0].outcome, Some(RunStatus::Reported));
    assert!(rec.rounds[0].notified);
    settle(&r).await;

    // 3. The review read the pushed branch, from a scratch checkout the bridge removed.
    let dir = r.svc.settings.run_dir(&id);
    let calls = lock(&reviewer.calls).clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cwd, dir.join("review-1"));
    assert!(calls[0].prompt.starts_with(REVIEWER_PROMPT));
    assert!(calls[0].prompt.contains("Write a file."));
    assert!(calls[0]
        .prompt
        .contains(&format!("## The diff, {base}..{head}")));
    assert!(calls[0].prompt.contains("+fn sum() {}"));
    assert!(!dir.join(REVIEW_RANGE_DIR).exists());
    assert!(!dir.join("review-1").exists());

    // 4. The hand off: the same text a Studio run is handed, queued as the run's next round,
    // for the cloud: no Studio slot starts it.
    let rec = r.svc.get(&id).unwrap();
    assert_eq!(rec.reviews[0].status, ReviewStatus::HandedOff);
    assert_eq!(rec.status, RunStatus::Queued);
    assert!(rec.on_cloud());
    assert_eq!(rec.waiting_for.as_deref(), Some(WAITING_CLOUD));
    let text = handoff_reply(&findings(), Some(HANDOFF_FINISH));
    assert_eq!(
        rec.round().unwrap().auto.as_deref(),
        Some(crate::hostrun::review::AUTO_REVIEW)
    );
    assert_eq!(rec.round().unwrap().text.as_deref(), Some(text.as_str()));
    assert_eq!(started(&r), 0);

    // 5. Its answer: the next link, on the same branch and pull request, and it may merge.
    let rec = r.svc.take_for_cloud(&s.notes()).unwrap();
    assert_eq!(rec.run_id, id);
    let (run, env, ann) = job_of(&r, &s, &rec);
    assert_eq!(run.run_id, format!("{id}-c2"));
    assert_eq!(env["REVIEW"], "answer");
    assert_eq!(ann[dc::ANN_REVIEW], "answer");
    assert_eq!(env["AUTO_MERGE"], "true");
    assert_eq!(env["RUN_ID"], id);
    assert_eq!(env["CONTINUATION"], "1");
    assert_eq!(env["BRANCH_SLUG"], "calc");
    assert_eq!(ann[dc::ANN_CONTINUATION_OF], id.as_str());
    assert!(run
        .spec
        .prompt
        .contains(&format!("{}\n\n{}", dc::ROUND_HEADER, "An earlier run")));
    assert!(run.spec.prompt.contains(text.trim()));
    assert!(run
        .spec
        .prompt
        .contains("merges it once every CI run on its exact head"));
    r.svc.note_job(&id, "job-2", "jesse/calc").unwrap();
    push_work(
        &s,
        "jesse/calc",
        "src/calc.rs",
        "fn sum() { /* all */ }\n",
        "Sum every element",
        Some(1),
    );
    let (report, leaks) = r
        .svc
        .cloud_report_marks(
            "job-2",
            "Done.\n".into(),
            RunStatus::Reported,
            Some("https://github.com/o/n/pull/1"),
        )
        .await;
    assert!(leaks.is_empty(), "{leaks:?}");
    assert!(report.contains("Review note: [["), "{report}");
    let mut end = ended("job-2", RunStatus::Reported, "Inbox/x-run-s-calc-c2.md");
    end.leaks = leaks;
    let rec = r.svc.cloud_ended(&end).unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    assert_eq!(rec.reviews[0].status, ReviewStatus::Answered);
    assert_eq!(rec.cloud.as_ref().unwrap().jobs, ["job-1", "job-2"]);
    // Reviewed once: a reply is a further link too, and is not reviewed again.
    r.svc.reply(&id, "One more thing.").unwrap();
    let rec = r.svc.take_for_cloud(&s.notes()).unwrap();
    let (run, env, _) = job_of(&r, &s, &rec);
    assert_eq!(
        (run.run_id.as_str(), env["REVIEW"].as_str()),
        (format!("{id}-c3").as_str(), "answer")
    );
    assert!(run.spec.prompt.contains("One more thing."));
    assert_eq!(lock(&reviewer.calls).len(), 1);
}

#[tokio::test]
async fn an_unreviewed_cloud_run_merges_as_today() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let reviewer = FakeReviewer::answering(vec![]);
    let r = rig_reviewing(&s, &["codex-write"], reviewer.clone());
    let id = r
        .svc
        .create_on(&free_reviewed("plain", "none"), merging())
        .unwrap()
        .run_id;
    let rec = r.svc.take_for_cloud(&s.notes()).unwrap();
    assert!(rec.reviewer_ids().is_empty());
    let (run, env, ann) = job_of(&r, &s, &rec);
    assert_eq!(run.review, None);
    assert!(!env.contains_key("REVIEW"), "{env:?}");
    assert!(ann.get(dc::ANN_REVIEW).is_none());
    assert_eq!(env["AUTO_MERGE"], "true");
    assert!(run
        .spec
        .prompt
        .contains("merges it once every CI run on its exact head"));
    r.svc.note_job(&id, "job-p", "jesse/plain").unwrap();
    let rec = r
        .svc
        .cloud_ended(&ended(
            "job-p",
            RunStatus::Reported,
            "Inbox/x-run-s-plain.md",
        ))
        .unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    settle(&r).await;
    assert!(lock(&reviewer.calls).is_empty());
    assert!(r.svc.get(&id).unwrap().reviews.is_empty());
}

#[tokio::test]
async fn a_leak_in_a_cloud_answer_leads_its_report_as_on_the_studio() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig_reviewing(
        &s,
        &[],
        FakeReviewer::answering(vec![Ok(TWO_FINDINGS.into())]),
    );
    let id = r
        .svc
        .create_on(&free_reviewed("leak", "codex-write"), merging())
        .unwrap()
        .run_id;
    r.svc.take_for_cloud(&s.notes()).unwrap();
    r.svc.note_job(&id, "job-1", "jesse/leak").unwrap();
    push_work(&s, "jesse/leak", "src/calc.rs", "a\n", "Add calc", Some(4));
    r.svc
        .cloud_ended(&ended("job-1", RunStatus::Reported, "Inbox/a.md"))
        .unwrap();
    settle(&r).await;
    r.svc.take_for_cloud(&s.notes()).unwrap();
    r.svc.note_job(&id, "job-2", "jesse/leak").unwrap();
    let head = push_work(
        &s,
        "jesse/leak",
        "src/calc.rs",
        "b // the reviewer was right\n",
        "Fix F1 from the code review",
        Some(4),
    );
    let (report, leaks) = r
        .svc
        .cloud_report_marks(
            "job-2",
            "Fixed.\n".into(),
            RunStatus::Reported,
            Some("https://github.com/o/n/pull/4"),
        )
        .await;
    let short = &head[..7];
    // The Studio's own leak test expects exactly these two places and this first line.
    assert_eq!(
        leaks,
        [
            format!("commit {short} message: F1, code review"),
            "src/calc.rs (added lines): reviewer".to_string()
        ]
    );
    assert!(
        report.starts_with(&format!(
            "REVIEW LEAK: commit {short} message: F1, code review; src/calc.rs (added lines): \
         reviewer\n\nFixed.\n"
        )),
        "{report}"
    );
    assert!(!r.svc.settings.run_dir(&id).join(REVIEW_ANSWER_DIR).exists());
    let mut end = ended("job-2", RunStatus::Reported, "Inbox/b.md");
    end.leaks = leaks.clone();
    let rec = r.svc.cloud_ended(&end).unwrap();
    assert_eq!(rec.reviews[0].leaks, leaks);
    assert_eq!(rec.status, RunStatus::Reported, "a leak blocks nothing");
}

// ---- ONE RULE, TWO HOSTS ----------------------------------------------------------------------

/// The reviewer default and its checks have one implementation: the same starts, once for the
/// Studio and once for the cloud, resolve the same reviewers from the same places, or are
/// refused with the same words.
#[tokio::test]
async fn the_studio_and_the_cloud_resolve_reviewers_by_one_rule() {
    let s = Scratch::new();
    vault_fixture(&s);
    let d = s.notes().join("Projects/drafts");
    for (name, extra) in [
        ("dflt", ""),
        ("named", "devjob-reviewer: glm\n"),
        ("off", "devjob-reviewer: none\n"),
        ("sickly", "devjob-reviewer: sick\n"),
        ("many", "devjob-reviewer: [codex-write, glm]\n"),
    ] {
        std::fs::write(
            d.join(format!("{name}.md")),
            loose(&format!("devjob-host: any\n{extra}")),
        )
        .unwrap();
    }
    let r = rig_reviewing(&s, &["codex-write", "glm"], FakeReviewer::answering(vec![]));
    let at = |name: &str| by_draft(&format!("Projects/drafts/{name}.md"));
    let cases: Vec<StartRequest> = vec![
        at("dflt"),
        at("named"),
        at("off"),
        at("sickly"),
        at("many"),
        StartRequest {
            reviewer: Some("glm".into()),
            ..at("off")
        },
        StartRequest {
            reviewer: Some("nobody".into()),
            ..at("dflt")
        },
        StartRequest {
            review_focus: Some("bounds".into()),
            ..at("off")
        },
        StartRequest {
            review_focus: Some("bounds".into()),
            ..at("dflt")
        },
    ];
    let outcome = |res: Result<StartedRun, Refusal>| match res {
        Ok(st) => {
            let rec = r.svc.get(&st.run_id).unwrap();
            Ok((
                rec.reviewer_ids(),
                rec.reviewer_label.clone(),
                rec.reviewers_source.clone(),
                rec.reviewers_skipped.clone(),
                rec.review_focus.clone(),
            ))
        }
        Err(e) => Err(e.message().to_string()),
    };
    for req in cases {
        let studio = outcome(r.svc.create_on(&req, None));
        let cloud = outcome(r.svc.create_on(&req, Some(CloudPart::default())));
        assert_eq!(studio, cloud, "{req:?}");
    }
    // And the default really is the default on the cloud.
    let st = r
        .svc
        .create_on(&at("dflt"), Some(CloudPart::default()))
        .unwrap();
    assert_eq!(
        r.svc.get(&st.run_id).unwrap().reviewer_ids(),
        ["codex-write", "glm"]
    );
}

/// The review prompt and the hand off have one implementation: a Studio run and a cloud run
/// with the same task and the same change are reviewed with the same prompt (but for the head
/// commit's id) and handed the same text.
#[tokio::test]
async fn a_studio_run_and_a_cloud_run_are_reviewed_and_handed_off_alike() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let reviewer = FakeReviewer::answering(vec![Ok(TWO_FINDINGS.into()), Ok(TWO_FINDINGS.into())]);
    let r = rig_reviewing(&s, &[], reviewer.clone());
    // On the Studio: its worktree.
    let studio = r
        .svc
        .start(free_reviewed("here", "codex-write"))
        .await
        .unwrap()
        .run_id;
    let studio_head =
        super::review::commit(&r, &studio, "src/calc.rs", "fn sum() {}\n", "Add the sum");
    finish_round(&r, &studio, 0, Some("Done.\n"), "");
    settle(&r).await;
    // On the cloud: its pushed branch.
    let cloud = r
        .svc
        .create_on(&free_reviewed("there", "codex-write"), merging())
        .unwrap()
        .run_id;
    r.svc.take_for_cloud(&s.notes()).unwrap();
    r.svc.note_job(&cloud, "job-t", "jesse/there").unwrap();
    let cloud_head = push_work(
        &s,
        "jesse/there",
        "src/calc.rs",
        "fn sum() {}\n",
        "Add the sum",
        None,
    );
    r.svc
        .cloud_ended(&ended("job-t", RunStatus::Reported, "Inbox/t.md"))
        .unwrap();
    settle(&r).await;

    let calls = lock(&reviewer.calls).clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].prompt.replace(&studio_head, "HEAD"),
        calls[1].prompt.replace(&cloud_head, "HEAD")
    );
    let (a, b) = (r.svc.get(&studio).unwrap(), r.svc.get(&cloud).unwrap());
    assert_eq!(a.round().unwrap().auto, b.round().unwrap().auto);
    assert_eq!(a.round().unwrap().text, b.round().unwrap().text);
    assert_eq!(
        b.round().unwrap().text.as_deref(),
        Some(handoff_reply(&findings(), Some(HANDOFF_FINISH)).as_str())
    );
    // Each goes on where it runs: the Studio's on its runner, the cloud's to the cloud.
    assert_eq!(a.status, RunStatus::Running);
    assert_eq!(b.status, RunStatus::Queued);
    assert!(b.on_cloud());
}
