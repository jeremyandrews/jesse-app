use super::*;
use crate::devjob::{
    build_cloud_job, plan_start, render_report_for, render_report_with, report_rel_path,
    report_rel_path_stem, tool_descriptors, tool_descriptors_with_studio, with_cloud, Agent,
    DevJobReport, DevJobToml, JobView, ReportInput, SpecAnnotation, StartPlan, TOOL_CANCEL,
    TOOL_START,
};

// ---- fixtures --------------------------------------------------------------------------

fn deny() -> Vec<String> {
    CloudSettings::default().merge_deny
}

fn args(v: Value) -> DevJobArgs {
    serde_json::from_value(v).unwrap()
}

fn plan(v: Value) -> Result<StartPlan, String> {
    plan_start(args(v), Agent::Claude)
}

fn cloud_start(v: Value) -> CloudStart {
    match plan(v) {
        Ok(StartPlan::Cloud(c)) => c,
        other => panic!("not a cloud start: {other:?}"),
    }
}

fn view(name: &str, created: u64, suspended: bool, finished: Option<bool>) -> JobView {
    JobView {
        name: name.into(),
        slug: name.into(),
        created_ms: created,
        started_ms: None,
        suspended,
        finished,
        finished_ms: None,
        reported: false,
        outcome: None,
        report_path: None,
        pull_request: None,
        origin: "turn".into(),
        spec: SpecAnnotation::default(),
        config_map: None,
        active_deadline_secs: None,
        failure_reason: None,
        run_id: Some(name.into()),
        cancelled: false,
    }
}

fn worker(name: &str, busy: bool, idle_min: u64, expires_min: Option<u64>) -> PoolWorker {
    PoolWorker {
        name: format!("kcb-{name}-1"),
        run: name.into(),
        ready: true,
        busy,
        idle_for: Duration::from_secs(idle_min * 60),
        expires_in: expires_min.map(|m| Duration::from_secs(m * 60)),
        age: None,
    }
}

/// [`worker`] with an age kc-burst reported, so [`remaining_life`] uses it rather than
/// `expires_in`.
fn aged_worker(name: &str, age_min: u64) -> PoolWorker {
    PoolWorker {
        age: Some(Duration::from_secs(age_min * 60)),
        ..worker(name, false, 0, None)
    }
}

const IDLE: Duration = Duration::from_secs(10 * 60);

// ---- settings ----------------------------------------------------------------------------

#[test]
fn the_cloud_is_off_by_default_and_the_deny_list_is_always_seeded() {
    let s = CloudSettings::default();
    assert!(!s.enabled);
    assert_eq!(s.namespace, "jesse-dev");
    assert_eq!(s.max_concurrent, 2);
    assert_eq!(s.burst_size, "s-4vcpu-8gb-intel");
    assert_eq!(
        s.merge_deny,
        ["tag1consulting/scolta*", "tag1consulting/goose"]
    );
    assert_eq!(s.max_timeout_minutes, 315);
    assert_eq!(s.worker_life_minutes, 480);
    assert!(s.problems(false).is_empty(), "off asks for nothing");
    // The defaults fit exactly: 315 + 30 (finish) + 120 (merge waits) + 15 (boot) = 480, so
    // enabling the cloud with nothing else overridden never trips the fit refusal.
    let mut enabled = s.clone();
    enabled.enabled = true;
    let p = enabled.problems(true).join("\n");
    assert!(
        !p.contains("more than worker_life_minutes"),
        "the shipped defaults must fit: {p}"
    );
    // A config's own list adds to the seed and never replaces it.
    let t: DevJobToml = toml::from_str(
        "enabled = true\n[cloud]\nenabled = true\nmerge_deny = [\"acme/secret\", \"tag1consulting/goose\"]\n",
    )
    .unwrap();
    let s = CloudSettings::from_toml(t.cloud, "/home/u");
    assert_eq!(
        s.merge_deny,
        [
            "tag1consulting/scolta*",
            "tag1consulting/goose",
            "acme/secret"
        ]
    );
    assert_eq!(s.kubeconfig, PathBuf::from("/home/u/.kube/kc1-devjob.yaml"));
    assert_eq!(
        s.ledger,
        PathBuf::from("/home/u/.jesse-bridge/devjob-cloud-ledger.json")
    );
    // An unknown key is an error, as everywhere in [devjob].
    assert!(toml::from_str::<DevJobToml>("[cloud]\nnamespace_typo = 1\n").is_err());
}

