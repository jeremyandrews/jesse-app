use super::*;
use crate::devjob::{
    build_cloud_job, plan_start, render_report_for, render_report_with, report_rel_path,
    report_rel_path_stem, tool_descriptors, tool_descriptors_with_studio, with_cloud, Agent,
    ChainExtras, DevJobReport, DevJobToml, JobView, ReportInput, SpecAnnotation, StartPlan,
    TOOL_CANCEL, TOOL_START,
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
        strand: None,
        strand_id: None,
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

/// The Jobs a [`Placement`] resumes, in order.
fn jobs(p: &Placement) -> Vec<&str> {
    p.resume.iter().map(|(j, _)| j.as_str()).collect()
}

// ---- settings ----------------------------------------------------------------------------

#[test]
fn the_cloud_is_off_by_default_and_the_deny_list_is_always_seeded() {
    let s = CloudSettings::default();
    assert!(!s.enabled);
    assert_eq!(s.namespace, "jesse-dev");
    assert_eq!(s.max_concurrent, 2);
    assert_eq!(s.burst_size, "g-4vcpu-16gb");
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
    // A config without the resource keys gets the defaults sized for the 16 GB worker.
    assert_eq!(s.job_memory_gi, 14);
    assert_eq!(s.job_memory_request_gi, 12);
    assert_eq!(s.job_cpu, 4);
    assert_eq!(s.job_cpu_request, 3);
    assert_eq!(s.max_timeout_minutes, 315);
    assert!(s.resource_problems().is_empty());
    // An unknown key is an error, as everywhere in [devjob].
    assert!(toml::from_str::<DevJobToml>("[cloud]\nnamespace_typo = 1\n").is_err());
}

#[test]
fn the_job_resources_and_time_cap_are_read_from_config() {
    let t: DevJobToml = toml::from_str(
        "[cloud]\nburst_size = \"g-8vcpu-32gb\"\njob_memory_gi = 30\njob_memory_request_gi = 28\n\
         job_cpu = 8\njob_cpu_request = 7\nmax_agent_minutes = 300\n",
    )
    .unwrap();
    let s = CloudSettings::from_toml(t.cloud, "/home/u");
    assert_eq!(
        s.job_resources(),
        JobResources {
            memory_gi: 30,
            memory_request_gi: 28,
            cpu: 8,
            cpu_request: 7,
        }
    );
    // Bridge 0.219.0's name for the cap still sets it.
    assert_eq!(s.max_timeout_minutes, 300);
    assert!(s.resource_problems().is_empty());
    // The cloud's cap, not the cluster's 240: absent is the default 120, more is capped.
    let cap = |asked| cloud_capped_timeout_minutes(asked, s.max_timeout_minutes);
    assert_eq!(cap(None), 120);
    assert_eq!(cap(Some(280)), 280);
    assert_eq!(cap(Some(900)), 300);
    assert_eq!(
        cloud_capped_timeout_minutes(Some(400), CloudSettings::default().max_timeout_minutes),
        315
    );
}

#[test]
fn a_config_pinned_to_the_8gb_worker_keeps_a_job_that_fits_it() {
    // Only burst_size, as configs written before the resources came from config.
    let t: DevJobToml =
        toml::from_str("[cloud]\nenabled = true\nburst_size = \"s-4vcpu-8gb-intel\"\n").unwrap();
    let s = CloudSettings::from_toml(t.cloud, "/home/u");
    assert_eq!(s.job_resources(), SMALL_WORKER_RESOURCES);
    assert_eq!(s.job_memory_request_gi, 6);
    assert_eq!(s.job_memory_gi, 7);
    assert!(
        s.resource_problems().is_empty(),
        "{:?}",
        s.resource_problems()
    );
    // The default size gets the 16 GB defaults.
    assert_eq!(
        CloudSettings::default().job_resources(),
        JobResources {
            memory_gi: 14,
            memory_request_gi: 12,
            cpu: 4,
            cpu_request: 3,
        }
    );
    // A request the named worker cannot hold is refused by name.
    let t: DevJobToml = toml::from_str(
        "[cloud]\nburst_size = \"s-4vcpu-8gb-intel\"\njob_memory_gi = 14\njob_memory_request_gi = 12\n\
         job_cpu = 8\njob_cpu_request = 6\n",
    )
    .unwrap();
    let p = CloudSettings::from_toml(t.cloud, "/home/u")
        .resource_problems()
        .join("\n");
    assert!(
        p.contains("job_memory_request_gi 12 does not fit burst_size \"s-4vcpu-8gb-intel\" (8 GB)"),
        "{p}"
    );
    assert!(
        p.contains("job_cpu_request 6 does not fit burst_size \"s-4vcpu-8gb-intel\" (4 vCPU)"),
        "{p}"
    );
    assert_eq!(size_shape("g-4vcpu-16gb"), (Some(16), Some(4)));
    assert_eq!(size_shape("custom"), (None, None));
}

#[test]
fn the_job_resources_are_refused_when_inconsistent() {
    let t: DevJobToml = toml::from_str(
        "[cloud]\nenabled = true\njob_memory_gi = 8\njob_memory_request_gi = 10\n\
         job_cpu = 2\njob_cpu_request = 3\n",
    )
    .unwrap();
    let s = CloudSettings::from_toml(t.cloud, "/home/u");
    let p = s.problems(true).join("\n");
    for needle in [
        "job_memory_request_gi 10 is above job_memory_gi 8",
        "job_cpu_request 3 is above job_cpu 2",
    ] {
        assert!(p.contains(needle), "{needle} missing from:\n{p}");
    }
    let t: DevJobToml = toml::from_str(
        "[cloud]\nenabled = true\njob_memory_gi = 0\njob_memory_request_gi = 0\n\
         job_cpu = 0\njob_cpu_request = 0\n",
    )
    .unwrap();
    let p = CloudSettings::from_toml(t.cloud, "/home/u")
        .problems(true)
        .join("\n");
    for key in [
        "job_memory_gi",
        "job_memory_request_gi",
        "job_cpu",
        "job_cpu_request",
    ] {
        assert!(
            p.contains(&format!("{key} must be at least 1")),
            "{key} missing from:\n{p}"
        );
    }
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
        p.contains(
            "max_timeout_minutes (316) + the finish allowance (30) + the merge waits \
                    (120) + a 15 minute boot allowance = 481 minutes, more than \
                    worker_life_minutes (480)"
        ),
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
    // A cap below the default holds for a start that names no timeout.
    assert_eq!(cloud_capped_timeout_minutes(None, 60), 60);
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
        resources: CloudSettings::default().job_resources(),
        services: CloudSettings::default().services,
        chain: None,
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
    // One dev job per worker, whatever the requests: no two cloud pods share a node.
    let anti =
        &pod["affinity"]["podAntiAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"][0];
    assert_eq!(anti["topologyKey"], "kubernetes.io/hostname");
    assert_eq!(
        anti["labelSelector"]["matchExpressions"][0]["key"],
        LABEL_RUN_ID
    );
    assert_eq!(
        anti["labelSelector"]["matchExpressions"][0]["operator"],
        "Exists"
    );
    assert_eq!(
        v["spec"]["template"]["metadata"]["labels"][LABEL_RUN_ID], "GD5",
        "the pod carries the label its own rule selects"
    );
    assert_eq!(pod["tolerations"][0]["effect"], "NoSchedule");
    let c = &pod["containers"][0];
    assert_eq!(c["image"], image);
    // The defaults, sized for the 16 GB worker, less Postgres's and Redis's share.
    assert_eq!(c["resources"]["requests"]["cpu"], "2250m");
    assert_eq!(c["resources"]["requests"]["memory"], "11648Mi");
    assert_eq!(c["resources"]["limits"]["cpu"], "3250m");
    assert_eq!(c["resources"]["limits"]["memory"], "13056Mi");
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
    assert_eq!(
        env["CARGO_BUILD_JOBS"], "3",
        "no more rustcs than the agent has CPUs"
    );
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
    let two = [
        worker("w1", false, 0, Some(999)),
        worker("w2", false, 0, Some(999)),
    ];
    // No worker: nothing starts, however free the slots.
    assert!(jobs(&cloud_resumable(&views, 2, &[], 0, life)).is_empty());
    // One idle worker: the oldest only.
    assert_eq!(jobs(&cloud_resumable(&views, 2, &one, 0, life)), ["old"]);
    // Two: both, oldest first.
    assert_eq!(
        jobs(&cloud_resumable(&views, 2, &two, 0, life)),
        ["old", "new"]
    );
    // A released Job still waiting for its pod to be placed takes one of them.
    assert_eq!(jobs(&cloud_resumable(&views, 2, &two, 1, life)), ["old"]);
    // The cap still holds: one running, cap two, two idle workers: one more.
    let mut v = views.clone();
    v.push(view("busy", 0, false, None));
    assert_eq!(jobs(&cloud_resumable(&v, 2, &two, 0, life)), ["old"]);
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
        jobs(&cloud_resumable(&views, 2, &[old], 0, 480)).is_empty(),
        "stays queued: no idle worker outlives it"
    );
    // 10 minutes old: 470 left, plenty.
    let young = aged_worker("young", 10);
    assert_eq!(jobs(&cloud_resumable(&views, 2, &[young], 0, 480)), ["gd5"]);
}

