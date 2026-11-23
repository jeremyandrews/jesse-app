//! Queued runs carry a priority (0.216.0): `low`, `normal`, `high`, `urgent`. The queue pass
//! starts resumed rounds first, then the highest priority, then the oldest; it never stops a
//! running run and never bypasses a cap.

use super::*;

/// A free prompt on `repo` with `priority`.
fn free_at(repo: &str, slug: &str, priority: Option<Priority>) -> StartRequest {
    StartRequest {
        priority,
        ..free_in(repo, slug)
    }
}

/// Create a queued run (no queue pass) and pin its `queued_at` to `at` seconds past a fixed
/// instant, so the order of the wait is the test's, not the clock's.
fn queue(r: &Rig, req: StartRequest, at: u64) -> String {
    let id = r.svc.create(&req).unwrap().run_id;
    update_run(&r.svc.settings.run_dir(&id), |x| {
        x.queued_at = Some(rfc(1_794_000_000_000 + at * 1000));
    })
    .unwrap();
    id
}

/// The run ids the fake launcher started, in order.
fn started(r: &Rig) -> Vec<String> {
    lock(&r.launcher.bootstrapped)
        .iter()
        .map(|l| l.trim_start_matches(sys::DEFAULT_LABEL_PREFIX).to_string())
        .collect()
}

/// A second bare repository, `o/m`, beside the fixture's `o/n`.
fn second_remote(s: &Scratch) {
    let remote = s.base.join("remote/o");
    let out = std::process::Command::new("git")
        .args(["clone", "-q", "--bare"])
        .arg(remote.join("n.git"))
        .arg(remote.join("m.git"))
        .output()
        .unwrap();
    assert!(out.status.success());
}

/// A queued record in memory: enough for [`queue_order`].
fn queued(id: &str, repo: &str, priority: Priority, at: u64, auto: Option<&str>) -> RunRecord {
    RunRecord {
        run_id: id.into(),
        repo: repo.into(),
        status: RunStatus::Queued,
        created_at: rfc(1),
        queued_at: Some(rfc(1_794_000_000_000 + at * 1000)),
        priority,
        rounds: vec![Round {
            n: 1,
            auto: auto.map(str::to_string),
            ..Round::default()
        }],
        ..RunRecord::default()
    }
}

#[tokio::test]
async fn a_mixed_queue_in_one_repository_starts_highest_priority_first_then_oldest() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let mut st = settings(&s);
    st.max_concurrent = 10;
    st.max_per_repo = 10;
    let r = rig_with(&s, st);
    // Queued oldest first: normal, low, high, normal, urgent, high.
    let n1 = queue(&r, free_at("o/n", "n1", None), 1);
    let l1 = queue(&r, free_at("o/n", "l1", Some(Priority::Low)), 2);
    let h1 = queue(&r, free_at("o/n", "h1", Some(Priority::High)), 3);
    let n2 = queue(&r, free_at("o/n", "n2", Some(Priority::Normal)), 4);
    let u1 = queue(&r, free_at("o/n", "u1", Some(Priority::Urgent)), 5);
    let h2 = queue(&r, free_at("o/n", "h2", Some(Priority::High)), 6);
    let want = vec![u1, h1, h2, n1, n2, l1];
    // What devjob_list shows: start order, each with its place in its repository.
    let runs: Vec<RunRecord> = r.svc.runs().into_iter().map(|(_, x)| x).collect();
    let listed: Vec<(String, Priority, usize)> = start_order(&runs)
        .into_iter()
        .map(|(x, n)| (x.run_id.clone(), x.priority, n))
        .collect();
    println!("start order (run, priority, place in o/n):");
    for (id, p, n) in &listed {
        println!("  {n}. {id} {}", p.label());
    }
    assert_eq!(listed.iter().map(|x| x.0.clone()).collect::<Vec<_>>(), want);
    assert_eq!(
        listed.iter().map(|x| x.2).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6]
    );
    // And what the queue pass does: exactly that order.
    r.svc.pump(now_ms());
    println!("queue pass started: {:?}", started(&r));
    assert_eq!(started(&r), want);
}

#[tokio::test]
async fn with_one_slot_the_urgent_run_takes_it_and_the_rest_wait_in_order() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let mut st = settings(&s);
    st.max_concurrent = 1;
    let r = rig_with(&s, st);
    let old = queue(&r, free_at("o/n", "old", None), 1);
    let urgent = queue(&r, free_at("o/n", "late", Some(Priority::Urgent)), 2);
    r.svc.pump(now_ms());
    assert_eq!(started(&r), vec![urgent.clone()]);
    assert_eq!(r.svc.get(&urgent).unwrap().status, RunStatus::Running);
    let waiting = r.svc.get(&old).unwrap();
    assert_eq!(waiting.status, RunStatus::Queued);
    assert_eq!(waiting.waiting_for.as_deref(), Some("slot"));
}

