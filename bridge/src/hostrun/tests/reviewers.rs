//! Who reviews a Studio run (0.213.0): `[hostrun] default_reviewers`, `reviewer` as an id, a
//! list or `none` (and `reviewers`, its list spelling), `devjob-reviewer:` the same way, the
//! difference between a reviewer named and a default one when it cannot review, several
//! reviewers in one cycle with one hand off and one answering round, and the cluster, which is
//! never reviewed.

use super::review::{
    commit, devjob_server, findings, pushes, review_rig, reviewed, settle, FakeReviewer,
    TWO_FINDINGS,
};
use super::*;
use crate::gateway::InProcessServer;
use crate::hostrun::review::*;

// ---- FIXTURES ------------------------------------------------------------------------------

const ONE_FINDING: &str = r#"{"findings": [
 {"id": "F1", "severity": "risk", "file": "src/calc.rs", "line": 5, "claim": "Overflow is unchecked.", "evidence": "It adds two u32 with +.", "disproof": "A test adding u32::MAX and 1 does not panic."}
], "summary": "One risk."}"#;

/// A rig whose bridge default is `defaults`.
fn default_rig(s: &Scratch, reviewer: Arc<FakeReviewer>, defaults: &[&str]) -> Rig {
    let mut st = settings(s);
    st.default_reviewers = defaults.iter().map(|d| d.to_string()).collect();
    st.max_concurrent = 20;
    st.max_per_repo = 20;
    let r = rig_with(s, st);
    r.svc.attach_reviewer(reviewer);
    r
}

fn with_reviewer(req: StartRequest, arg: ReviewerArg) -> StartRequest {
    StartRequest {
        reviewer: Some(arg),
        ..req
    }
}

/// GD5's draft with `extra` added to its front matter.
fn gd5_draft_with(s: &Scratch, extra: &str) {
    let body = format!("## PROMPT\n{SECTION}");
    std::fs::write(
        s.notes().join("Projects/drafts/gd5-prompt.md"),
        draft("GD5", &format!("{STUDIO}{extra}"), &body),
    )
    .unwrap();
}

/// The body of round `i`'s Inbox report, after its front matter.
fn report_body(s: &Scratch, rec: &RunRecord, i: usize) -> String {
    let text = std::fs::read_to_string(s.notes().join(rec.rounds[i].report_path.as_ref().unwrap()))
        .unwrap();
    text.split("---\n\n").nth(1).unwrap().to_string()
}

// ---- THE ARGUMENT ----------------------------------------------------------------------------

#[test]
fn the_reviewer_argument_is_none_an_id_a_list_or_nothing_and_each_fault_is_named() {
    let ids = |v: &[&str]| ReviewerChoice::Ids(v.iter().map(|s| s.to_string()).collect());
    let ok = |a: ReviewerArg| choose(Some(&a)).unwrap();
    assert_eq!(choose(None).unwrap(), ReviewerChoice::Unset);
    assert_eq!(
        ok("  ".into()),
        ReviewerChoice::Unset,
        "a blank string is unset"
    );
    assert_eq!(ok("none".into()), ReviewerChoice::Off);
    assert_eq!(ok("None".into()), ReviewerChoice::Off);
    assert_eq!(ok(vec!["none"].into()), ReviewerChoice::Off);
    assert_eq!(ok(" glm ".into()), ids(&["glm"]));
    assert_eq!(
        ok(vec!["glm", "codex-write"].into()),
        ids(&["glm", "codex-write"]),
        "a list is used exactly, in its order"
    );
    for (bad, want) in [
        (
            ReviewerArg::Many(vec![]),
            "reviewer names no model: give at least one id, or \"none\"",
        ),
        (
            vec!["none", "glm"].into(),
            "reviewer \"none\" stands alone: it cannot be listed with a model",
        ),
        (
            vec!["a", "b", "c", "d"].into(),
            "reviewer names 4 models; the most is 3",
        ),
        (vec!["glm", "glm"].into(), "reviewer \"glm\" is named twice"),
        (
            "two words".into(),
            "reviewer \"two words\" is not a model id",
        ),
    ] {
        assert_eq!(choose(Some(&bad)).unwrap_err(), want, "{bad:?}");
    }
    // `reviewer` and `reviewers` are one argument: both is refused.
    assert_eq!(
        merge_spellings(Some("glm".into()), Some(vec!["codex-write".into()])).unwrap_err(),
        "pass reviewer or reviewers, not both"
    );
    assert_eq!(
        merge_spellings(None, Some(vec!["glm".into()])).unwrap(),
        Some(vec!["glm"].into())
    );
    // The wire takes a string or a list.
    let body: StartBody =
        serde_json::from_value(json!({ "draft": "d.md", "reviewer": ["codex-write", "glm"] }))
            .unwrap();
    assert_eq!(body.reviewer, Some(vec!["codex-write", "glm"].into()));
    let body: StartBody =
        serde_json::from_value(json!({ "draft": "d.md", "reviewer": "none" })).unwrap();
    assert_eq!(body.reviewer, Some("none".into()));
}