#[test]
fn a_resumed_job_is_pinned_to_the_worker_that_outlives_it() {
    let job = JobView {
        active_deadline_secs: Some(3600),
        ..view("gd5", 1, true, None)
    };
    // Listed first, the old one has 10 minutes left; the young one 470.
    let idle = [aged_worker("old", 470), aged_worker("young", 10)];
    let p = cloud_resumable(&[job], 2, &idle, 0, 480);
    assert_eq!(p.resume, [("gd5".to_string(), "kcb-young-1".to_string())]);
    assert!(p.spent.is_empty(), "every Job found a worker");
    // The resume carries the pin, so the scheduler cannot pick the old worker instead.
    let patch = pin_to_worker("kcb-young-1");
    assert_eq!(patch["spec"]["suspend"], false);
    assert_eq!(
        patch["spec"]["template"]["spec"]["affinity"]["nodeAffinity"]
            ["requiredDuringSchedulingIgnoredDuringExecution"]["nodeSelectorTerms"][0]
            ["matchFields"][0],
        json!({ "key": "metadata.name", "operator": "In", "values": ["kcb-young-1"] })
    );
}

#[test]
fn a_queued_job_too_long_for_the_only_idle_worker_gets_a_fresh_one() {
    let job = JobView {
        active_deadline_secs: Some(3600),
        ..view("gd5", 1, true, None)
    };
    let old = aged_worker("old", 470);
    let p = cloud_resumable(&[job], 2, std::slice::from_ref(&old), 0, 480);
    assert!(p.resume.is_empty());
    assert_eq!(p.spent, ["kcb-old-1"]);
    // Plain plan_burst counts the old worker as the queue's capacity and starts nothing.
    assert_eq!(plan_burst(std::slice::from_ref(&old), 0, 1, 2, IDLE).up, 0);
    // Left out as spent, the queued Job gets a fresh worker at once.
    let plan = plan_pass(std::slice::from_ref(&old), &p.spent, 0, 1, 2, IDLE);
    assert_eq!(plan.up, 1);
    assert!(plan.down.is_empty(), "not idle long enough yet");
    // Once idle the configured minutes, the spent worker is stopped.
    let idle_old = PoolWorker {
        idle_for: IDLE,
        ..old.clone()
    };
    assert_eq!(
        plan_pass(&[idle_old], &p.spent, 0, 1, 2, IDLE).down,
        ["old"]
    );
    // A worker that does cover a queued Job is never spent.
    let young = aged_worker("young", 10);
    let job = JobView {
        active_deadline_secs: Some(3600),
        ..view("gd5", 1, true, None)
    };
    assert!(cloud_resumable(&[job], 2, &[young], 0, 480)
        .spent
        .is_empty());
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
    assert!(jobs(&cloud_resumable(&views, 2, &[short], 0, 480)).is_empty());
    // 70 minutes left: covers it.
    let enough = worker("w2", false, 0, Some(70));
    assert_eq!(
        jobs(&cloud_resumable(&views, 2, &[enough], 0, 480)),
        ["gd5"]
    );
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
    let no_age =
        r#"{"name":"kcb-abcdef-1","run":"abcdef","expires":1795440000,"cost_so_far_eur":0.0064}"#;
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
        let with = with_cloud(base, 315);
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
        // The cloud's time cap is the config's, and says so.
        assert!(start["description"]
            .as_str()
            .unwrap()
            .contains("capped at 315 by the bridge's [devjob.cloud] max_timeout_minutes"));
        assert!(
            start["inputSchema"]["properties"]["timeout_minutes"]["description"]
                .as_str()
                .unwrap()
                .contains("with host cloud, at 315 from [devjob.cloud] max_timeout_minutes")
        );
        assert!(names(&with).contains(&TOOL_CANCEL.to_string()));
        // Every tool the base listed is still listed, in its order.
        let n = names(&with);
        assert_eq!(&n[..before.len()], names(&before).as_slice());
    }
    // Without the cloud the cluster's three are exactly what they were.
    assert_eq!(names(&tool_descriptors()), crate::devjob::DEVJOB_TOOLS);
    // And none of them offers a host this bridge does not have.
    for base in [tool_descriptors(), tool_descriptors_with_studio()] {
        let text = serde_json::to_string(&base).unwrap();
        assert!(!text.contains("cloud"), "{text}");
    }
}