#[tokio::test]
async fn a_high_run_in_a_full_repository_leaves_the_free_slot_to_a_normal_run_elsewhere() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    second_remote(&s);
    let mut st = settings(&s);
    st.max_concurrent = 2;
    st.max_per_repo = 1;
    let r = rig_with(&s, st);
    // o/n is busy: one running run, at its cap of one.
    let busy = r.svc.start(free_at("o/n", "busy", None)).await.unwrap();
    assert_eq!(busy.status, RunStatus::Running);
    let high = queue(&r, free_at("o/n", "high", Some(Priority::Urgent)), 1);
    let normal = queue(&r, free_at("o/m", "normal", None), 2);
    r.svc.pump(now_ms());
    assert_eq!(r.svc.get(&normal).unwrap().status, RunStatus::Running);
    let h = r.svc.get(&high).unwrap();
    assert_eq!(h.status, RunStatus::Queued);
    assert_eq!(h.waiting_for.as_deref(), Some("repo o/n"));
    // And the running run was never touched.
    assert_eq!(r.svc.get(&busy.run_id).unwrap().status, RunStatus::Running);
    assert!(lock(&r.launcher.booted_out).is_empty());
}

#[test]
fn resumed_rounds_go_before_every_priority() {
    let runs = vec![
        queued("URG", "o/n", Priority::Urgent, 1, None),
        queued("LIM", "o/n", Priority::Low, 5, Some(AUTO_LIMIT)),
        queued("RST", "o/m", Priority::Normal, 3, Some(AUTO_RESTART)),
        queued("NRM", "o/n", Priority::Normal, 0, None),
    ];
    let order: Vec<(&str, usize)> = start_order(&runs)
        .into_iter()
        .map(|(x, n)| (x.run_id.as_str(), n))
        .collect();
    // Both resumed rounds first (oldest first between them), then urgent, then normal; the
    // place counts within each repository.
    assert_eq!(order, vec![("RST", 1), ("LIM", 1), ("URG", 2), ("NRM", 3)]);
}

#[test]
fn with_every_run_normal_the_order_is_the_old_one() {
    let mut runs = vec![
        queued("C", "o/n", Priority::Normal, 2, None),
        queued("A", "o/n", Priority::Normal, 2, None),
        queued("B", "o/n", Priority::Normal, 1, None),
    ];
    // No `queued_at`: `created_at` stands in, as before.
    runs.push(RunRecord {
        queued_at: None,
        created_at: rfc(1_794_000_000_000),
        ..queued("D", "o/n", Priority::Normal, 0, None)
    });
    let order: Vec<&str> = start_order(&runs)
        .into_iter()
        .map(|(x, _)| x.run_id.as_str())
        .collect();
    assert_eq!(order, vec!["D", "B", "A", "C"]);
}

#[test]
fn a_record_without_a_priority_reads_as_normal_and_normal_is_not_written() {
    let old = r#"{"run_id":"GD5","repo":"o/n","branch":"gd5","origin":"app","status":"queued",
        "created_at":"2026-11-20T10:00:00Z","claude":"claude","permission_mode":"auto"}"#;
    let rec: RunRecord = serde_json::from_str(old).unwrap();
    assert_eq!(rec.priority, Priority::Normal);
    assert!(rec.priority_changes.is_empty());
    let v = serde_json::to_value(&rec).unwrap();
    assert!(v.get("priority").is_none(), "{v}");
    assert!(v.get("priority_changes").is_none(), "{v}");
    // A raised one is stored by name and reads back.
    let high = RunRecord {
        priority: Priority::High,
        ..rec
    };
    let v = serde_json::to_value(&high).unwrap();
    assert_eq!(v["priority"], "high");
    let back: RunRecord = serde_json::from_value(v).unwrap();
    assert_eq!(back.priority, Priority::High);
}

#[test]
fn only_the_four_names_are_priorities() {
    assert_eq!(Priority::parse("urgent"), Ok(Priority::Urgent));
    assert_eq!(Priority::parse(" High "), Ok(Priority::High));
    assert_eq!(Priority::parse("low"), Ok(Priority::Low));
    assert_eq!(Priority::parse("normal"), Ok(Priority::Normal));
    for bad in ["asap", "", "1", "highest"] {
        let e = Priority::parse(bad).unwrap_err();
        assert!(e.contains("low, normal, high, urgent"), "{e}");
    }
    assert!(Priority::Urgent > Priority::High && Priority::High > Priority::Normal);
    assert!(Priority::Normal > Priority::Low);
}