#[test]
fn a_draft_names_one_reviewer_a_list_or_none() {
    let fm_of = |extra: &str| format!("---\nstrand: Gd\n{extra}---\n# x\n");
    let read = |extra: &str| draft_reviewer("d.md", &fm_of(extra));
    assert_eq!(read("").unwrap(), None);
    assert_eq!(
        read("devjob-reviewer: codex-write\n").unwrap(),
        Some("codex-write".into())
    );
    assert_eq!(
        read("devjob-reviewer: \"glm\"\n").unwrap(),
        Some("glm".into())
    );
    assert_eq!(
        read("devjob-reviewer: none\n").unwrap(),
        Some("none".into())
    );
    assert_eq!(
        read("devjob-reviewer: [codex-write, 'glm']\n").unwrap(),
        Some(vec!["codex-write", "glm"].into())
    );
    assert_eq!(
        read("devjob-reviewer:\n  - codex-write\n  - glm\ndevjob-host: studio\n").unwrap(),
        Some(vec!["codex-write", "glm"].into())
    );
    assert_eq!(
        read("devjob-reviewer:\n- none\n").unwrap(),
        Some(vec!["none"].into())
    );
    // Blank lines and comments in a block list are skipped, never the end of it.
    assert_eq!(
        read("devjob-reviewer:\n  # switched off for now\n  - none\n").unwrap(),
        Some(vec!["none"].into()),
        "a comment above `- none` still switches the review off"
    );
    assert_eq!(
        read("devjob-reviewer:\n  - codex-write\n\n  - glm  # second\ndevjob-host: studio\n")
            .unwrap(),
        Some(vec!["codex-write", "glm"].into())
    );
    assert_eq!(
        read("devjob-reviewer: none # no review\n").unwrap(),
        Some("none".into())
    );
    assert_eq!(
        read("devjob-reviewer: [codex-write, glm] # both\n").unwrap(),
        Some(vec!["codex-write", "glm"].into())
    );
    assert_eq!(
        read("devjob-reviewer: [a, b, c, d]\n").unwrap_err(),
        "d.md: devjob-reviewer names 4 models; the most is 3"
    );
    assert_eq!(
        read("devjob-reviewer: [glm, glm]\n").unwrap_err(),
        "d.md: devjob-reviewer \"glm\" is named twice"
    );
    assert_eq!(
        read("devjob-reviewer: [none, glm]\n").unwrap_err(),
        "d.md: devjob-reviewer \"none\" stands alone: it cannot be listed with a model"
    );
}

#[test]
fn default_reviewers_is_codex_unless_the_config_says_otherwise() {
    let st = HostRunSettings::from_toml(None, "/home/u", "claude");
    assert_eq!(st.default_reviewers, ["codex-write"]);
    let toml = |text: &str| -> HostRunSettings {
        let t: HostRunToml = toml::from_str(text).unwrap();
        HostRunSettings::from_toml(Some(t), "/home/u", "claude")
    };
    assert!(toml("default_reviewers = []\n")
        .default_reviewers
        .is_empty());
    assert_eq!(
        toml("default_reviewers = [\"glm\", \"codex-write\"]\n").default_reviewers,
        ["glm", "codex-write"]
    );
    let s = Scratch::new();
    let problems = |list: &str| {
        let mut st = toml(&format!("enabled = true\ndefault_reviewers = {list}\n"));
        st.root = s.root();
        st.local_checkout_root = s.base.join("checkouts");
        st.problems()
            .into_iter()
            .filter(|p| p.contains("default_reviewers"))
            .collect::<Vec<_>>()
    };
    assert!(problems("[]").is_empty());
    assert!(problems("[\"glm\"]").is_empty());
    assert_eq!(
        problems("[\"glm\", \"glm\"]"),
        ["[hostrun] default_reviewers: reviewer \"glm\" is named twice"]
    );
    assert_eq!(
        problems("[\"a\", \"b\", \"c\", \"d\"]"),
        ["[hostrun] default_reviewers: reviewer names 4 models; the most is 3"]
    );
    assert_eq!(
        problems("[\"none\"]"),
        ["[hostrun] default_reviewers: write [] for no default review, not \"none\""]
    );
}

// ---- RESOLUTION AT THE START -------------------------------------------------------------------