// ---- resources from config, and a memory kill ---------------------------------------------

#[test]
fn the_cloud_job_takes_its_resources_from_config_and_never_literals() {
    let spec = DevJobSpec::validate(
        args(json!({ "repo": "jeremyandrews/trovato", "prompt": "x", "branch_slug": "dj17-x" })),
        Agent::Claude,
    )
    .unwrap();
    let extras = CloudJobExtras {
        run_id: "DJ17".into(),
        merge: MergeDecision::No,
        merge_deny: String::new(),
        resources: JobResources {
            memory_gi: 30,
            memory_request_gi: 28,
            cpu: 8,
            cpu_request: 7,
        },
        services: false,
        chain: None,
    };
    let job = build_cloud_job("jesse-dev", "a", "p", "img", &spec, &extras, "turn");
    let v = serde_json::to_value(&job).unwrap();
    let c = &v["spec"]["template"]["spec"]["containers"][0];
    assert_eq!(c["resources"]["requests"]["memory"], "28Gi");
    assert_eq!(c["resources"]["requests"]["cpu"], "7");
    assert_eq!(c["resources"]["requests"]["ephemeral-storage"], "20Gi");
    assert_eq!(c["resources"]["limits"]["memory"], "30Gi");
    assert_eq!(c["resources"]["limits"]["cpu"], "8");
    let jobs = c["env"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "CARGO_BUILD_JOBS")
        .unwrap()["value"]
        .clone();
    assert_eq!(jobs, "8");
}

fn oom_pod(reason: &str, last: bool) -> k8s_openapi::api::core::v1::Pod {
    let terminated = json!({ "terminated": { "exitCode": 137, "reason": reason } });
    let (state, last_state) = if last {
        (
            json!({ "waiting": { "reason": "CrashLoopBackOff" } }),
            terminated,
        )
    } else {
        (terminated, json!({}))
    };
    serde_json::from_value(json!({
        "metadata": { "name": "agent-dj17-x-abcde-xyz" },
        "spec": { "containers": [{
            "name": "agent",
            "image": "img",
            "resources": { "limits": { "cpu": "4", "memory": "14Gi" } },
        }] },
        "status": { "containerStatuses": [{
            "name": "agent",
            "image": "img",
            "imageID": "",
            "ready": false,
            "restartCount": 0,
            "state": state,
            "lastState": last_state,
        }] },
    }))
    .unwrap()
}