#[test]
fn devjob_start_takes_a_priority_on_the_studio_and_refuses_one_elsewhere() {
    let plan =
        |v: Value| devjob::plan_start(serde_json::from_value(v).unwrap(), devjob::Agent::Claude);
    let studio = plan(json!({ "host": "studio", "prompt": "p", "branch_slug": "x",
                              "repo": "o/n", "priority": "urgent" }))
    .unwrap();
    assert!(
        matches!(
            studio,
            devjob::StartPlan::Studio {
                priority: Some(Priority::Urgent),
                ..
            }
        ),
        "{studio:?}"
    );
    let none = plan(json!({ "host": "studio", "draft": "Projects/drafts/x.md" })).unwrap();
    assert!(matches!(
        none,
        devjob::StartPlan::Studio { priority: None, .. }
    ));
    let e = plan(json!({ "host": "studio", "draft": "Projects/drafts/x.md", "priority": "asap" }))
        .unwrap_err();
    assert!(e.contains("\"asap\"") && e.contains("urgent"), "{e}");
    for host in [json!(null), json!("cluster"), json!("cloud")] {
        let mut args = json!({ "repo": "o/n", "prompt": "p", "branch_slug": "x",
                               "priority": "high" });
        if !host.is_null() {
            args["host"] = host;
        }
        let e = plan(args).unwrap_err();
        assert!(
            e.contains("priority applies only with host \"studio\""),
            "{e}"
        );
    }
}

/// `Projects/drafts/<name>.md`, a loose draft on `o/n` whose front matter adds `extra`.
fn priority_draft(s: &Scratch, name: &str, extra: &str) -> String {
    let rel = format!("Projects/drafts/{name}.md");
    std::fs::write(
        s.notes().join(&rel),
        format!("---\ndevjob-repo: o/n\n{extra}---\n# {name}\n\n## PROMPT\n{SECTION}"),
    )
    .unwrap();
    rel
}

#[tokio::test]
async fn the_draft_key_sets_the_priority_and_the_start_argument_wins() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let low = priority_draft(&s, "low-one", "devjob-priority: low # not today\n");
    let id = r.svc.create(&by_draft(&low)).unwrap().run_id;
    assert_eq!(r.svc.get(&id).unwrap().priority, Priority::Low);
    let id = r
        .svc
        .create(&StartRequest {
            priority: Some(Priority::Urgent),
            again: true,
            ..by_draft(&low)
        })
        .unwrap()
        .run_id;
    assert_eq!(r.svc.get(&id).unwrap().priority, Priority::Urgent);
    // No key and no argument: normal, and nothing is written.
    let plain = priority_draft(&s, "plain-one", "");
    let id = r.svc.create(&by_draft(&plain)).unwrap().run_id;
    assert_eq!(r.svc.get(&id).unwrap().priority, Priority::Normal);
    let raw = std::fs::read_to_string(r.svc.settings.run_dir(&id).join("run.json")).unwrap();
    assert!(!raw.contains("priority"), "{raw}");
    // An unknown value is refused, not read as normal.
    let bad = priority_draft(&s, "bad-one", "devjob-priority: asap\n");
    let e = r.svc.create(&by_draft(&bad)).unwrap_err();
    assert!(
        matches!(&e, Refusal::Invalid(m) if m.contains("devjob-priority") && m.contains("\"asap\"")),
        "{e:?}"
    );
}