#[test]
fn the_cloud_refuses_to_boot_on_each_problem_by_name() {
    let t: DevJobToml = toml::from_str(
        "[cloud]\nenabled = true\nkubeconfig = \"/nonexistent/kc.yaml\"\nimage = \"ghcr.io/x:latest\"\n\
         kc_burst = \"/nonexistent/kc-burst\"\nmax_concurrent = 0\nburst_ttl_minutes = 600\n\
         daily_cost_cap_eur = 0.0\nmerge_deny = [\"not a pattern\"]\nmax_timeout_minutes = 0\n\
         worker_life_minutes = 600\n",
    )
    .unwrap();
    let s = CloudSettings::from_toml(t.cloud, "/home/u");
    let p = s.problems(false).join("\n");
    for needle in [
        "needs [devjob] enabled",
        "kubeconfig /nonexistent/kc.yaml is unreadable",
        "is not pinned by digest",
        "max_concurrent must be at least 1",
        "kc_burst /nonexistent/kc-burst is not a file",
        "burst_ttl_minutes must be 30 to 480",
        "daily_cost_cap_eur must be above 0",
        "merge_deny \"not a pattern\"",
        "max_timeout_minutes must be at least 1",
        "worker_life_minutes must be 480 or less: raise kc-burst's max_ttl first",
    ] {
        assert!(p.contains(needle), "{needle} missing from:\n{p}");
    }
    // With [devjob] on, only the "needs" line goes.
    assert!(!s
        .problems(true)
        .join("\n")
        .contains("needs [devjob] enabled"));
}

#[test]
fn the_cloud_refuses_a_timeout_and_worker_life_combination_that_does_not_fit() {
    // At the boundary: 315 + 30 + 120 + 15 = 480, exactly worker_life_minutes. Not refused.
    let fits = CloudSettings {
        enabled: true,
        max_timeout_minutes: 315,
        worker_life_minutes: 480,
        ..CloudSettings::default()
    };
    assert!(!fits
        .problems(true)
        .join("\n")
        .contains("more than worker_life_minutes"));
    // One minute over the boundary: refused, naming the numbers.
    let over = CloudSettings {
        enabled: true,
        max_timeout_minutes: 316,
        worker_life_minutes: 480,
        ..CloudSettings::default()
    };
    let p = over.problems(true).join("\n");
    assert!(
        p.contains("max_timeout_minutes (316) + the finish allowance (30) + the merge waits \
                    (120) + a 15 minute boot allowance = 481 minutes, more than \
                    worker_life_minutes (480)"),
        "{p}"
    );
}

#[test]
fn the_cloud_server_runs_with_kc1s_cluster_settings() {
    let home = DevJobSettings::default();
    let c = CloudSettings {
        enabled: true,
        image: format!(
            "ghcr.io/jeremyandrews/jesse-dev-agent@sha256:{}",
            "a".repeat(64)
        ),
        ..CloudSettings::default()
    };
    let s = c.cluster_settings(&home);
    assert_eq!(s.namespace, "jesse-dev");
    assert_eq!(s.image, c.image);
    assert_eq!(s.max_concurrent, 2);
    assert_eq!(s.kubeconfig, c.kubeconfig);
    assert!(!s.cloud.enabled, "the cloud server has no cloud of its own");
    // The costs the ledger commits: a 4 hour up, a 1 hour extension.
    assert!((c.up_cost() - 0.0767 * 4.0).abs() < 1e-9);
    assert!((c.extend_cost() - 0.0767).abs() < 1e-9);
}

#[test]
fn the_cloud_timeout_defaults_to_two_hours_and_is_capped_at_its_own_max_not_the_clusters() {
    assert_eq!(cloud_capped_timeout_minutes(None, 315), 120);
    assert_eq!(cloud_capped_timeout_minutes(Some(200), 315), 200);
    // Over the cluster's 240 cap, but under the cloud's own 315: granted in full.
    assert_eq!(cloud_capped_timeout_minutes(Some(300), 315), 300);
    // Over the cloud's own cap too: capped there, not at the cluster's 240.
    assert_eq!(cloud_capped_timeout_minutes(Some(10_000), 315), 315);
}

// ---- routing -----------------------------------------------------------------------------