#[test]
fn a_cloud_run_killed_for_memory_says_so_in_its_failure_line() {
    let pod = oom_pod("OOMKilled", false);
    let line = crate::devjob::oom_killed_line(&pod).unwrap();
    assert_eq!(line, "killed at the 14 Gi memory limit (OOMKilled)");
    assert_eq!(
        crate::devjob::oom_killed_line(&oom_pod("OOMKilled", true)).as_deref(),
        Some("killed at the 14 Gi memory limit (OOMKilled)"),
        "the last state counts as well"
    );
    assert_eq!(
        crate::devjob::oom_killed_line(&oom_pod("Error", false)),
        None
    );

    // No report in the log, the Job failed at its backoff limit: the kill leads the line.
    let mut report = DevJobReport::missing("no report in the log");
    crate::devjob::explain_failure(
        &mut report,
        Some("BackoffLimitExceeded: Job has reached the specified backoff limit"),
        Some(&line),
    );
    assert_eq!(report.status, "failed");
    assert_eq!(
        report.failure.as_deref(),
        Some(
            "killed at the 14 Gi memory limit (OOMKilled); the Job failed \
             (BackoffLimitExceeded: Job has reached the specified backoff limit); \
             no report in the log"
        )
    );
    // Without a memory kill the line is exactly what it was.
    let mut report = DevJobReport::missing("no report in the log");
    crate::devjob::explain_failure(&mut report, Some("BackoffLimitExceeded"), None);
    assert_eq!(
        report.failure.as_deref(),
        Some("the Job failed (BackoffLimitExceeded); no report in the log")
    );
}

// ---- Postgres and Redis beside the agent ------------------------------------------------

/// The live kc1 size: 28 Gi and 8 CPU limits, 26 Gi and 7 CPU requests.
fn live_resources() -> JobResources {
    JobResources {
        memory_gi: 28,
        memory_request_gi: 26,
        cpu: 8,
        cpu_request: 7,
    }
}

fn trovato_job(services: bool) -> Value {
    let spec = DevJobSpec::validate(
        args(json!({ "repo": "jeremyandrews/trovato", "prompt": "x", "branch_slug": "dj25-x" })),
        Agent::Claude,
    )
    .unwrap();
    let extras = CloudJobExtras {
        run_id: "DJ25".into(),
        merge: MergeDecision::AtGreen,
        merge_deny: CloudSettings::default().merge_deny_env(),
        resources: live_resources(),
        services,
        chain: None,
    };
    let job = build_cloud_job(
        "jesse-dev",
        "agent-dj25-x-abcde",
        "prompt-dj25-x-abcde",
        "img",
        &spec,
        &extras,
        "turn",
    );
    serde_json::to_value(&job).unwrap()
}

fn mebibytes(q: &Value) -> u32 {
    let q = q.as_str().unwrap();
    match (q.strip_suffix("Gi"), q.strip_suffix("Mi")) {
        (Some(n), _) => n.parse::<u32>().unwrap() * 1024,
        (_, Some(n)) => n.parse().unwrap(),
        _ => panic!("not Gi or Mi: {q}"),
    }
}

fn millicores(q: &Value) -> u32 {
    let q = q.as_str().unwrap();
    match q.strip_suffix('m') {
        Some(n) => n.parse().unwrap(),
        None => q.parse::<u32>().unwrap() * 1000,
    }
}