#[tokio::test]
async fn the_start_resolves_none_then_an_explicit_id_or_list_then_the_draft_then_the_default() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = default_rig(&s, FakeReviewer::answering(vec![]), &["codex-write"]);
    let who = |id: &str| {
        let rec = r.svc.get(id).unwrap();
        (
            rec.reviewers.clone(),
            rec.reviewers_source.clone().unwrap(),
            rec.reviewer.clone(),
            rec.reviewer_label.clone(),
        )
    };
    let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // Nothing named: the default.
    r.svc.start(free("plain")).await.unwrap();
    assert_eq!(
        who("s-plain"),
        (
            v(&["codex-write"]),
            "default".into(),
            Some("codex-write".into()),
            Some("Codex".into())
        )
    );
    let prompt =
        std::fs::read_to_string(r.svc.settings.run_dir("s-plain").join(PROMPT_MD)).unwrap();
    assert!(prompt.ends_with(&format!("\n\n{REVIEWED_RUN_SENTENCE}\n")));
    // `none`: no review, and an ordinary prompt.
    r.svc
        .start(with_reviewer(free("off"), "none".into()))
        .await
        .unwrap();
    assert_eq!(who("s-off"), (v(&[]), "none".into(), None, None));
    let prompt = std::fs::read_to_string(r.svc.settings.run_dir("s-off").join(PROMPT_MD)).unwrap();
    assert!(!prompt.contains(REVIEWED_RUN_SENTENCE));
    r.svc
        .start(with_reviewer(free("off2"), vec!["none"].into()))
        .await
        .unwrap();
    assert_eq!(who("s-off2").1, "none");
    // An explicit string, and an explicit list used exactly, labels joined.
    r.svc.start(reviewed(free("one"), "glm")).await.unwrap();
    assert_eq!(
        who("s-one"),
        (
            v(&["glm"]),
            "explicit".into(),
            Some("glm".into()),
            Some("GLM 5".into())
        )
    );
    r.svc
        .start(with_reviewer(
            free("two"),
            vec!["glm", "codex-write"].into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        who("s-two"),
        (
            v(&["glm", "codex-write"]),
            "explicit".into(),
            Some("glm".into()),
            Some("GLM 5 + Codex".into())
        )
    );
    assert_eq!(
        r.svc.get("s-two").unwrap().reviewer_labels,
        v(&["GLM 5", "Codex"])
    );
    // The draft: a string, a list, `none`; and the start's own wins over it.
    for (extra, start, want, source) in [
        ("devjob-reviewer: glm\n", None, v(&["glm"]), "explicit"),
        (
            "devjob-reviewer: [glm, codex-write]\n",
            None,
            v(&["glm", "codex-write"]),
            "explicit",
        ),
        ("devjob-reviewer: none\n", None, v(&[]), "none"),
        (
            "devjob-reviewer: none\n",
            Some(ReviewerArg::from("glm")),
            v(&["glm"]),
            "explicit",
        ),
        (
            "devjob-reviewer: glm\n",
            Some(ReviewerArg::from("none")),
            v(&[]),
            "none",
        ),
        ("", None, v(&["codex-write"]), "default"),
    ] {
        gd5_draft_with(&s, extra);
        let started = r
            .svc
            .start(StartRequest {
                reviewer: start.clone(),
                again: true,
                ..item("GD5")
            })
            .await
            .unwrap();
        let rec = r.svc.get(&started.run_id).unwrap();
        assert_eq!(
            (rec.reviewers.clone(), rec.reviewers_source.as_deref()),
            (want.clone(), Some(source)),
            "{extra:?} with {start:?}"
        );
    }
}

#[tokio::test]
async fn an_empty_default_list_leaves_a_run_unreviewed_and_its_report_as_it_was() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let reviewer = FakeReviewer::answering(vec![]);
    let r = default_rig(&s, reviewer.clone(), &[]);
    r.svc.start(free("plain")).await.unwrap();
    let rec = r.svc.get("s-plain").unwrap();
    assert!(rec.reviewers.is_empty() && rec.reviewers_skipped.is_empty());
    assert_eq!(rec.reviewers_source.as_deref(), Some("default"));
    commit(&r, "s-plain", "a.txt", "a\n", "A");
    finish_round(&r, "s-plain", 0, Some("Merged.\n"), "");
    settle(&r).await;
    let rec = r.svc.get("s-plain").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    assert!(rec.reviews.is_empty());
    assert!(lock(&reviewer.calls).is_empty());
    assert!(report_body(&s, &rec, 0).starts_with("Merged.\n"));
    // A focus with nothing to review is still refused.
    let e = r
        .svc
        .start(StartRequest {
            review_focus: Some("bounds".into()),
            ..free("focus")
        })
        .await
        .unwrap_err();
    assert_eq!(
        e.message(),
        "review_focus needs a reviewer: pass reviewer too"
    );
}