#[tokio::test]
async fn devjob_priority_changes_a_queued_run_and_refuses_a_running_one() {
    use crate::gateway::InProcessServer;
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let mut st = settings(&s);
    st.max_concurrent = 1;
    let r = rig_with(&s, st);
    let kc = s.base.join("kc.yaml");
    std::fs::write(
        &kc,
        "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster:\n    server: https://127.0.0.1:1\n\
         contexts:\n- name: x\n  context:\n    cluster: c\n    user: u\ncurrent-context: x\n\
         users:\n- name: u\n  user:\n    token: not-a-real-token\n",
    )
    .unwrap();
    let dj = devjob::DevJobs::connect(devjob::DevJobSettings::from_toml(
        Some(devjob::DevJobToml {
            enabled: true,
            kubeconfig: Some(kc),
            ..devjob::DevJobToml::default()
        }),
        "/home/u",
    ))
    .await
    .unwrap();
    dj.attach_studio(r.svc.clone());
    let call = |tool: &'static str, args: Value| {
        let dj = &dj;
        async move { dj.call(tool, args).await.unwrap() }
    };
    // One running, two queued behind it; the start answers the priority it was given.
    let run = call(
        devjob::TOOL_START,
        json!({ "host": "studio", "prompt": "p", "branch_slug": "first", "repo": "o/n" }),
    )
    .await;
    assert_eq!(tool_text(&run)["status"], "running", "{run}");
    assert_eq!(tool_text(&run)["priority"], "normal");
    let a = call(
        devjob::TOOL_START,
        json!({ "host": "studio", "prompt": "p", "branch_slug": "second", "repo": "o/n" }),
    )
    .await;
    assert_eq!(tool_text(&a)["status"], "queued", "{a}");
    let b = call(
        devjob::TOOL_START,
        json!({ "host": "studio", "prompt": "p", "branch_slug": "third", "repo": "o/n",
                "priority": "low" }),
    )
    .await;
    assert_eq!(tool_text(&b)["priority"], "low", "{b}");
    // The list: priority on every run, the queued ones in start order with their places.
    let list = tool_text(&call(devjob::TOOL_LIST, json!({})).await);
    let active = list["studio"]["active"].as_array().unwrap().clone();
    let brief: Vec<(String, String, Value)> = active
        .iter()
        .map(|x| {
            (
                x["run_id"].as_str().unwrap().to_string(),
                x["priority"].as_str().unwrap().to_string(),
                x["queue_position"].clone(),
            )
        })
        .collect();
    assert_eq!(
        brief,
        vec![
            ("s-first".into(), "normal".into(), Value::Null),
            ("s-second".into(), "normal".into(), json!(1)),
            ("s-third".into(), "low".into(), json!(2)),
        ]
    );
    // Raise the last one: it is now first in line, and its history says so.
    let raised = call(
        devjob::TOOL_PRIORITY,
        json!({ "run_id": "s-third", "priority": "urgent" }),
    )
    .await;
    assert_eq!(raised["isError"], false, "{raised}");
    assert_eq!(tool_text(&raised)["priority"], "urgent");
    assert_eq!(tool_text(&raised)["status"], "queued");
    let rec = r.svc.get("s-third").unwrap();
    assert_eq!(rec.priority, Priority::Urgent);
    assert_eq!(rec.priority_changes.len(), 1);
    assert_eq!(rec.priority_changes[0].from, Priority::Low);
    assert_eq!(rec.priority_changes[0].to, Priority::Urgent);
    assert!(rec.priority_changes[0].at.ends_with('Z') || rec.priority_changes[0].at.contains('+'));
    let list = tool_text(&call(devjob::TOOL_LIST, json!({})).await);
    let ids: Vec<&str> = list["studio"]["active"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["run_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["s-first", "s-third", "s-second"]);
    let status = tool_text(&call(devjob::TOOL_STATUS, json!({ "job_name": "s-second" })).await);
    assert_eq!(status["priority"], "normal", "{status}");
    // When the slot frees, the raised run takes it.
    finish_round(&r, "s-first", 0, Some("Done.\n"), "");
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("s-third").unwrap().status, RunStatus::Running);
    assert_eq!(r.svc.get("s-second").unwrap().status, RunStatus::Queued);
    // A running run is refused, with its status; so is an unknown run or level.
    let refused = call(
        devjob::TOOL_PRIORITY,
        json!({ "run_id": "s-third", "priority": "low" }),
    )
    .await;
    assert_eq!(refused["isError"], true);
    let text = refused["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("s-third is running") && text.contains("only a queued run"),
        "{text}"
    );
    assert_eq!(r.svc.get("s-third").unwrap().priority, Priority::Urgent);
    let unknown = call(
        devjob::TOOL_PRIORITY,
        json!({ "run_id": "s-second", "priority": "asap" }),
    )
    .await;
    assert_eq!(unknown["isError"], true);
    assert!(unknown["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("\"asap\""));
    assert_eq!(r.svc.get("s-second").unwrap().priority, Priority::Normal);
    let cancelled = r.svc.cancel("s-second").unwrap();
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    let e = r.svc.set_priority("s-second", Priority::High).unwrap_err();
    assert!(
        matches!(&e, Refusal::State(m) if m.contains("s-second is cancelled")),
        "{e:?}"
    );
    assert!(matches!(
        r.svc.set_priority("nope", Priority::High),
        Err(Refusal::NotFound(_))
    ));
}