fn env_of(c: &Value) -> BTreeMap<String, String> {
    c["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap().to_string(),
                e["value"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn a_cloud_pod_runs_postgres_and_redis_as_native_sidecars_before_the_agent() {
    let v = trovato_job(true);
    let pod = &v["spec"]["template"]["spec"];
    let init = pod["initContainers"].as_array().unwrap();
    let names: Vec<&str> = init.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["postgres", "redis"], "Postgres first, then Redis");
    for c in init {
        assert_eq!(c["restartPolicy"], "Always", "a native sidecar");
        assert_eq!(c["imagePullPolicy"], "IfNotPresent");
        let sc = &c["securityContext"];
        assert_eq!(sc["runAsUser"], 999);
        assert_eq!(sc["runAsGroup"], 999);
        assert_eq!(sc["runAsNonRoot"], true);
        assert_eq!(sc["allowPrivilegeEscalation"], false);
        assert_eq!(sc["capabilities"]["drop"], json!(["ALL"]));
        assert_eq!(sc["seccompProfile"]["type"], "RuntimeDefault");
        assert!(c.get("ports").is_none(), "nothing exposed outside the pod");
        assert!(c["startupProbe"]["exec"]["command"].is_array());
        assert!(c.get("envFrom").is_none(), "no secret reaches a sidecar");
    }
    let (pg, redis) = (&init[0], &init[1]);
    assert_eq!(pg["image"], "postgres:16");
    let pg_env = env_of(pg);
    assert_eq!(pg_env["POSTGRES_USER"], "trovato");
    assert_eq!(pg_env["POSTGRES_PASSWORD"], "trovato");
    assert_eq!(pg_env["POSTGRES_DB"], "trovato_test");
    assert_eq!(pg_env["PGDATA"], "/var/lib/postgresql/data/pgdata");
    assert_eq!(
        pg["volumeMounts"][0],
        json!({ "name": "pgdata", "mountPath": "/var/lib/postgresql/data" })
    );
    assert_eq!(
        pg["startupProbe"]["exec"]["command"],
        json!([
            "pg_isready",
            "-h",
            "127.0.0.1",
            "-U",
            "trovato",
            "-d",
            "trovato_test"
        ])
    );
    assert_eq!(
        pg["args"],
        json!(["postgres", "-c", "listen_addresses=localhost"])
    );
    assert_eq!(redis["image"], "redis:7");
    assert_eq!(
        redis["args"],
        json!([
            "--save",
            "",
            "--appendonly",
            "no",
            "--bind",
            "127.0.0.1",
            "-::1"
        ])
    );
    assert_eq!(
        redis["startupProbe"]["exec"]["command"],
        json!(["redis-cli", "-h", "127.0.0.1", "ping"])
    );
    let vols = pod["volumes"].as_array().unwrap();
    assert!(vols
        .iter()
        .any(|v| v["name"] == "pgdata" && v["emptyDir"].is_object()));

    // The agent's env: today's, plus trovato CI's two URLs.
    let agent = &pod["containers"][0];
    assert_eq!(pod["containers"].as_array().unwrap().len(), 1);
    let env = env_of(agent);
    assert_eq!(
        env["DATABASE_URL"],
        "postgres://trovato:trovato@localhost:5432/trovato_test"
    );
    assert_eq!(env["REDIS_URL"], "redis://127.0.0.1:6379");
    let today = env_of(&trovato_job(false)["spec"]["template"]["spec"]["containers"][0]);
    let mut rest = env.clone();
    rest.remove("DATABASE_URL");
    rest.remove("REDIS_URL");
    rest.remove("CARGO_BUILD_JOBS");
    let mut today_rest = today.clone();
    today_rest.remove("CARGO_BUILD_JOBS");
    assert_eq!(rest, today_rest, "nothing else in the env changes");
    assert_eq!(today["CARGO_BUILD_JOBS"], "8");
    assert_eq!(
        env["CARGO_BUILD_JOBS"], "7",
        "the agent's 7.25 CPU limit, rounded down"
    );

    // Agent and sidecars together are exactly the configured pod.
    let all: Vec<&Value> = std::iter::once(agent).chain(init.iter()).collect();
    let sum = |kind: &str, res: &str, f: fn(&Value) -> u32| -> u32 {
        all.iter().map(|c| f(&c["resources"][kind][res])).sum()
    };
    assert_eq!(sum("requests", "memory", mebibytes), 26 * 1024);
    assert_eq!(sum("limits", "memory", mebibytes), 28 * 1024);
    assert_eq!(sum("requests", "cpu", millicores), 7000);
    assert_eq!(sum("limits", "cpu", millicores), 8000);
    assert_eq!(agent["resources"]["limits"]["memory"], "27392Mi");
    assert_eq!(agent["resources"]["requests"]["memory"], "25984Mi");
    assert_eq!(agent["resources"]["limits"]["cpu"], "7250m");
    assert_eq!(agent["resources"]["requests"]["cpu"], "6250m");
    assert_eq!(agent["resources"]["requests"]["ephemeral-storage"], "20Gi");
    // The scheduling rules are today's.
    let off = trovato_job(false);
    let off_pod = &off["spec"]["template"]["spec"];
    for key in [
        "affinity",
        "nodeSelector",
        "tolerations",
        "securityContext",
        "restartPolicy",
    ] {
        assert_eq!(pod[key], off_pod[key], "{key}");
    }
}

#[test]
fn with_services_off_the_cloud_job_is_byte_for_byte_the_single_container_pod() {
    // Rendered by bridge 0.222.0's build_cloud_job for the same spec and extras, updated for
    // DJ29's JOB_DEADLINE_SECONDS, CONTINUATION and terminationGracePeriodSeconds.
    let today = include_str!("../../../golden/cloud-job-single-container.json");
    let now = serde_json::to_string_pretty(&trovato_job(false)).unwrap() + "\n";
    assert_eq!(now, today);
}

// ---- continuations: the Job JSON ----------------------------------------------------------

fn job_env_of(job: &Value) -> BTreeMap<String, String> {
    env_of(&job["spec"]["template"]["spec"]["containers"][0])
}

/// A spec and extras for the continuation JSON tests, a timeout named.
fn chain_spec(timeout_minutes: u64) -> (DevJobSpec, CloudJobExtras) {
    let mut spec = DevJobSpec::validate(
        args(json!({ "repo": "o/n", "prompt": "x", "branch_slug": "gd5-x" })),
        Agent::Claude,
    )
    .unwrap();
    spec.timeout_minutes = timeout_minutes;
    let extras = CloudJobExtras {
        run_id: "GD5".into(),
        merge: MergeDecision::No,
        merge_deny: String::new(),
        resources: CloudSettings::default().job_resources(),
        services: false,
        chain: None,
    };
    (spec, extras)
}

#[test]
fn job_deadline_seconds_always_equals_active_deadline_seconds() {
    for timeout_minutes in [120, 315] {
        let (spec, extras) = chain_spec(timeout_minutes);
        let job = build_cloud_job("jesse-dev", "a", "p", "img", &spec, &extras, "turn");
        let v = serde_json::to_value(&job).unwrap();
        let deadline = v["spec"]["activeDeadlineSeconds"].clone();
        assert_eq!(job_env_of(&v)["JOB_DEADLINE_SECONDS"], deadline.to_string());
        assert_eq!(cloud_deadline_secs(&spec).to_string(), deadline.to_string());
    }
}

#[test]
fn the_pods_termination_grace_period_is_the_checkpoint_grace() {
    let (spec, extras) = chain_spec(120);
    let job = build_cloud_job("jesse-dev", "a", "p", "img", &spec, &extras, "turn");
    let v = serde_json::to_value(&job).unwrap();
    assert_eq!(
        v["spec"]["template"]["spec"]["terminationGracePeriodSeconds"],
        CHECKPOINT_GRACE_SECS
    );
}

#[test]
fn a_continuations_run_id_env_is_the_chain_root_while_its_label_is_the_new_link() {
    let (spec, mut extras) = chain_spec(120);
    extras.run_id = "B248-c2".into();
    extras.chain = Some(ChainExtras {
        root: "B248".into(),
        link: 1,
        continuation_of: "B248".into(),
        chain_json: "[]".into(),
    });
    let job = build_cloud_job("jesse-dev", "a", "p", "img", &spec, &extras, "turn");
    let v = serde_json::to_value(&job).unwrap();
    assert_eq!(v["metadata"]["labels"][LABEL_RUN_ID], "B248-c2");
    let env = job_env_of(&v);
    assert_eq!(env["RUN_ID"], "B248", "the pull request stays [B248]");
    assert_eq!(env["CONTINUATION"], "1");
    let ann = &v["metadata"]["annotations"];
    assert_eq!(ann[ANN_CHAIN_ROOT], "B248");
    assert_eq!(ann[ANN_CONTINUATION], "1");
    assert_eq!(ann[ANN_CONTINUATION_OF], "B248");
    assert_eq!(ann[ANN_CHAIN], "[]");
}

#[test]
fn link_0_carries_no_chain_annotations_and_its_continuation_env_is_0() {
    let (spec, extras) = chain_spec(120);
    let job = build_cloud_job("jesse-dev", "a", "p", "img", &spec, &extras, "turn");
    let v = serde_json::to_value(&job).unwrap();
    assert_eq!(job_env_of(&v)["RUN_ID"], "GD5");
    assert_eq!(job_env_of(&v)["CONTINUATION"], "0");
    let ann = v["metadata"]["annotations"].as_object().unwrap();
    for key in [
        ANN_CHAIN_ROOT,
        ANN_CONTINUATION,
        ANN_CONTINUATION_OF,
        ANN_CHAIN,
    ] {
        assert!(!ann.contains_key(key), "{key}");
    }
}

// ---- continuation create-or-reuse, and stopped_at_time_limit ------------------------------

#[test]
fn a_continuation_is_created_once_and_reused_on_a_retried_pass() {
    let existing = ["GD5".to_string(), "GD5-c2".to_string()];
    assert_eq!(
        continuation_create_or_reuse(&existing, "GD5-c3"),
        ContinuationCreate::Create,
        "no Job named GD5-c3 yet"
    );
    assert_eq!(
        continuation_create_or_reuse(&existing, "GD5-c2"),
        ContinuationCreate::Reuse,
        "a retried finalize pass must not create a second Job for the same link"
    );
}

#[test]
fn stopped_at_time_limit_reads_the_companion_keys_or_the_agents_own_wording() {
    let mut report = DevJobReport::default();
    assert!(!stopped_at_time_limit(&report, None), "nothing said so");

    report
        .extra
        .insert("stopped_at_limit".into(), "agent".into());
    assert!(stopped_at_time_limit(&report, None));

    let report = DevJobReport {
        failure: Some("agent hit the 18900s wall clock limit".into()),
        ..DevJobReport::default()
    };
    assert!(stopped_at_time_limit(&report, None));

    let report = DevJobReport {
        failure: Some(
            "the pod was stopped (SIGTERM) before the run finished; its work as of the SIGTERM \
             is checkpointed to jesse/gd5-x-wip on origin"
                .into(),
        ),
        ..DevJobReport::default()
    };
    assert!(
        !stopped_at_time_limit(&report, Some("OOMKilled")),
        "a SIGTERM the Job's own deadline does not explain is not the time limit"
    );
    assert!(stopped_at_time_limit(
        &report,
        Some("DeadlineExceeded: Job was active longer than specified deadline")
    ));

    let report = DevJobReport {
        failure: Some("cargo test exited 101".into()),
        ..DevJobReport::default()
    };
    assert!(!stopped_at_time_limit(&report, None));
}

// ---- continuation_decision --------------------------------------------------------------

fn checkpoint(tree: &str) -> crate::hostrun::sys::Checkpoint {
    crate::hostrun::sys::Checkpoint {
        sha: "deadbeef".into(),
        tree: tree.into(),
        subject: "checkpoint of jesse/gd5-x at 2026-11-26T10:00:00Z [skip ci]".into(),
        parent_count: 1,
    }
}

/// [`continuation_decision`]'s defaults: nothing cancelled or killed, no pull request, stopped
/// at the limit, no question, a usable checkpoint, link 0 of at most 3, no previous to compare.
struct Case<'a> {
    cancelled: bool,
    oom_killed: bool,
    has_pull_request: bool,
    stopped_at_limit: bool,
    needs_you: bool,
    checkpoint: Result<crate::hostrun::sys::Checkpoint, &'a str>,
    current_link: u32,
    max_continuations: u32,
    previous: Option<(&'a str, &'a str)>,
}

impl Default for Case<'_> {
    fn default() -> Self {
        Case {
            cancelled: false,
            oom_killed: false,
            has_pull_request: false,
            stopped_at_limit: true,
            needs_you: false,
            checkpoint: Ok(checkpoint("tree1")),
            current_link: 0,
            max_continuations: 3,
            previous: None,
        }
    }
}