#[tokio::test]
async fn a_named_reviewer_that_cannot_review_is_refused_and_so_are_too_many_and_repeats() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = default_rig(&s, FakeReviewer::answering(vec![]), &["codex-write"]);
    let runs = r.svc.settings.runs_dir();
    for (arg, want) in [
        (
            ReviewerArg::from(vec!["glm", "sick"]),
            "reviewer \"sick\" is unhealthy right now",
        ),
        (
            "nobody".into(),
            "reviewer \"nobody\" is not a model this bridge knows",
        ),
        (
            vec!["glm", "codex-write", "glm"].into(),
            "reviewer \"glm\" is named twice",
        ),
        (
            vec!["glm", "codex-write", "a", "b"].into(),
            "reviewer names 4 models; the most is 3",
        ),
    ] {
        let e = r
            .svc
            .start(with_reviewer(free("x"), arg.clone()))
            .await
            .unwrap_err();
        assert_eq!(e, Refusal::Invalid(want.into()), "{arg:?}");
    }
    // A draft that names one that is down is refused the same way.
    gd5_draft_with(&s, "devjob-reviewer: sick\n");
    assert_eq!(
        r.svc.start(item("GD5")).await.unwrap_err().message(),
        "reviewer \"sick\" is unhealthy right now"
    );
    assert!(!runs.exists() || std::fs::read_dir(&runs).unwrap().next().is_none());
    // The tool and the route refuse both spellings at once.
    let both = devjob::DevJobArgs {
        prompt: Some("p".into()),
        branch_slug: Some("x".into()),
        host: Some("studio".into()),
        reviewer: Some("glm".into()),
        reviewers: Some(vec!["codex-write".into()]),
        ..devjob::DevJobArgs::default()
    };
    assert_eq!(
        devjob::plan_start(both, devjob::Agent::Claude).unwrap_err(),
        "pass reviewer or reviewers, not both"
    );
    let dj = devjob_server(&s, &r).await;
    let out = dj
        .call(
            devjob::TOOL_START,
            json!({ "host": "studio", "prompt": "p", "branch_slug": "x",
                    "reviewer": "glm", "reviewers": ["codex-write"] }),
        )
        .await
        .unwrap();
    assert_eq!(out["isError"], true);
    assert!(
        out.to_string()
            .contains("pass reviewer or reviewers, not both"),
        "{out}"
    );
    let out = dj
        .call(
            devjob::TOOL_START,
            json!({ "host": "studio", "prompt": "p", "branch_slug": "x",
                    "reviewers": ["glm", "glm"] }),
        )
        .await
        .unwrap();
    assert!(
        out.to_string()
            .contains("reviewer \\\"glm\\\" is named twice"),
        "{out}"
    );
}

// ---- A DEFAULT REVIEWER THAT IS DOWN -------------------------------------------------------------

#[tokio::test]
async fn a_default_reviewer_that_is_down_starts_the_run_unreviewed_and_its_report_says_so() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let reviewer = FakeReviewer::answering(vec![]);
    let r = default_rig(&s, reviewer.clone(), &["sick"]);
    let started = r.svc.start(item("GD5")).await.unwrap();
    assert_eq!(started.status, RunStatus::Running, "not refused");
    let rec = r.svc.get("GD5").unwrap();
    assert!(rec.reviewers.is_empty() && rec.reviewer.is_none());
    assert_eq!(rec.reviewers_source.as_deref(), Some("default"));
    assert_eq!(
        rec.reviewers_skipped,
        [Skipped {
            reviewer: "sick".into(),
            reason: "reviewer \"sick\" is unhealthy right now".into()
        }]
    );
    // An ordinary run: its prompt finishes as usual, one round, no review.
    let prompt = std::fs::read_to_string(r.svc.settings.run_dir("GD5").join(PROMPT_MD)).unwrap();
    assert!(!prompt.contains(REVIEWED_RUN_SENTENCE));
    commit(&r, "GD5", "a.txt", "a\n", "A");
    finish_round(&r, "GD5", 0, Some("Merged.\n"), "");
    settle(&r).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    assert_eq!(rec.rounds.len(), 1);
    assert!(rec.reviews.is_empty() && lock(&reviewer.calls).is_empty());
    let want = "UNREVIEWED: default reviewer sick unavailable: reviewer \"sick\" is unhealthy \
                right now";
    assert!(
        report_body(&s, &rec, 0).starts_with(&format!("{want}\n\nMerged.\n")),
        "{}",
        report_body(&s, &rec, 0)
    );
    let p = pushes(&r);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].1, want);
    let served = r.svc.served_one(&rec);
    assert_eq!(served["reviewers"], json!([]));
    assert_eq!(served["reviewers_source"], "default");

    // Two defaults, one down: the run is unreviewed, not reviewed by the other one.
    let reviewer = FakeReviewer::answering(vec![Ok(TWO_FINDINGS.into())]);
    let s2 = Scratch::new();
    vault_fixture(&s2);
    remote_fixture(&s2);
    let r = default_rig(&s2, reviewer.clone(), &["codex-write", "nobody"]);
    r.svc.start(item("GD5")).await.unwrap();
    let rec = r.svc.get("GD5").unwrap();
    assert!(rec.reviewers.is_empty() && rec.reviewer.is_none());
    assert_eq!(rec.reviewers_skipped.len(), 1);
    let prompt = std::fs::read_to_string(r.svc.settings.run_dir("GD5").join(PROMPT_MD)).unwrap();
    assert!(!prompt.contains(REVIEWED_RUN_SENTENCE));
    commit(&r, "GD5", "src/calc.rs", "a\n", "Add calc");
    finish_round(&r, "GD5", 0, Some("Merged.\n"), "");
    settle(&r).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    assert_eq!(rec.rounds.len(), 1, "one implementing round, no hand off");
    assert!(rec.reviews.is_empty());
    assert!(lock(&reviewer.calls).is_empty(), "no reviewer was asked");
    assert!(
        report_body(&s2, &rec, 0).starts_with(
            "UNREVIEWED: default reviewer nobody unavailable: reviewer \"nobody\" is not a \
             model this bridge knows\n\nMerged.\n"
        ),
        "{}",
        report_body(&s2, &rec, 0)
    );
}