#[test]
fn host_cloud_routes_a_strand_item_a_draft_or_a_free_prompt() {
    let item =
        cloud_start(json!({ "host": "cloud", "strand": "Gd", "item_id": "GD5", "merge": true }));
    assert_eq!(
        item.kind,
        CloudKind::Item {
            strand: "Gd".into(),
            item_id: "GD5".into(),
            repo: None
        }
    );
    assert_eq!(item.merge, Some(true));
    let d = cloud_start(
        json!({ "host": "cloud", "draft": "Projects/drafts/x.md", "repo": "o/n",
                                "test_command": "cargo test", "timeout_minutes": 30 }),
    );
    assert_eq!(
        d.kind,
        CloudKind::Draft {
            draft: "Projects/drafts/x.md".into(),
            repo: Some("o/n".into())
        }
    );
    assert_eq!(d.merge, None, "absent defers to the draft's devjob-merge");
    assert_eq!(d.test_command.as_deref(), Some("cargo test"));
    let f = cloud_start(json!({ "host": "cloud", "repo": "o/n", "prompt": "Fix it.",
                                "branch_slug": "fix-it", "agent": "codex" }));
    match f.kind {
        CloudKind::Free(spec) => {
            assert_eq!(spec.repo, "o/n");
            assert_eq!(spec.agent, Agent::Codex);
            assert_eq!(spec.branch_slug, "fix-it");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn host_cloud_refuses_what_the_cloud_cannot_do() {
    for (v, needle) in [
        (
            json!({ "host": "cloud", "prompt": "x", "branch_slug": "x" }),
            "repo is missing",
        ),
        (
            json!({ "host": "cloud", "repo": "local/scratch", "prompt": "x", "branch_slug": "x" }),
            "local repositories run only on the Studio",
        ),
        (
            json!({ "host": "cloud", "repo": "o/n", "prompt": "x", "branch_slug": "x", "again": true }),
            "again applies only",
        ),
        (
            json!({ "host": "cloud", "repo": "o/n", "prompt": "x", "branch_slug": "x", "reviewer": "codex" }),
            "cloud runs are not reviewed",
        ),
        (
            json!({ "host": "cloud", "strand": "Gd", "item_id": "GD5", "prompt": "x" }),
            "takes its prompt and model from the file",
        ),
        (
            json!({ "host": "cloud", "strand": "Gd" }),
            "strand and item_id go together",
        ),
        (
            json!({ "host": "cloud", "draft": "a.md", "strand": "Gd", "item_id": "GD5" }),
            "not both",
        ),
        (
            json!({ "host": "cloud", "draft": "a.md", "agent": "llama" }),
            "is not one of claude",
        ),
        (
            json!({ "host": "cloud", "draft": "a.md", "timeout_minutes": 0 }),
            "at least 1",
        ),
        (json!({ "host": "moon" }), "is not cluster, studio or cloud"),
    ] {
        let e = plan(v.clone()).unwrap_err();
        assert!(e.contains(needle), "{v}: {e}");
    }
    // `none` is no review, which is what a cloud run gets anyway.
    assert!(plan(json!({ "host": "cloud", "repo": "o/n", "prompt": "x", "branch_slug": "x", "reviewer": "none" })).is_ok());
}

#[test]
fn merge_belongs_to_the_cloud_and_the_cluster_path_is_unchanged_without_it() {
    for host in [json!(null), json!("cluster"), json!("studio")] {
        let mut v = json!({ "repo": "o/n", "prompt": "x", "branch_slug": "x", "merge": true });
        if !host.is_null() {
            v["host"] = host.clone();
        }
        let e = plan(v).unwrap_err();
        assert!(
            e.contains("merge applies only with host \"cloud\""),
            "{host}: {e}"
        );
    }
    // Without merge, no host is the cluster, exactly as before.
    match plan(json!({ "repo": "o/n", "prompt": "x", "branch_slug": "x" })).unwrap() {
        StartPlan::Cluster(spec) => assert_eq!(spec.repo, "o/n"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_draft_declares_where_it_may_run() {
    let d = |host: &str| format!("---\nstrand: Gd\n{host}---\n# x\n");
    assert_eq!(DraftHost::of(&d("")), DraftHost::Unset);
    assert_eq!(
        DraftHost::of(&d("devjob-host: studio\n")),
        DraftHost::Studio
    );
    assert_eq!(DraftHost::of(&d("devjob-host: cloud\n")), DraftHost::Cloud);
    assert_eq!(DraftHost::of(&d("devjob-host: \"any\"\n")), DraftHost::Any);
    assert_eq!(
        DraftHost::of(&d("devjob-host: cluster\n")),
        DraftHost::Other
    );
    let capable: Vec<bool> = [
        "",
        "devjob-host: studio\n",
        "devjob-host: cloud\n",
        "devjob-host: any\n",
    ]
    .iter()
    .map(|h| DraftHost::of(&d(h)).cloud_capable())
    .collect();
    assert_eq!(capable, [false, false, true, true]);
    let drains: Vec<bool> = [
        "",
        "devjob-host: studio\n",
        "devjob-host: cloud\n",
        "devjob-host: any\n",
    ]
    .iter()
    .map(|h| DraftHost::of(&d(h)).drains_to_cloud())
    .collect();
    assert_eq!(
        drains,
        [false, false, false, true],
        "only any drains; no declaration keeps today's queue"
    );
    let e = check_cloud_capable("Projects/drafts/x.md", &d("devjob-host: studio\n")).unwrap_err();
    assert!(e.contains("does not say devjob-host: cloud or any"), "{e}");
    assert!(draft_merge(&d("devjob-merge: true\n")));
    assert!(!draft_merge(&d("devjob-merge: yes\n")));
    assert!(!draft_merge(&d("")));
}

// ---- the deny list and the merge decision -------------------------------------------------

#[test]
fn the_deny_list_matches_exact_names_and_prefixes_without_case() {
    let deny = deny();
    for r in [
        "tag1consulting/scolta",
        "tag1consulting/scolta-php",
        "tag1consulting/scolta.js",
        "Tag1Consulting/Scolta-Drupal",
        "tag1consulting/goose",
        "TAG1CONSULTING/GOOSE",
    ] {
        assert!(denied_by(r, &deny).is_some(), "{r}");
    }
    for r in [
        "tag1consulting/goose-eggs",
        "tag1consulting/gooses",
        "tag1consulting/scol",
        "jeremyandrews/trovato",
        "other/scolta",
    ] {
        assert_eq!(denied_by(r, &deny), None, "{r}");
    }
    assert!(valid_deny_pattern("tag1consulting/scolta*"));
    assert!(valid_deny_pattern("acme/*"));
    assert!(valid_deny_pattern("acme/x.y_z"));
    for bad in ["acme", "acme/", "/x", "acme/x y", "a*/b", "acme/x/y"] {
        assert!(!valid_deny_pattern(bad), "{bad}");
    }
}

#[test]
fn a_run_merges_only_when_asked_and_never_on_the_deny_list() {
    let deny = deny();
    assert_eq!(
        merge_decision("jeremyandrews/trovato", true, &deny),
        MergeDecision::AtGreen
    );
    assert_eq!(
        merge_decision("jeremyandrews/trovato", false, &deny),
        MergeDecision::No
    );
    assert_eq!(
        merge_decision("tag1consulting/scolta-wp", true, &deny),
        MergeDecision::Denied("tag1consulting/scolta*".into())
    );
    assert_eq!(
        merge_decision("tag1consulting/goose", false, &deny),
        MergeDecision::No
    );
    // What the Job is given.
    assert_eq!(MergeDecision::AtGreen.auto_merge_env(), "true");
    assert_eq!(MergeDecision::No.auto_merge_env(), "");
    assert_eq!(MergeDecision::Denied("x/y".into()).auto_merge_env(), "");
    assert_eq!(
        MergeDecision::Denied("x/y".into()).annotation(),
        "denied: x/y"
    );
}

#[test]
fn the_cloud_job_carries_the_merge_guard_and_the_pool_and_nothing_of_the_home_cluster() {
    let spec = DevJobSpec::validate(
        args(json!({ "repo": "tag1consulting/goose", "prompt": "x", "branch_slug": "gd5-x" })),
        Agent::Claude,
    )
    .unwrap();
    let extras = |merge| CloudJobExtras {
        run_id: "GD5".into(),
        merge,
        merge_deny: CloudSettings::default().merge_deny_env(),
    };
    let image = format!(
        "ghcr.io/jeremyandrews/jesse-dev-agent@sha256:{}",
        "b".repeat(64)
    );
    let job = build_cloud_job(
        "jesse-dev",
        "agent-gd5-x-abcde",
        "prompt-gd5-x-abcde",
        &image,
        &spec,
        &extras(MergeDecision::Denied("tag1consulting/goose".into())),
        "turn",
    );
    let v = serde_json::to_value(&job).unwrap();
    assert_eq!(
        v["spec"]["suspend"], true,
        "always queued until a worker is idle"
    );
    assert_eq!(v["metadata"]["labels"][LABEL_RUN_ID], "GD5");
    assert_eq!(
        v["metadata"]["annotations"][ANN_MERGE],
        "denied: tag1consulting/goose"
    );
    let pod = &v["spec"]["template"]["spec"];
    assert_eq!(pod["nodeSelector"][BURST_LABEL], "true");
    assert_eq!(pod["nodeSelector"][POOL_LABEL], "devjob");
    assert_eq!(pod["tolerations"][0]["key"], BURST_LABEL);
    assert_eq!(pod["tolerations"][0]["effect"], "NoSchedule");
    let c = &pod["containers"][0];
    assert_eq!(c["image"], image);
    assert_eq!(c["resources"]["requests"]["cpu"], "3");
    assert_eq!(c["resources"]["requests"]["memory"], "6Gi");
    let env: BTreeMap<String, String> = c["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap().to_string(),
                e["value"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        env["AUTO_MERGE"], "",
        "a denied repository is never told to merge"
    );
    assert_eq!(
        env["MERGE_DENY"],
        "tag1consulting/scolta* tag1consulting/goose"
    );
    assert_eq!(env["AGENT_GITHUB"], "none");
    assert_eq!(env["REBASE_ON_MAIN"], "true");
    assert_eq!(env["RUN_ID"], "GD5");
    assert_eq!(env["JESSE_RUN_REPORT"], RUN_REPORT_FILE);
    assert_eq!(env["CARGO_PROFILE_DEV_DEBUG"], "0");
    // No cache claim on a disposable worker: one emptyDir, the caches under it.
    let vols = pod["volumes"].as_array().unwrap();
    assert!(vols
        .iter()
        .all(|v| v.get("persistentVolumeClaim").is_none()));
    assert_eq!(vols[1]["emptyDir"]["sizeLimit"], "60Gi");
    // The deadline: the agent's two hours, plus the 30 minute finish allowance, plus three CI
    // waits — not the cluster's doubled timeout.
    assert_eq!(v["spec"]["activeDeadlineSeconds"], 7200 + 1800 + 3 * 2400);
    assert_eq!(cloud_deadline_secs(&spec), 7200 + 1800 + 3 * 2400);
    let merging = build_cloud_job(
        "jesse-dev",
        "a",
        "p",
        &image,
        &spec,
        &extras(MergeDecision::AtGreen),
        "turn",
    );
    let v = serde_json::to_value(&merging).unwrap();
    let auto = v["spec"]["template"]["spec"]["containers"][0]["env"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "AUTO_MERGE")
        .unwrap()["value"]
        .clone();
    assert_eq!(auto, "true");
}

// ---- what the agent is told ---------------------------------------------------------------

#[test]
fn the_cloud_agent_is_told_it_has_no_credential_and_how_its_run_ends() {
    let p = cloud_paragraph("GD5", "o/n", "jesse/gd5-x", &MergeDecision::AtGreen);
    for needle in [
        "### Run ID and report",
        "dev job GD5 on kc1",
        "branch jesse/gd5-x, cut from a freshly fetched origin/main",
        "You have no GitHub credential. Do not push, open a pull request or merge",
        "rebases your branch onto a freshly fetched origin/main",
        "other runs may be working on this repository at the same time",
        "titled [GD5]",
        "merges it once every CI run on its exact head commit has passed",
        "JESSE_RUN_REPORT",
        "NEEDS YOU:",
    ] {
        assert!(p.contains(needle), "{needle}");
    }
    let no = cloud_paragraph("GD5", "o/n", "jesse/gd5-x", &MergeDecision::No);
    assert!(no.contains("leaves it open for Jeremy: this run does not merge"));
    let denied = cloud_paragraph(
        "GD5",
        "tag1consulting/goose",
        "jesse/gd5-x",
        &MergeDecision::Denied("tag1consulting/goose".into()),
    );
    assert!(denied.contains("tag1consulting/goose is on the merge deny list"));
    let full = cloud_prompt(
        "Do it.\n\n",
        "GD5",
        "o/n",
        "jesse/gd5-x",
        &MergeDecision::No,
    );
    assert!(full.starts_with("Do it.\n\n### Run ID and report\n\n"));
}

#[test]
fn a_cloud_run_id_never_takes_one_already_used() {
    let taken = ["GD5", "GD5-2", "s-fix"];
    let t = |id: &str| taken.contains(&id);
    assert_eq!(unique_run_id("GD5", t), "GD5-3");
    assert_eq!(unique_run_id("s-fix", t), "s-fix-2");
    assert_eq!(unique_run_id("GD6", t), "GD6");
    assert_eq!(
        cloud_slug("GD5", Some("Run the thing on the Studio.")),
        "gd5-run-the-thing-on-the-studio"
    );
    assert_eq!(cloud_slug("s-fix", None), "s-fix");
    let long = cloud_slug("DJ1", Some(&"word ".repeat(40)));
    assert!(
        long.chars().count() <= crate::devjob::MAX_SLUG_CHARS && crate::devjob::valid_slug(&long),
        "{long}"
    );
}

// ---- slots ---------------------------------------------------------------------------------

#[test]
fn a_cloud_slot_is_free_while_active_and_queued_are_under_the_cap() {
    let running = view("a", 1, false, None);
    let queued = view("b", 2, true, None);
    let done = view("c", 0, false, Some(true));
    assert!(cloud_slot_free(&[], 2));
    assert!(cloud_slot_free(&[running.clone(), done.clone()], 2));
    assert!(
        !cloud_slot_free(&[running.clone(), queued.clone()], 2),
        "a queued one holds a slot"
    );
    assert!(!cloud_slot_free(&[running.clone(), running.clone()], 2));
}

#[test]
fn a_queued_cloud_job_starts_only_onto_an_idle_ready_worker() {
    let views = vec![view("old", 1, true, None), view("new", 2, true, None)];
    let life = 480;
    let one = [worker("w1", false, 0, Some(999))];
    let two = [worker("w1", false, 0, Some(999)), worker("w2", false, 0, Some(999))];
    // No worker: nothing starts, however free the slots.
    assert!(cloud_resumable(&views, 2, &[], 0, life).is_empty());
    // One idle worker: the oldest only.
    assert_eq!(cloud_resumable(&views, 2, &one, 0, life), ["old"]);
    // Two: both, oldest first.
    assert_eq!(cloud_resumable(&views, 2, &two, 0, life), ["old", "new"]);
    // A released Job still waiting for its pod to be placed takes one of them.
    assert_eq!(cloud_resumable(&views, 2, &two, 1, life), ["old"]);
    // The cap still holds: one running, cap two, two idle workers: one more.
    let mut v = views.clone();
    v.push(view("busy", 0, false, None));
    assert_eq!(cloud_resumable(&v, 2, &two, 0, life), ["old"]);
}

#[test]
fn cloud_resumable_skips_a_worker_whose_remaining_life_is_short_of_the_jobs_deadline() {
    let job = JobView {
        active_deadline_secs: Some(3600),
        ..view("gd5", 1, true, None)
    };
    let views = vec![job];
    // 470 of 480 minutes already spent: 10 minutes left, short of the Job's 60 minute deadline.
    let old = aged_worker("old", 470);
    assert!(
        cloud_resumable(&views, 2, &[old], 0, 480).is_empty(),
        "stays queued: no idle worker outlives it"
    );
    // 10 minutes old: 470 left, plenty.
    let young = aged_worker("young", 10);
    assert_eq!(cloud_resumable(&views, 2, &[young], 0, 480), ["gd5"]);
}

#[test]
fn an_idle_worker_with_no_known_age_is_sized_by_its_current_expiry() {
    let job = JobView {
        active_deadline_secs: Some(3600),
        ..view("gd5", 1, true, None)
    };
    let views = vec![job];
    // No age reported: 50 minutes left by kc-burst's own `expires`, short of the deadline.
    let short = worker("w", false, 0, Some(50));
    assert!(cloud_resumable(&views, 2, &[short], 0, 480).is_empty());
    // 70 minutes left: covers it.
    let enough = worker("w2", false, 0, Some(70));
    assert_eq!(cloud_resumable(&views, 2, &[enough], 0, 480), ["gd5"]);
}

#[test]
fn remaining_life_prefers_age_over_expires_and_falls_back_when_age_is_unknown() {
    let w = aged_worker("a", 100);
    assert_eq!(
        remaining_life(&w, 480),
        Duration::from_secs(380 * 60),
        "480 - 100"
    );
    let no_age = worker("b", false, 0, Some(45));
    assert_eq!(remaining_life(&no_age, 480), Duration::from_secs(45 * 60));
    let unknown = worker("c", false, 0, None);
    assert_eq!(remaining_life(&unknown, 480), Duration::ZERO);
}

// ---- burst workers -------------------------------------------------------------------------

#[test]
fn workers_are_started_for_demand_up_to_the_cap_and_never_twice() {
    // Nothing to do, nothing running: nothing.
    assert_eq!(plan_burst(&[], 0, 0, 2, IDLE), BurstPlan::default());
    // One queued run: one worker.
    assert_eq!(plan_burst(&[], 0, 1, 2, IDLE).up, 1);
    // Five queued: two, the cap.
    assert_eq!(plan_burst(&[], 0, 5, 2, IDLE).up, 2);
    // One already starting counts.
    assert_eq!(plan_burst(&[], 1, 5, 2, IDLE).up, 1);
    // One up and busy, one starting: none more.
    assert_eq!(
        plan_burst(&[worker("aaaaaa", true, 0, Some(200))], 1, 5, 2, IDLE).up,
        0
    );
}

#[test]
fn an_idle_worker_is_stopped_only_after_idle_minutes_and_only_when_nothing_wants_it() {
    let w = [worker("aaaaaa", false, 9, Some(200))];
    // Idle 9 minutes of 10: kept.
    assert!(plan_burst(&w, 0, 0, 2, IDLE).down.is_empty());
    let w = [worker("aaaaaa", false, 10, Some(200))];
    assert_eq!(plan_burst(&w, 0, 0, 2, IDLE).down, ["aaaaaa"]);
    // A queued run wants it: kept, however long idle.
    assert!(plan_burst(&w, 0, 1, 2, IDLE).down.is_empty());
    // Two idle, one wanted: the longest idle goes.
    let w = [
        worker("aaaaaa", false, 12, None),
        worker("bbbbbb", false, 30, None),
    ];
    assert_eq!(plan_burst(&w, 0, 1, 2, IDLE).down, ["bbbbbb"]);
    // A busy worker is never stopped.
    let w = [worker("aaaaaa", true, 0, None)];
    assert!(plan_burst(&w, 0, 0, 2, IDLE).down.is_empty());
}

#[test]
fn a_busy_worker_near_its_expiry_is_extended_and_an_idle_one_is_not() {
    let w = [
        worker("aaaaaa", true, 0, Some(29)),
        worker("bbbbbb", true, 0, Some(31)),
        worker("cccccc", false, 0, Some(5)),
        worker("dddddd", true, 0, None),
    ];
    assert_eq!(plan_burst(&w, 0, 3, 4, IDLE).extend, ["aaaaaa"]);
}

#[tokio::test(start_paused = true)]
async fn idle_time_runs_on_the_tokio_clock_and_resets_when_a_pod_lands() {
    let mut c = IdleClock::default();
    let a = "kcb-aaaaaa-1".to_string();
    assert_eq!(c.observe(&[(a.clone(), false)])[&a], Duration::ZERO);
    tokio::time::advance(Duration::from_secs(9 * 60)).await;
    assert_eq!(
        c.observe(&[(a.clone(), false)])[&a],
        Duration::from_secs(9 * 60)
    );
    // A pod lands: busy, and idleness starts again from zero once it goes.
    assert_eq!(c.observe(&[(a.clone(), true)])[&a], Duration::ZERO);
    tokio::time::advance(Duration::from_secs(60)).await;
    assert_eq!(c.observe(&[(a.clone(), false)])[&a], Duration::ZERO);
    tokio::time::advance(Duration::from_secs(10 * 60)).await;
    let idle = c.observe(&[(a.clone(), false)])[&a];
    assert_eq!(idle, IDLE);
    // Which is exactly when the plan stops it.
    let w = [PoolWorker {
        name: a.clone(),
        run: "aaaaaa".into(),
        ready: true,
        busy: false,
        idle_for: idle,
        expires_in: None,
        age: None,
    }];
    assert_eq!(plan_burst(&w, 0, 0, 2, IDLE).down, ["aaaaaa"]);
    // A worker that is gone is forgotten.
    assert!(c.observe(&[]).is_empty());
}

#[test]
fn the_ledger_is_a_hard_daily_cap_that_starts_again_each_day() {
    let mut l = Ledger::default();
    assert!(l.try_commit("2026-11-23", 0.30, 1.0));
    assert!(l.try_commit("2026-11-23", 0.30, 1.0));
    assert!(l.try_commit("2026-11-23", 0.30, 1.0));
    assert!(
        !l.try_commit("2026-11-23", 0.30, 1.0),
        "the fourth would pass the cap"
    );
    assert!((l.committed_eur - 0.90).abs() < 1e-9);
    l.refund("2026-11-23", 0.30);
    assert!(l.try_commit("2026-11-23", 0.30, 1.0));
    // Yesterday's refund does not touch today; a new day starts at zero.
    l.refund("2026-11-22", 5.0);
    assert!((l.committed_eur - 0.90).abs() < 1e-9);
    assert!(l.try_commit("2026-11-24", 1.0, 1.0));
    assert!((l.committed_eur - 1.0).abs() < 1e-9);
    // On disk, and back.
    let dir = std::env::temp_dir().join(format!("jesse-cloud-ledger-{}", crate::random_hex()));
    let path = dir.join("nested/ledger.json");
    l.save(&path).unwrap();
    assert_eq!(Ledger::load(&path), l);
    assert_eq!(Ledger::load(&dir.join("missing.json")), Ledger::default());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn kc_bursts_json_lines_parse() {
    let status = r#"{"name":"kcb-abcdef-1","provider":"digitalocean","run":"abcdef","id":"1","age":"5m","age_seconds":300,"time_left":"3h55m","expires":1795440000,"node_status":"Ready","hourly_eur":0.0767,"cost_so_far_eur":0.0064,"cost_full_ttl_eur":0.3068}
{"cost_full_ttl_eur":0.3068,"cost_so_far_eur":0.0064,"workers":1,"tailnet_orphans":0}"#;
    assert_eq!(
        parse_status(status),
        [BurstWorker {
            name: "kcb-abcdef-1".into(),
            run: "abcdef".into(),
            expires: Some(1795440000),
            age_seconds: Some(300),
            cost_so_far_eur: 0.0064,
        }]
    );
    assert!(parse_status(r#"{"workers":0}"#).is_empty());
    // No `age_seconds` at all: an older kc-burst, or a worker just up: unknown, not zero.
    let no_age = r#"{"name":"kcb-abcdef-1","run":"abcdef","expires":1795440000,"cost_so_far_eur":0.0064}"#;
    assert_eq!(parse_status(no_age)[0].age_seconds, None);
    let up =
        r#"{"name":"kcb-qwerty-1","run":"qwerty","public_ipv4":"1.2.3.4","node_status":"Ready"}"#;
    assert_eq!(parse_up(up).as_deref(), Some("qwerty"));
    assert_eq!(parse_up("progress\n{}"), None);
}

// ---- reports and tools ---------------------------------------------------------------------

#[test]
fn a_cloud_report_is_named_and_marked_like_a_studio_run_and_the_home_report_is_unchanged() {
    let spec = SpecAnnotation {
        repo: "o/n".into(),
        agent: "claude".into(),
        branch_slug: "gd5-x".into(),
        timeout_minutes: 120,
        ..SpecAnnotation::default()
    };
    let report = DevJobReport {
        status: "ok".into(),
        pull_request: Some("https://github.com/o/n/pull/7".into()),
        ..DevJobReport::default()
    };
    let input = ReportInput {
        job_name: "agent-gd5-x-abcde",
        slug: "gd5-x",
        spec: &spec,
        prompt: Some("Do it."),
        report: &report,
        log_tail: &[],
        queued_ms: 0,
        started_ms: None,
        finished_ms: None,
    };
    assert_eq!(
        render_report_with(&input, None, None),
        render_report_for(&input, None)
    );
    let c = render_report_with(&input, None, Some("GD5"));
    assert!(c.starts_with(
        "---\ndevjob: agent-gd5-x-abcde\nrepo: o/n\noutcome: pull request opened\nhost: cloud\nrun: GD5\n---\n\n# Cloud run GD5: pull request opened\n"
    ), "{c}");
    assert_eq!(
        report_rel_path_stem("2026-11-23", "run-GD5", |_| false),
        "Inbox/2026-11-23-run-GD5.md"
    );
    assert_eq!(
        report_rel_path("2026-11-23", "gd5-x", |_| false),
        "Inbox/2026-11-23-dev-job-gd5-x.md",
        "the home cluster's name is unchanged"
    );
}

#[test]
fn the_tools_name_the_cloud_only_when_it_is_attached() {
    let names = |t: &[Value]| -> Vec<String> {
        t.iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect()
    };
    for base in [tool_descriptors(), tool_descriptors_with_studio()] {
        let before = base.clone();
        let with = with_cloud(base);
        let start = with.iter().find(|t| t["name"] == TOOL_START).unwrap();
        let hosts = start["inputSchema"]["properties"]["host"]["enum"]
            .as_array()
            .unwrap();
        assert!(hosts.contains(&json!("cloud")));
        assert!(hosts.contains(&json!("cluster")));
        assert_eq!(
            start["inputSchema"]["properties"]["merge"]["type"],
            "boolean"
        );
        assert!(start["description"]
            .as_str()
            .unwrap()
            .contains("host \"cloud\""));
        assert!(names(&with).contains(&TOOL_CANCEL.to_string()));
        // Every tool the base listed is still listed, in its order.
        let n = names(&with);
        assert_eq!(&n[..before.len()], names(&before).as_slice());
    }
    // Without the cloud the cluster's three are exactly what they were.
    assert_eq!(names(&tool_descriptors()), crate::devjob::DEVJOB_TOOLS);
}