fn decide(c: Case) -> ContinuationDecision {
    continuation_decision(
        c.cancelled,
        c.oom_killed,
        c.has_pull_request,
        c.stopped_at_limit,
        c.needs_you,
        c.checkpoint.as_ref().map_err(|e| *e),
        c.current_link,
        c.max_continuations,
        c.previous,
    )
}

fn stops(d: &ContinuationDecision, needle: &str) -> bool {
    matches!(d, ContinuationDecision::Stop(why) if why.contains(needle))
}

#[test]
fn continuation_decision_follows_its_rules_in_order() {
    // The B248 shape: capped, checkpoint present, link 0, max 3: continue as link 1.
    assert_eq!(
        decide(Case::default()),
        ContinuationDecision::Continue { link: 1 }
    );
    // The B247 shape: capped, no checkpoint: stop, naming why.
    assert!(stops(
        &decide(Case {
            checkpoint: Err("jesse/gd5-x-wip does not exist on origin"),
            ..Case::default()
        }),
        "does not exist"
    ));
    // Rule 1: cancelled, first match, whatever else is true.
    assert!(stops(
        &decide(Case {
            cancelled: true,
            ..Case::default()
        }),
        "cancelled"
    ));
    // Rule 2: killed for memory.
    assert!(stops(
        &decide(Case {
            oom_killed: true,
            ..Case::default()
        }),
        "memory"
    ));
    // Rule 3: a pull request already.
    assert!(stops(
        &decide(Case {
            has_pull_request: true,
            ..Case::default()
        }),
        "already on GitHub"
    ));
    // Rule 4: did not stop at its time limit.
    assert!(stops(
        &decide(Case {
            stopped_at_limit: false,
            ..Case::default()
        }),
        "did not stop"
    ));
    // Rule 5: the agent asked a question.
    assert!(stops(
        &decide(Case {
            needs_you: true,
            ..Case::default()
        }),
        "NEEDS YOU"
    ));
    // Rule 6: the checkpoint could not be read (collapsed with "no checkpoint" by the caller).
    assert!(stops(
        &decide(Case {
            checkpoint: Err("the checkpoint could not be read: rate limited"),
            ..Case::default()
        }),
        "rate limited"
    ));
    // Rule 7: already at the cap.
    assert!(stops(
        &decide(Case {
            current_link: 3,
            max_continuations: 3,
            ..Case::default()
        }),
        "max_continuations"
    ));
    assert_eq!(
        decide(Case {
            current_link: 2,
            max_continuations: 3,
            ..Case::default()
        }),
        ContinuationDecision::Continue { link: 3 }
    );
    // 0 turns continuations off outright, even at link 0.
    assert!(stops(
        &decide(Case {
            max_continuations: 0,
            ..Case::default()
        }),
        "max_continuations"
    ));
    // Rule 8: no progress since the checkpoint this run itself resumed from.
    assert!(stops(
        &decide(Case {
            previous: Some(("GD5", "tree1")),
            ..Case::default()
        }),
        "no progress since GD5"
    ));
    // Different tree: real progress, continues.
    assert_eq!(
        decide(Case {
            previous: Some(("GD5", "tree0")),
            ..Case::default()
        }),
        ContinuationDecision::Continue { link: 1 }
    );
}