// ---- SEVERAL REVIEWERS, ONE CYCLE -----------------------------------------------------------------

#[tokio::test]
async fn two_reviewers_review_at_once_and_are_answered_in_one_round_and_one_push() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let reviewer = FakeReviewer::per_reviewer(
        vec![
            ("codex-write", vec![Ok(TWO_FINDINGS.into())]),
            ("glm", vec![Ok(ONE_FINDING.into())]),
        ],
        Some(2),
    );
    let r = review_rig(&s, reviewer.clone());
    r.svc
        .start(with_reviewer(
            item("GD5"),
            vec!["codex-write", "glm"].into(),
        ))
        .await
        .unwrap();
    let dir = r.svc.settings.run_dir("GD5");
    let head = commit(&r, "GD5", "src/calc.rs", "fn sum() {}\n", "Add the sum");
    finish_round(&r, "GD5", 0, Some("Implemented. Draft PR opened.\n"), "");
    settle(&r).await;
    assert!(pushes(&r).is_empty(), "no push before the answering round");

    // Both ran, at once (the barrier fails a review that waited for the other), each in its
    // own worktree at the same head, each with the same prompt.
    let calls = lock(&reviewer.calls).clone();
    assert_eq!(calls.len(), 2);
    let mut by: Vec<(&str, &Path)> = calls
        .iter()
        .map(|c| (c.reviewer.as_str(), c.cwd.as_path()))
        .collect();
    by.sort();
    assert_eq!(
        by,
        [
            ("codex-write", dir.join("review-1").as_path()),
            ("glm", dir.join("review-2").as_path())
        ]
    );
    assert_eq!(calls[0].prompt, calls[1].prompt);
    assert!(calls[0].prompt.starts_with(REVIEWER_PROMPT));

    // Two notes, one cycle, tagged A and B, each id shown with its tag.
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.reviews.len(), 2);
    for (v, tag, who, count) in [
        (&rec.reviews[0], "A", "codex-write", 2),
        (&rec.reviews[1], "B", "glm", 1),
    ] {
        assert_eq!(v.status, ReviewStatus::HandedOff);
        assert_eq!(v.cycle, Some(1));
        assert_eq!(v.tag.as_deref(), Some(tag));
        assert_eq!(v.reviewer, who);
        assert_eq!(v.head.as_deref(), Some(head.as_str()));
        assert_eq!(v.answer_round, Some(2));
        assert_eq!(v.findings.len(), count);
        let note = std::fs::read_to_string(s.notes().join(v.path.as_ref().unwrap())).unwrap();
        assert!(note.contains(&format!("### {tag}.F1 (")), "{note}");
        assert!(note.contains(&format!("reviewer: {who}\n")));
    }
    assert_eq!(
        rec.reviews[0].path.as_deref(),
        Some(format!("Inbox/{}-run-GD5-review-1.md", today()).as_str())
    );
    assert_eq!(
        rec.reviews[1].path.as_deref(),
        Some(format!("Inbox/{}-run-GD5-review-2.md", today()).as_str())
    );

    // ONE hand off, the next round, every finding with an id unique across it.
    assert_eq!(rec.rounds.len(), 2);
    let text = rec.round().unwrap().text.clone().unwrap();
    let glm_findings: Vec<Finding> = parse_review(ONE_FINDING).unwrap().findings;
    let tagged = |t: &str, f: Vec<Finding>| -> Vec<Finding> {
        f.into_iter()
            .map(|x| Finding {
                id: format!("{t}.{}", x.id),
                ..x
            })
            .collect()
    };
    assert_eq!(
        text,
        handoff_reply_cycle(
            &[
                Block {
                    tag: Some("A"),
                    label: "Codex",
                    id: "codex-write",
                    findings: tagged("A", findings())
                },
                Block {
                    tag: Some("B"),
                    label: "GLM 5",
                    id: "glm",
                    findings: tagged("B", glm_findings)
                },
            ],
            &[],
            Some(HANDOFF_FINISH)
        )
    );
    assert!(text.starts_with(HANDOFF_INTRO_MANY));
    assert!(text.contains("## Block A: Codex (codex-write)\n\n### A.F1 (bug) src/calc.rs:3"));
    assert!(text.contains("### A.F2 (nit)"));
    assert!(text.contains("## Block B: GLM 5 (glm)\n\n### B.F1 (risk) src/calc.rs:5"));
    assert!(!text.contains("### F1 "), "no bare id in a cycle of two");
    assert!(!text.contains(PARTLY_REVIEWED_PREFIX));

    // The served JSON while the answering round runs.
    let served = r.svc.served_one(&rec);
    assert_eq!(served["reviewer"], "codex-write");
    assert_eq!(served["reviewer_label"], "Codex + GLM 5");
    assert_eq!(served["reviewers"], json!(["codex-write", "glm"]));
    assert_eq!(served["reviewers_source"], "explicit");
    assert_eq!(served["reviews"].as_array().unwrap().len(), 2);

    // The one answering round: one report, one push, both notes archived and linked.
    commit(
        &r,
        "GD5",
        "src/calc.rs",
        "fn sum() { /* all */ }\n",
        "Sum every element",
    );
    finish_round(
        &r,
        "GD5",
        0,
        Some("Done. PR ready.\n\n| id | verdict | reason |\n|---|---|---|\n| A.F1 | accept | real |\n| A.F2 | reject | equal |\n| B.F1 | accept | real |\n"),
        "",
    );
    settle(&r).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    assert_eq!(
        rec.rounds.len(),
        2,
        "one answering round, never one per reviewer"
    );
    for v in &rec.reviews {
        assert_eq!(v.status, ReviewStatus::Answered);
        assert!(v.leaks.is_empty(), "{:?}", v.leaks);
    }
    let body = report_body(&s, &rec, 1);
    assert!(body.starts_with("Done. PR ready.\n"), "{body}");
    for n in [1, 2] {
        assert!(body.contains(&format!(
            "Review note: [[Inbox/archive/{}-run-GD5-review-{n}]]",
            today()
        )));
    }
    let p = pushes(&r);
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0].1, "Done. PR ready.");
    let served = r.svc.served_one(&rec);
    assert_eq!(served["reviews"][1]["status"], "answered");
    // What the app decodes: a run with two reviews.
    pin_app_fixture(&s, "studio-run-two-reviews-served.json", &served);

    // Never a second cycle.
    r.svc.reply("GD5", "One more thing.").unwrap();
    settle(&r).await;
    finish_round(&r, "GD5", 0, Some("Did it.\n"), "");
    settle(&r).await;
    assert_eq!(r.svc.get("GD5").unwrap().reviews.len(), 2);
    assert_eq!(lock(&reviewer.calls).len(), 2);
}