#[test]
fn max_continuations_defaults_to_3_0_parses_and_11_is_refused() {
    assert_eq!(CloudSettings::default().max_continuations, 3);
    let s = |max: u32| {
        CloudSettings::from_toml(
            Some(CloudToml {
                enabled: true,
                max_continuations: Some(max),
                ..CloudToml::default()
            }),
            "",
        )
    };
    assert_eq!(s(0).max_continuations, 0);
    assert!(s(0)
        .problems(true)
        .iter()
        .all(|p| !p.contains("max_continuations")));
    assert!(s(11)
        .problems(true)
        .iter()
        .any(|p| p.contains("max_continuations")));
}

// ---- the continuation prompt --------------------------------------------------------------

#[test]
fn the_continuation_prompt_is_pinned_and_strips_any_earlier_section() {
    let base = "Do the thing.\n";
    let input = ContinuationPromptInput {
        link: 1,
        max: 3,
        root: "B248",
        previous: "B248",
        minutes: 315,
        branch: "jesse/gd5-x",
        sha: "deadbeef",
    };
    let want = "Do the thing.\n\n### Continuation\n\n\
        This is continuation 1 of at most 3 of cloud run B248. The attempt before this one \
        (B248) reached its 315 minute time limit before it finished, and its work was saved: \
        your branch jesse/gd5-x starts from its checkpoint deadbeef, with its commits on the \
        branch and its uncommitted changes in the work tree. If the environment variable \
        JESSE_PROGRESS_FILE names a file that exists, read it first: it says what the earlier \
        attempts finished and what they meant to do next. Continue the task above from there. \
        Do not start over, and do not redo work that is already committed and passing. Keep \
        that file up to date as you go (what is done, what is next), never put this prompt's \
        text or any secret in it, because it is pushed to the repository with your checkpoints, \
        and commit after each step you finish: you have the same time limit.\n";
    assert_eq!(continuation_prompt(base, &input), want);

    // A prompt that already has one section keeps exactly one, with the pinned text.
    let already = "Do the thing.\n\n### Continuation\n\nstale text here.\n";
    let again = continuation_prompt(already, &input);
    assert_eq!(again, want);
    assert_eq!(again.matches("### Continuation").count(), 1);
}

#[test]
fn services_are_on_by_default_and_one_switch_turns_them_off() {
    assert!(CloudSettings::default().services);
    let t: DevJobToml = toml::from_str("[cloud]\nservices = false\n").unwrap();
    assert!(!CloudSettings::from_toml(t.cloud, "/home/u").services);
    let t: DevJobToml = toml::from_str("[cloud]\nservices = true\n").unwrap();
    assert!(CloudSettings::from_toml(t.cloud, "/home/u").services);
    // The live size leaves the agent plenty.
    let live = CloudSettings {
        burst_size: "s-8vcpu-32gb-amd".into(),
        job_memory_gi: 28,
        job_memory_request_gi: 26,
        job_cpu: 8,
        job_cpu_request: 7,
        ..CloudSettings::default()
    };
    assert!(
        live.resource_problems().is_empty(),
        "{:?}",
        live.resource_problems()
    );
    assert_eq!(
        live.job_resources().agent(true),
        AgentResources {
            memory_request_mi: 25984,
            memory_limit_mi: 27392,
            cpu_request_milli: 6250,
            cpu_limit_milli: 7250,
        }
    );
    // Off, the agent has the whole pod, as before.
    assert_eq!(
        live.job_resources().agent(false),
        AgentResources {
            memory_request_mi: 26 * 1024,
            memory_limit_mi: 28 * 1024,
            cpu_request_milli: 7000,
            cpu_limit_milli: 8000,
        }
    );
    // The 8 GB worker's defaults still fit beside the sidecars.
    let small = CloudSettings {
        burst_size: "s-4vcpu-8gb-intel".into(),
        ..CloudSettings::from_toml(
            toml::from_str::<DevJobToml>("[cloud]\nburst_size = \"s-4vcpu-8gb-intel\"\n")
                .unwrap()
                .cloud,
            "/home/u",
        )
    };
    assert!(
        small.resource_problems().is_empty(),
        "{:?}",
        small.resource_problems()
    );
}

#[test]
fn the_bridge_refuses_a_size_that_leaves_the_agent_under_2gi_or_1_cpu() {
    // A 3 Gi limit leaves the agent 1.75 Gi once the sidecars have their 1.25 Gi.
    let t: DevJobToml = toml::from_str(
        "[cloud]\nenabled = true\njob_memory_gi = 3\njob_memory_request_gi = 2\n\
         job_cpu = 1\njob_cpu_request = 1\n",
    )
    .unwrap();
    let s = CloudSettings::from_toml(t.cloud, "/home/u");
    let p = s.problems(true).join("\n");
    for needle in [
        "job_memory_request_gi 2 / job_memory_gi 3 leave the agent 1408Mi / 1792Mi once \
         Postgres and Redis have theirs, less than 2Gi",
        "job_cpu_request 1 / job_cpu 1 leave the agent 250m / 250m CPU once Postgres and Redis \
         have theirs, less than 1",
    ] {
        assert!(p.contains(needle), "{needle} missing from:\n{p}");
    }
    // At the floor exactly: 2 Gi and 1 CPU left to the agent's limits is allowed, a request
    // under it is not.
    let floor = CloudSettings {
        job_memory_gi: 4,
        job_memory_request_gi: 3,
        job_cpu: 2,
        job_cpu_request: 2,
        ..CloudSettings::default()
    };
    assert_eq!(floor.job_resources().agent(true).memory_limit_mi, 2816);
    assert!(
        floor.resource_problems().is_empty(),
        "{:?}",
        floor.resource_problems()
    );
    let cpu_short = CloudSettings {
        job_cpu: 2,
        job_cpu_request: 1,
        ..CloudSettings::default()
    };
    assert!(cpu_short
        .resource_problems()
        .join("\n")
        .contains("job_cpu_request 1 / job_cpu 2 leave the agent 250m / 1250m CPU"));
    // A request equal to its limit would give the agent a request above its own limit.
    let equal = CloudSettings {
        job_memory_gi: 14,
        job_memory_request_gi: 14,
        ..CloudSettings::default()
    };
    assert!(equal.resource_problems().join("\n").contains(
        "job_memory_request_gi 14 must be below job_memory_gi 14 with services on: the agent \
         would request 13696Mi under a 13056Mi limit"
    ));
    // Services off: the same sizes are today's, and boot.
    for c in [&s, &equal] {
        let off = CloudSettings {
            services: false,
            ..(*c).clone()
        };
        assert!(
            off.resource_problems().is_empty(),
            "{:?}",
            off.resource_problems()
        );
    }
}

#[test]
fn a_memory_kill_at_the_agents_share_reads_in_gi() {
    let mut pod = oom_pod("OOMKilled", false);
    pod.spec.as_mut().unwrap().containers[0]
        .resources
        .as_mut()
        .unwrap()
        .limits
        .as_mut()
        .unwrap()
        .insert(
            "memory".into(),
            k8s_openapi::apimachinery::pkg::api::resource::Quantity("27392Mi".into()),
        );
    assert_eq!(
        crate::devjob::oom_killed_line(&pod).as_deref(),
        Some("killed at the 26.75 Gi memory limit (OOMKilled)")
    );
}

#[test]
fn the_largest_configured_sizes_convert_exactly_with_services_on_or_off() {
    let huge = JobResources {
        memory_gi: u32::MAX,
        memory_request_gi: 12,
        cpu: u32::MAX,
        cpu_request: 3,
    };
    let on = huge.agent(true);
    assert_eq!(on.memory_limit_mi, u64::from(u32::MAX) * 1024 - 1280);
    assert_eq!(on.cpu_limit_milli, u64::from(u32::MAX) * 1000 - 750);
    assert_eq!(on.build_jobs(), u64::from(u32::MAX) - 1);
    let off = huge.agent(false);
    assert_eq!(off.memory_limit_mi, u64::from(u32::MAX) * 1024);
    assert_eq!(memory_quantity(off.memory_limit_mi), "4294967295Gi");
    assert_eq!(cpu_quantity(off.cpu_limit_milli), "4294967295");
    // The size that wrapped to 16 Gi in u32 keeps its full limit.
    let wrapping = JobResources {
        memory_gi: 4_194_320,
        ..huge
    };
    assert_eq!(
        memory_quantity(wrapping.agent(false).memory_limit_mi),
        "4194320Gi"
    );
    assert_eq!(
        wrapping.agent(true).memory_limit_mi,
        4_194_320u64 * 1024 - 1280
    );
}