#[tokio::test]
async fn one_of_two_failing_hands_off_the_other_and_the_report_is_partly_reviewed() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let reviewer = FakeReviewer::per_reviewer(
        vec![
            ("codex-write", vec![Ok(TWO_FINDINGS.into())]),
            (
                "glm",
                vec![Err("the reviewer failed (504): timed out".into())],
            ),
        ],
        None,
    );
    let r = review_rig(&s, reviewer.clone());
    r.svc
        .start(with_reviewer(
            item("GD5"),
            vec!["codex-write", "glm"].into(),
        ))
        .await
        .unwrap();
    commit(&r, "GD5", "src/calc.rs", "a\n", "Add calc");
    finish_round(&r, "GD5", 0, Some("Implemented.\n"), "");
    settle(&r).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.reviews[0].status, ReviewStatus::HandedOff);
    assert_eq!(rec.reviews[1].status, ReviewStatus::Failed);
    assert!(rec.reviews[1].path.is_none());
    assert_eq!(rec.rounds.len(), 2);
    let text = rec.round().unwrap().text.clone().unwrap();
    assert!(text.contains("## Block A: Codex (codex-write)"));
    assert!(!text.contains("Block B"));
    assert!(text.contains(
        "Not every check of your work could run: glm: the reviewer failed (504): timed out. \
         Make the first line of your report PARTLY REVIEWED: followed by exactly that"
    ));
    finish_round(&r, "GD5", 0, Some("Done.\n"), "");
    settle(&r).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    let body = report_body(&s, &rec, 1);
    assert!(
        body.starts_with("PARTLY REVIEWED: glm: the reviewer failed (504): timed out\n\nDone.\n"),
        "{body}"
    );
    assert_eq!(rec.reviews[0].status, ReviewStatus::Answered);
    assert_eq!(rec.reviews[1].status, ReviewStatus::Failed);
    let p = pushes(&r);
    assert_eq!(p.len(), 1);
    assert!(p[0].1.starts_with("PARTLY REVIEWED: glm"), "{p:?}");
}

#[tokio::test]
async fn two_reviewers_that_both_fail_hand_off_unreviewed_once() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let reviewer = FakeReviewer::per_reviewer(
        vec![
            ("codex-write", vec![Err("down".into())]),
            ("glm", vec![Err("also down".into())]),
        ],
        None,
    );
    let r = review_rig(&s, reviewer);
    r.svc
        .start(with_reviewer(
            item("GD5"),
            vec!["codex-write", "glm"].into(),
        ))
        .await
        .unwrap();
    commit(&r, "GD5", "a.txt", "a\n", "A");
    finish_round(&r, "GD5", 0, Some("Implemented.\n"), "");
    settle(&r).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.rounds.len(), 2);
    let why = "codex-write: down; glm: also down";
    assert_eq!(
        rec.round().unwrap().text.as_deref(),
        Some(unreviewed_reply(why, Some(HANDOFF_FINISH)).as_str())
    );
    finish_round(&r, "GD5", 0, Some("Finished.\n"), "");
    settle(&r).await;
    let rec = r.svc.get("GD5").unwrap();
    assert!(report_body(&s, &rec, 1).starts_with(&format!("UNREVIEWED: {why}\n\nFinished.\n")));
}

#[test]
fn the_leak_check_of_a_cycle_covers_every_reviewer_and_every_shown_id() {
    let a = Review {
        reviewer: "codex-write".into(),
        reviewer_label: "Codex".into(),
        findings: findings(),
        tag: Some("A".into()),
        ..Review::default()
    };
    let b = Review {
        reviewer: "glm".into(),
        reviewer_label: "GLM 5".into(),
        findings: parse_review(ONE_FINDING).unwrap().findings,
        tag: Some("B".into()),
        ..Review::default()
    };
    assert_eq!(
        leak_terms_all(&[a, b]),
        [
            "codex-write",
            "Codex",
            "glm",
            "GLM 5",
            "F1",
            "A.F1",
            "F2",
            "A.F2",
            "B.F1",
            "reviewer",
            "code review",
            "review finding"
        ]
    );
}

#[tokio::test]
async fn a_leak_of_the_second_reviewer_is_found_in_the_answering_round() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let reviewer = FakeReviewer::per_reviewer(
        vec![
            ("codex-write", vec![Ok(TWO_FINDINGS.into())]),
            ("glm", vec![Ok(ONE_FINDING.into())]),
        ],
        None,
    );
    let r = review_rig(&s, reviewer);
    r.svc
        .start(with_reviewer(
            item("GD5"),
            vec!["codex-write", "glm"].into(),
        ))
        .await
        .unwrap();
    commit(&r, "GD5", "src/calc.rs", "a\n", "Add calc");
    finish_round(&r, "GD5", 0, Some("Done.\n"), "");
    settle(&r).await;
    let head = commit(
        &r,
        "GD5",
        "src/calc.rs",
        "b\n",
        "Check the overflow GLM 5 saw (B.F1)",
    );
    finish_round(&r, "GD5", 0, Some("Fixed.\n"), "");
    settle(&r).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Reported, "a leak blocks nothing");
    let want = format!("commit {} message: glm, GLM 5, F1, B.F1", &head[..7]);
    assert_eq!(rec.reviews[0].leaks, std::slice::from_ref(&want));
    assert_eq!(rec.reviews[1].leaks, std::slice::from_ref(&want));
    assert!(report_body(&s, &rec, 1).starts_with(&format!("REVIEW LEAK: {want}\n\nFixed.\n")));
}

// ---- AFTER THE FACT, AND THE CLUSTER -------------------------------------------------------------

#[tokio::test]
async fn devjob_review_takes_a_list_runs_one_cycle_and_hands_off_once() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let reviewer = FakeReviewer::per_reviewer(
        vec![
            ("codex-write", vec![Ok(TWO_FINDINGS.into())]),
            ("glm", vec![Ok(ONE_FINDING.into())]),
        ],
        Some(2),
    );
    let r = review_rig(&s, reviewer.clone());
    r.svc.start(free("late")).await.unwrap();
    commit(&r, "s-late", "a.txt", "a\n", "A");
    finish_round(&r, "s-late", 0, Some("Merged.\n"), "");
    settle(&r).await;
    for (arg, want) in [
        (
            ReviewerArg::from("none"),
            "a review needs a reviewer: name a model id",
        ),
        (vec!["glm", "glm"].into(), "reviewer \"glm\" is named twice"),
        (
            vec!["glm", "codex-write", "a", "b"].into(),
            "reviewer names 4 models; the most is 3",
        ),
        (
            vec!["glm", "sick"].into(),
            "reviewer \"sick\" is unhealthy right now",
        ),
    ] {
        assert_eq!(
            r.svc
                .request_review("s-late", arg.clone(), None, true)
                .unwrap_err(),
            Refusal::Invalid(want.into()),
            "{arg:?}"
        );
    }
    let dj = devjob_server(&s, &r).await;
    let out = dj
        .call(
            devjob::TOOL_REVIEW,
            json!({ "run_id": "s-late", "reviewer": ["codex-write", "glm"] }),
        )
        .await
        .unwrap();
    assert_ne!(out["isError"], true, "{out}");
    r.svc.reviews_settled().await;
    settle(&r).await;
    let rec = r.svc.get("s-late").unwrap();
    assert_eq!(lock(&reviewer.calls).len(), 2);
    assert_eq!(rec.reviews.len(), 2);
    assert!(rec
        .reviews
        .iter()
        .all(|v| v.origin == ORIGIN_REQUEST && v.cycle == Some(1) && v.answer_round == Some(2)));
    assert_eq!(rec.rounds.len(), 2, "one hand off for the cycle");
    let text = rec.round().unwrap().text.clone().unwrap();
    assert!(text.starts_with(HANDOFF_INTRO_MANY));
    assert!(
        text.ends_with(&format!("{HANDOFF_RULES}\n")),
        "no finish after the fact"
    );
}

#[test]
fn the_cluster_ignores_the_default_and_refuses_a_named_reviewer() {
    let args = |reviewer: Option<ReviewerArg>, reviewers: Option<Vec<String>>| devjob::DevJobArgs {
        repo: Some("o/n".into()),
        prompt: Some("p".into()),
        branch_slug: Some("x".into()),
        reviewer,
        reviewers,
        ..devjob::DevJobArgs::default()
    };
    // No reviewer: the cluster, as ever; `default_reviewers` never reaches it.
    assert!(matches!(
        devjob::plan_start(args(None, None), devjob::Agent::Claude).unwrap(),
        devjob::StartPlan::Cluster(_)
    ));
    // `none` changes nothing there.
    assert!(matches!(
        devjob::plan_start(args(Some("none".into()), None), devjob::Agent::Claude).unwrap(),
        devjob::StartPlan::Cluster(_)
    ));
    // A reviewer by name is refused, in either spelling.
    for a in [
        args(Some("glm".into()), None),
        args(None, Some(vec!["glm".into()])),
        args(Some(vec!["codex-write", "glm"].into()), None),
    ] {
        let e = devjob::plan_start(a, devjob::Agent::Claude).unwrap_err();
        assert!(
            e.starts_with("reviewer and review_focus apply only with host \"studio\""),
            "{e}"
        );
    }
}

#[test]
fn the_partly_reviewed_line_is_always_the_recorded_one() {
    let failed = ["glm: it timed out".to_string()];
    assert_eq!(
        ensure_partly("Done.\n", &failed),
        "PARTLY REVIEWED: glm: it timed out\n\nDone.\n"
    );
    for wrote in [
        "PARTLY REVIEWED:\n\nDone.\n",
        "PARTLY REVIEWED: something else\nDone.\n",
    ] {
        assert_eq!(
            ensure_partly(wrote, &failed),
            "PARTLY REVIEWED: glm: it timed out\n\nDone.\n",
            "{wrote:?}"
        );
    }
    assert_eq!(
        with_second_line("PARTLY REVIEWED: x\n\nDone.\n", "REVIEW LEAK: y"),
        "PARTLY REVIEWED: x\n\nREVIEW LEAK: y\n\nDone.\n"
    );
}

#[tokio::test]
async fn a_leak_of_the_failed_reviewer_in_a_partly_reviewed_run_keeps_the_status_first() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let reviewer = FakeReviewer::per_reviewer(
        vec![
            ("codex-write", vec![Ok(TWO_FINDINGS.into())]),
            (
                "glm",
                vec![Err("the reviewer failed (504): timed out".into())],
            ),
        ],
        None,
    );
    let r = review_rig(&s, reviewer);
    r.svc
        .start(with_reviewer(
            item("GD5"),
            vec!["codex-write", "glm"].into(),
        ))
        .await
        .unwrap();
    commit(&r, "GD5", "src/calc.rs", "a\n", "Add calc");
    finish_round(&r, "GD5", 0, Some("Implemented.\n"), "");
    settle(&r).await;
    let head = commit(
        &r,
        "GD5",
        "src/calc.rs",
        "b\n",
        "Retry what glm could not check",
    );
    finish_round(&r, "GD5", 0, Some("Done.\n"), "");
    settle(&r).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Reported, "a leak blocks nothing");
    let leak = format!("REVIEW LEAK: commit {} message: glm", &head[..7]);
    let body = report_body(&s, &rec, 1);
    assert!(
        body.starts_with(&format!(
            "PARTLY REVIEWED: glm: the reviewer failed (504): timed out\n\n{leak}\n\nDone.\n"
        )),
        "{body}"
    );
}
