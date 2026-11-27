//! One run pipeline (0.226.0): a cloud run is a run record in the one queue, taken by a cloud
//! slot in the same order a Studio slot takes a Studio run, and it keeps its id, its record and
//! its priority on the cloud. The Job side is faked ([`FakeCloud`]); the record side is real.

use super::*;

/// A free prompt on `repo` with `priority`.
fn free_at(repo: &str, slug: &str, priority: Option<Priority>) -> StartRequest {
    StartRequest {
        priority,
        ..free_in(repo, slug)
    }
}

/// What a `devjob_start` with `host: "cloud"` asks of the cloud, here nothing beyond the start.
fn on_cloud() -> Option<CloudPart> {
    Some(CloudPart::default())
}

/// Create a queued run (no queue pass) on `cloud` and pin its `queued_at` to `at` seconds past a
/// fixed instant, so the order of the wait is the test's, not the clock's.
fn queue_on(r: &Rig, req: StartRequest, cloud: Option<CloudPart>, at: u64) -> String {
    let id = r.svc.create_on(&req, cloud).unwrap().run_id;
    update_run(&r.svc.settings.run_dir(&id), |x| {
        x.queued_at = Some(rfc(1_794_000_000_000 + at * 1000));
    })
    .unwrap();
    id
}

/// Every run a free cloud slot takes, one after another, until none is left.
fn take_all(r: &Rig, notes: &Path) -> Vec<String> {
    std::iter::from_fn(|| r.svc.take_for_cloud(notes).map(|x| x.run_id)).collect()
}

/// The run ids the fake launcher started, in order.
fn started(r: &Rig) -> Vec<String> {
    lock(&r.launcher.bootstrapped)
        .iter()
        .map(|l| l.trim_start_matches(sys::DEFAULT_LABEL_PREFIX).to_string())
        .collect()
}

/// The cloud's Jobs, faked: how a cancel answers, and every Job asked to stop.
#[derive(Default)]
struct FakeCloud {
    running: Mutex<bool>,
    stopped: Mutex<Vec<String>>,
}

impl CloudJobs for FakeCloud {
    fn cancel<'a>(
        &'a self,
        job: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<CloudCancel, String>> + Send + 'a>> {
        lock(&self.stopped).push(job.to_string());
        let how = if *lock(&self.running) {
            CloudCancel::Stopping
        } else {
            CloudCancel::Deleted
        };
        Box::pin(async move { Ok(how) })
    }
}

/// A Job's report, as the cloud's watcher hands it to the record.
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

fn rig_any(s: &Scratch) -> Rig {
    let mut st = settings(s);
    st.max_concurrent = 10;
    st.max_per_repo = 10;
    rig_with(s, st)
}

// ---- one queue, one priority rule -----------------------------------------------------------

#[tokio::test]
async fn a_cloud_run_queued_at_high_starts_before_an_older_normal_one() {
    let s = Scratch::new();
    vault_fixture(&s);
    let r = rig_any(&s);
    let older = queue_on(&r, free_at("o/n", "older", None), on_cloud(), 1);
    let high = queue_on(
        &r,
        free_at("o/n", "newer", Some(Priority::High)),
        on_cloud(),
        2,
    );
    let first = r.svc.take_for_cloud(&s.notes()).unwrap();
    assert_eq!(first.run_id, high);
    assert_eq!(first.status, RunStatus::Running);
    assert!(first.on_cloud());
    let still = r.svc.get(&older).unwrap();
    assert_eq!(still.status, RunStatus::Queued);
    assert_eq!(still.waiting_for.as_deref(), Some(WAITING_CLOUD));
    assert_eq!(take_all(&r, &s.notes()), vec![older]);
}

#[tokio::test]
async fn devjob_priority_on_a_queued_cloud_run_reorders_it() {
    let s = Scratch::new();
    vault_fixture(&s);
    let r = rig_any(&s);
    let a = queue_on(&r, free_at("o/n", "first", None), on_cloud(), 1);
    let b = queue_on(&r, free_at("o/n", "second", None), on_cloud(), 2);
    // The tool's own path: `devjob_priority` is `set_priority_and_pump`.
    let raised = r
        .svc
        .set_priority_and_pump(&b, Priority::Urgent)
        .await
        .unwrap();
    assert_eq!(raised.priority, Priority::Urgent);
    assert_eq!(raised.priority_changes.len(), 1);
    // The Studio's queue pass never starts a cloud run, raised or not.
    assert!(started(&r).is_empty());
    assert_eq!(raised.status, RunStatus::Queued);
    assert_eq!(take_all(&r, &s.notes()), vec![b.clone(), a]);
    // Taken, it is running on the cloud, and its priority can no longer change.
    let e = r.svc.set_priority(&b, Priority::Low).unwrap_err();
    assert!(e.message().contains("is running"), "{}", e.message());
}

/// The rule that orders the queue has one implementation: the same mixed queue, once as Studio
/// runs and once as cloud runs, starts in the same order on a Studio slot and on a cloud slot.
#[tokio::test]
async fn the_studio_and_the_cloud_start_the_same_queue_in_the_same_order() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig_any(&s);
    let queue_of = |host: Option<CloudPart>, tag: &str| -> Vec<String> {
        let mut ids = Vec::new();
        for (i, p) in [
            None,
            Some(Priority::Low),
            Some(Priority::High),
            Some(Priority::Normal),
            Some(Priority::Urgent),
            Some(Priority::High),
        ]
        .into_iter()
        .enumerate()
        {
            ids.push(queue_on(
                &r,
                free_at("o/n", &format!("{tag}{i}"), p),
                host.clone(),
                i as u64,
            ));
        }
        ids
    };
    let studio = queue_of(None, "s");
    let cloud = queue_of(on_cloud(), "c");
    // By place in the queue: urgent, the two highs oldest first, the two normals, low.
    let want = |ids: &[String]| -> Vec<String> {
        [4, 2, 5, 0, 3, 1].iter().map(|&i| ids[i].clone()).collect()
    };
    r.svc.pump(now_ms());
    let studio_order = started(&r);
    let cloud_order = take_all(&r, &s.notes());
    println!("studio slot order: {studio_order:?}\ncloud slot order:  {cloud_order:?}");
    assert_eq!(studio_order, want(&studio));
    assert_eq!(cloud_order, want(&cloud));
    // Neither slot took the other host's runs.
    assert!(studio_order.iter().all(|id| studio.contains(id)));
    assert!(cloud_order.iter().all(|id| cloud.contains(id)));
}

// ---- `any`: one record, whichever slot frees first ------------------------------------------

#[tokio::test]
async fn an_any_run_keeps_its_run_id_and_record_on_the_cloud() {
    let s = Scratch::new();
    remote_fixture(&s);
    let d = s.notes().join("Projects/drafts");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("busy.md"), loose("devjob-host: studio\n")).unwrap();
    std::fs::write(d.join("any-a.md"), loose("devjob-host: any\n")).unwrap();
    let r = rig(&s);
    let busy = r
        .svc
        .start(by_draft("Projects/drafts/busy.md"))
        .await
        .unwrap();
    assert_eq!(busy.status, RunStatus::Running);
    let q = r
        .svc
        .start(by_draft("Projects/drafts/any-a.md"))
        .await
        .unwrap();
    assert_eq!(q.status, RunStatus::Queued);
    let before = r.svc.get(&q.run_id).unwrap();
    assert_eq!(before.host, RunHost::Any);
    assert_eq!(before.served_host(), "studio");

    let took = r.svc.take_for_cloud(&s.notes()).unwrap();
    assert_eq!(took.run_id, q.run_id, "the same id on the cloud");
    assert_eq!(took.status, RunStatus::Running);
    assert_eq!(took.closed_reason, None, "never closed as moved");
    assert_eq!(took.created_at, before.created_at);
    assert_eq!(took.rounds.len(), 1);
    let named = r
        .svc
        .note_job(&q.run_id, "agent-s-any-a-abcde", "jesse/s-any-a")
        .unwrap();
    assert_eq!(
        named.cloud.as_ref().unwrap().job_name.as_deref(),
        Some("agent-s-any-a-abcde")
    );
    assert_eq!(named.branch, "jesse/s-any-a");
    let served = r.svc.served_one(&named);
    assert_eq!(served["host"], "cloud");
    assert_eq!(served["run_id"], q.run_id.as_str());

    // On the cloud the Studio leaves it alone: no runner, yet never interrupted, never started
    // a second time, and it holds no Studio slot.
    finish_round(&r, &busy.run_id, 0, Some("ok\n"), "");
    // An hour on: well past the settle time after which a Studio run with no runner is
    // interrupted, and past the quiet push.
    r.svc.pass(now_ms() + 60 * 60_000).await;
    let mid = r.svc.get(&q.run_id).unwrap();
    assert_eq!(mid.stalled_since, None);
    assert_eq!(mid.status, RunStatus::Running);
    assert!(!started(&r).contains(&q.run_id));

    // Its Job is reported: the same record ends, with the report on its round.
    let rel = "Inbox/2026-11-27-run-s-any-a.md";
    let end = r
        .svc
        .cloud_ended(&ended("agent-s-any-a-abcde", RunStatus::Reported, rel))
        .unwrap();
    assert_eq!(end.run_id, q.run_id);
    assert_eq!(end.status, RunStatus::Reported);
    assert_eq!(end.rounds[0].report_path.as_deref(), Some(rel));
    assert!(end.rounds[0].notified, "the cloud's watcher pushed already");
    assert_eq!(r.svc.get(&q.run_id).unwrap(), end);
    // A Job no record owns (an earlier bridge's) ends nothing.
    assert!(r
        .svc
        .cloud_ended(&ended("agent-old-zzzzz", RunStatus::Failed, rel))
        .is_none());
}

#[tokio::test]
async fn only_a_waiting_any_draft_or_a_cloud_run_is_taken_and_a_failed_take_gives_it_back() {
    let s = Scratch::new();
    remote_fixture(&s);
    let d = s.notes().join("Projects/drafts");
    std::fs::create_dir_all(&d).unwrap();
    for (f, h) in [
        ("any-a", "devjob-host: any\n"),
        ("cloud-b", "devjob-host: cloud\n"),
        ("studio-c", "devjob-host: studio\n"),
        ("none-d", ""),
        ("any-e", "devjob-host: any\n"),
    ] {
        std::fs::write(d.join(format!("{f}.md")), loose(h)).unwrap();
    }
    let r = rig(&s);
    assert!(r.svc.take_for_cloud(&s.notes()).is_none());
    // An `any` run that gets a Studio slot at once is the Studio's: it never waited.
    let first = r
        .svc
        .start(by_draft("Projects/drafts/any-a.md"))
        .await
        .unwrap();
    assert_eq!(first.status, RunStatus::Running);
    assert!(r.svc.take_for_cloud(&s.notes()).is_none());
    // Started on the Studio, a draft that does not say `any` waits for the Studio.
    for f in ["none-d", "studio-c", "cloud-b"] {
        let q = r
            .svc
            .start(by_draft(&format!("Projects/drafts/{f}.md")))
            .await
            .unwrap();
        assert_eq!(q.status, RunStatus::Queued, "{f}");
    }
    assert!(r.svc.take_for_cloud(&s.notes()).is_none());
    let e = r
        .svc
        .start(by_draft("Projects/drafts/any-e.md"))
        .await
        .unwrap();
    let took = r.svc.take_for_cloud(&s.notes()).unwrap();
    assert_eq!(took.run_id, e.run_id);
    assert!(r.svc.take_for_cloud(&s.notes()).is_none());
    // kc1 could not make its Job: back in the queue at its place, the reason recorded, taken
    // again by the next free slot.
    r.svc.return_from_cloud(&e.run_id, "kc1 refused the Job");
    let back = r.svc.get(&e.run_id).unwrap();
    assert_eq!(back.status, RunStatus::Queued);
    assert!(!back.on_cloud());
    assert_eq!(back.started_at, None);
    assert!(back
        .error
        .as_deref()
        .unwrap()
        .contains("kc1 refused the Job"));
    r.svc.pump(now_ms());
    assert_eq!(
        r.svc.get(&e.run_id).unwrap().waiting_for.as_deref(),
        Some("repo o/n")
    );
    let again = r.svc.take_for_cloud(&s.notes()).unwrap();
    assert_eq!(again.run_id, e.run_id);
    r.svc
        .note_job(&e.run_id, "agent-s-any-e-abcde", "jesse/s-any-e");
    // A run already named a Job is not given back by a late failure.
    r.svc.return_from_cloud(&e.run_id, "late");
    assert!(r.svc.get(&e.run_id).unwrap().on_cloud());
    // The cloud can never run it: an `any` run goes back to the Studio's queue as a Studio run.
    let refused = r.svc.refused_by_cloud(&e.run_id, "no").unwrap();
    assert_eq!(
        refused.status,
        RunStatus::Running,
        "named already: left alone"
    );
}

#[tokio::test]
async fn a_run_the_cloud_can_never_run_fails_if_it_asked_for_the_cloud_else_waits_for_the_studio() {
    let s = Scratch::new();
    remote_fixture(&s);
    let d = s.notes().join("Projects/drafts");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("busy.md"), loose("devjob-host: studio\n")).unwrap();
    std::fs::write(d.join("any-a.md"), loose("devjob-host: any\n")).unwrap();
    let r = rig(&s);
    r.svc
        .start(by_draft("Projects/drafts/busy.md"))
        .await
        .unwrap();
    let any = r
        .svc
        .start(by_draft("Projects/drafts/any-a.md"))
        .await
        .unwrap();
    let cloud = queue_on(&r, free_at("o/n", "c", None), on_cloud(), 1);
    assert_eq!(r.svc.take_for_cloud(&s.notes()).unwrap().run_id, cloud);
    let failed = r
        .svc
        .refused_by_cloud(&cloud, "its prompt is gone")
        .unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert!(failed.error.unwrap().contains("its prompt is gone"));
    assert_eq!(r.svc.take_for_cloud(&s.notes()).unwrap().run_id, any.run_id);
    let studio = r.svc.refused_by_cloud(&any.run_id, "no").unwrap();
    assert_eq!(studio.status, RunStatus::Queued);
    assert_eq!(studio.host, RunHost::Studio);
    assert!(r.svc.take_for_cloud(&s.notes()).is_none());
}

// ---- the record's own actions on a cloud run ------------------------------------------------

/// A cloud run taken and named its Job `job`.
fn taken(r: &Rig, s: &Scratch, slug: &str, job: &str) -> String {
    let id = queue_on(r, free_at("o/n", slug, None), on_cloud(), 1);
    assert_eq!(r.svc.take_for_cloud(&s.notes()).unwrap().run_id, id);
    r.svc.note_job(&id, job, &format!("jesse/{slug}")).unwrap();
    id
}

#[tokio::test]
async fn cancelling_a_cloud_run_deletes_a_waiting_job_and_stops_a_running_one() {
    let s = Scratch::new();
    vault_fixture(&s);
    let r = rig_any(&s);
    let cloud = Arc::new(FakeCloud::default());
    r.svc.attach_cloud(cloud.clone());
    // Queued, no Job yet: cancelled like any queued run.
    let q = queue_on(&r, free_at("o/n", "q", None), on_cloud(), 0);
    assert_eq!(
        r.svc.cancel_and_wake(&q).await.unwrap().status,
        RunStatus::Cancelled
    );
    // Its Job never started: deleted, and the run is cancelled at once.
    let a = taken(&r, &s, "a", "job-a");
    let c = r.svc.cancel_and_wake(&a).await.unwrap();
    assert_eq!(c.status, RunStatus::Cancelled);
    assert_eq!(*lock(&cloud.stopped), vec!["job-a".to_string()]);
    // Running: stopped, still running until the watcher reports it cancelled.
    *lock(&cloud.running) = true;
    let b = taken(&r, &s, "b", "job-b");
    let c = r.svc.cancel_and_wake(&b).await.unwrap();
    assert_eq!(c.status, RunStatus::Running);
    assert!(c.round().unwrap().cancel_requested_at.is_some());
    assert_eq!(lock(&cloud.stopped).last().unwrap(), "job-b");
    let rel = "Inbox/2026-11-27-run-s-b.md";
    let end = r
        .svc
        .cloud_ended(&ended("job-b", RunStatus::Cancelled, rel))
        .unwrap();
    assert_eq!(end.status, RunStatus::Cancelled);
    // No launchd job was ever touched for either.
    assert!(lock(&r.launcher.booted_out).is_empty());
}

#[tokio::test]
async fn a_failed_cloud_run_is_closed_and_archived_like_any_run_and_refuses_a_reply() {
    let s = Scratch::new();
    vault_fixture(&s);
    let r = rig_any(&s);
    let id = taken(&r, &s, "f", "job-f");
    // Running, a reply is a message, as on a running Studio run (DJ33); with no cloud attached
    // to steer, it waits for the next round.
    let out = r.svc.reply_to(&id, "go on").unwrap();
    assert_eq!(out.record.status, RunStatus::Running);
    assert_eq!(
        out.message.unwrap().channel,
        crate::hostrun::mailbox::CHANNEL_NEXT_ROUND
    );
    r.svc
        .cloud_ended(&ended("job-f", RunStatus::Failed, "Inbox/x.md"))
        .unwrap();
    // Failed with no branch on origin, there is nothing to continue: refused as on the Studio.
    let e = r.svc.reply(&id, "go on").unwrap_err();
    assert!(
        e.message().contains("nothing to continue from"),
        "{}",
        e.message()
    );
    let closed = r.svc.close(&id, None).unwrap();
    assert_eq!(closed.status, RunStatus::Closed);
    let archived = r.svc.archive(&id).unwrap();
    assert!(archived.is_archived());
}

#[tokio::test]
async fn a_capped_cloud_run_continues_as_the_same_records_next_round() {
    let s = Scratch::new();
    vault_fixture(&s);
    let r = rig_any(&s);
    let id = taken(&r, &s, "long", "job-1");
    let mut cap = ended("job-1", RunStatus::Failed, "Inbox/2026-11-27-run-s-long.md");
    cap.continued_as = Some("job-2".into());
    let mid = r.svc.cloud_ended(&cap).unwrap();
    assert_eq!(mid.run_id, id);
    assert_eq!(mid.status, RunStatus::Running);
    assert_eq!(mid.rounds.len(), 2);
    assert_eq!(mid.rounds[1].auto.as_deref(), Some(AUTO_CONTINUATION));
    let part = mid.cloud.clone().unwrap();
    assert_eq!(part.job_name.as_deref(), Some("job-2"));
    assert_eq!(part.jobs, vec!["job-1".to_string(), "job-2".to_string()]);
    // A retried pass over the first link changes nothing.
    assert_eq!(r.svc.cloud_ended(&cap).unwrap(), mid);
    let end = r
        .svc
        .cloud_ended(&ended(
            "job-2",
            RunStatus::Reported,
            "Inbox/2026-11-27-run-s-long-2.md",
        ))
        .unwrap();
    assert_eq!(end.status, RunStatus::Reported);
    let paths: Vec<_> = end.rounds.iter().map(|x| x.report_path.clone()).collect();
    assert_eq!(
        paths,
        vec![
            Some("Inbox/2026-11-27-run-s-long.md".into()),
            Some("Inbox/2026-11-27-run-s-long-2.md".into())
        ]
    );
}

// ---- the start ------------------------------------------------------------------------------

#[tokio::test]
async fn a_cloud_start_takes_the_studios_checks_plus_the_clouds_own() {
    let s = Scratch::new();
    vault_fixture(&s);
    let d = s.notes().join("Projects/drafts");
    let body = format!("Some context.\n\n## PROMPT\n{SECTION}");
    std::fs::write(
        d.join("gd5-prompt.md"),
        draft("GD5", "devjob-repo: o/n\ndevjob-host: cloud\n", &body),
    )
    .unwrap();
    std::fs::write(d.join("studio-only.md"), loose("devjob-host: studio\n")).unwrap();
    std::fs::write(d.join("cloud-ok.md"), loose("devjob-host: cloud\n")).unwrap();
    let r = rig_any(&s);
    let e = r
        .svc
        .create_on(&by_draft("Projects/drafts/studio-only.md"), on_cloud())
        .unwrap_err();
    assert!(e.message().contains("devjob-host"), "{}", e.message());
    let e = r
        .svc
        .create_on(&free_in("local/scratch", "x"), on_cloud())
        .unwrap_err();
    assert!(e
        .message()
        .contains("local repositories run only on the Studio"));
    let e = r
        .svc
        .create_on(
            &StartRequest {
                reviewer: Some(crate::hostrun::review::ReviewerArg::One("codex".into())),
                ..free_in("o/n", "rev")
            },
            on_cloud(),
        )
        .unwrap_err();
    // The Studio's own check of a reviewer, by its words: the rig has no reviewer attached.
    assert!(
        e.message().contains("this bridge has no reviewer attached"),
        "{}",
        e.message()
    );
    let ok = r
        .svc
        .create_on(&by_draft("Projects/drafts/cloud-ok.md"), on_cloud())
        .unwrap();
    let rec = r.svc.get(&ok.run_id).unwrap();
    assert_eq!(rec.host, RunHost::Cloud);
    assert_eq!(rec.branch, "jesse/s-cloud-ok");
    // Nothing names a reviewer: `[hostrun] default_reviewers` applies, as on the Studio (empty
    // in this rig, so the run is not reviewed).
    assert_eq!(
        rec.reviewers_source.as_deref(),
        Some(crate::hostrun::review::SOURCE_DEFAULT)
    );
    assert!(rec.reviewer_ids().is_empty());
    assert_eq!(ok.waiting_for.as_deref(), Some(WAITING_CLOUD));
    // The same item twice is refused as on the Studio: one live run per item.
    let first = r.svc.create_on(&item("GD5"), on_cloud());
    assert!(first.is_ok(), "{first:?}");
    let e = r.svc.create_on(&item("GD5"), on_cloud()).unwrap_err();
    assert!(matches!(e, Refusal::Live(_)), "{e:?}");
}

#[test]
fn a_studio_record_is_byte_for_byte_what_it_was() {
    let r = RunRecord {
        run_id: "X1".into(),
        ..RunRecord::default()
    };
    let v = serde_json::to_value(&r).unwrap();
    assert!(v.get("host").is_none() && v.get("cloud").is_none(), "{v}");
    let back: RunRecord = serde_json::from_value(v).unwrap();
    assert_eq!(back.host, RunHost::Studio);
    assert!(!back.is_cloud());
}

#[tokio::test]
async fn the_runs_list_serves_a_cloud_run_once_and_counts_it_as_the_clouds() {
    let s = Scratch::new();
    vault_fixture(&s);
    let r = rig_any(&s);
    let id = taken(&r, &s, "listed", "agent-listed-abcde");
    let mut list = r.svc.list_json();
    assert_eq!(
        list["counts"]["running"], 0,
        "not a Studio run: {}",
        list["counts"]
    );
    // The cloud lists the same Job, and one an earlier bridge left with no record.
    let mut owned = crate::devjob::JobView::of(&k8s_openapi::api::batch::v1::Job::default());
    owned.name = "agent-listed-abcde".into();
    owned.run_id = Some(id.clone());
    let mut legacy = owned.clone();
    legacy.name = "agent-old-zzzzz".into();
    legacy.run_id = Some("OLD1".into());
    r.svc.add_cloud(
        &mut list,
        ArchivedMode::Hide,
        &cloud::CloudRuns::Listed(vec![owned, legacy]),
    );
    let rows: Vec<(String, String)> = list["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["run_id"].as_str().unwrap().to_string(),
                x["host"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(rows.iter().filter(|x| x.0 == id).count(), 1, "{rows:?}");
    assert!(rows.contains(&(id.clone(), "cloud".into())));
    assert!(rows.contains(&("OLD1".into(), "cloud".into())));
    assert_eq!(list["counts"]["cloud"]["running"], 2, "{}", list["counts"]);
}

// ---- edges: archive, ids, a draft that cannot be read, a draft that asks for review ----------

#[tokio::test]
async fn an_archived_cloud_runs_job_is_not_listed_again_as_an_unowned_one() {
    let s = Scratch::new();
    vault_fixture(&s);
    let r = rig_any(&s);
    let id = taken(&r, &s, "gone", "agent-gone-abcde");
    r.svc
        .cloud_ended(&ended(
            "agent-gone-abcde",
            RunStatus::Reported,
            "Inbox/x.md",
        ))
        .unwrap();
    r.svc.archive(&id).unwrap();
    let mut job = crate::devjob::JobView::of(&k8s_openapi::api::batch::v1::Job::default());
    job.name = "agent-gone-abcde".into();
    job.run_id = Some(id.clone());
    job.finished = Some(true);
    job.outcome = Some("pull request opened".into());
    let mut list = r.svc.list_json();
    r.svc.add_cloud(
        &mut list,
        ArchivedMode::Hide,
        &cloud::CloudRuns::Listed(vec![job]),
    );
    let rows = list["runs"].as_array().unwrap();
    assert!(rows.iter().all(|x| x["run_id"] != id.as_str()), "{rows:?}");
    assert_eq!(list["counts"]["cloud"]["finished"], 0, "{}", list["counts"]);
}

#[tokio::test]
async fn a_new_run_never_takes_the_id_of_a_cloud_job_an_earlier_bridge_made() {
    let s = Scratch::new();
    vault_fixture(&s);
    let r = rig_any(&s);
    r.svc
        .set_cloud_ids(["s-dup".to_string()].into_iter().collect());
    let a = r.svc.create_on(&free_in("o/n", "dup"), on_cloud()).unwrap();
    assert_eq!(a.run_id, "s-dupb");
    // The Studio's own start passes over it too.
    let b = r.svc.create(&free_in("o/n", "dup")).unwrap();
    assert_eq!(b.run_id, "s-dupc");
}

#[test]
fn a_cloud_start_whose_draft_cannot_be_read_again_is_refused() {
    let e = cloud_opt_in(Some("Projects/drafts/x.md"), None).unwrap_err();
    assert!(e.contains("could not be read again"), "{e}");
    assert!(
        cloud_opt_in(None, None).is_ok(),
        "a free prompt has no draft"
    );
    assert!(cloud_opt_in(
        Some("Projects/drafts/x.md"),
        Some("---\ndevjob-host: studio\n---\n")
    )
    .is_err());
    assert!(cloud_opt_in(
        Some("Projects/drafts/x.md"),
        Some("---\ndevjob-host: cloud\n---\n")
    )
    .is_ok());
}

#[tokio::test]
async fn a_cloud_start_of_a_draft_that_asks_for_a_reviewer_is_reviewed_by_it() {
    let s = Scratch::new();
    vault_fixture(&s);
    let d = s.notes().join("Projects/drafts");
    std::fs::write(
        d.join("rev.md"),
        loose("devjob-host: cloud\ndevjob-reviewer: codex\ndevjob-merge: true\n"),
    )
    .unwrap();
    std::fs::write(
        d.join("norev.md"),
        loose("devjob-host: cloud\ndevjob-reviewer: none\n"),
    )
    .unwrap();
    let r = rig_any(&s);
    // No reviewer attached: refused by the Studio's own check, never run unreviewed.
    let e = r
        .svc
        .create_on(&by_draft("Projects/drafts/rev.md"), on_cloud())
        .unwrap_err();
    assert!(
        e.message().contains("this bridge has no reviewer attached"),
        "{}",
        e.message()
    );
    assert!(r.svc.get("s-rev").is_none(), "nothing was queued");
    // With one that knows `codex`, the draft's reviewer reviews it.
    r.svc.attach_reviewer(std::sync::Arc::new(Knows("codex")));
    let ok = r
        .svc
        .create_on(&by_draft("Projects/drafts/rev.md"), on_cloud())
        .unwrap();
    let rec = r.svc.get(&ok.run_id).unwrap();
    assert_eq!(rec.reviewer_ids(), ["codex"]);
    assert_eq!(
        rec.reviewers_source.as_deref(),
        Some(crate::hostrun::review::SOURCE_EXPLICIT)
    );
    let off = r
        .svc
        .create_on(&by_draft("Projects/drafts/norev.md"), on_cloud())
        .unwrap();
    assert!(r.svc.get(&off.run_id).unwrap().reviewer_ids().is_empty());
}

/// A reviewer registry that knows one id.
struct Knows(&'static str);

impl Reviewer for Knows {
    fn check(&self, id: &str) -> Result<ReviewerInfo, String> {
        if id != self.0 {
            return Err(format!("reviewer {id:?} is not a model this bridge knows"));
        }
        Ok(ReviewerInfo {
            id: id.into(),
            label: id.into(),
            harness: "codex".into(),
        })
    }

    fn review(
        &self,
        _call: ReviewCall,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
        Box::pin(async { Err("not in this test".to_string()) })
    }
}

#[tokio::test]
async fn a_run_taken_but_never_named_is_found_after_a_restart() {
    let s = Scratch::new();
    vault_fixture(&s);
    let r = rig_any(&s);
    let id = queue_on(&r, free_at("o/n", "mid", None), on_cloud(), 1);
    let took = r.svc.take_for_cloud(&s.notes()).unwrap();
    // The bridge stops here: no Job name. A new bridge's cloud pass sees it.
    let unnamed = r.svc.unnamed_on_cloud();
    assert_eq!(
        unnamed.iter().map(|x| x.run_id.clone()).collect::<Vec<_>>(),
        vec![id.clone()]
    );
    assert_eq!(unnamed[0], took);
    // No Job was made: queued again, and taken by the next free slot.
    r.svc
        .return_from_cloud(&id, "the bridge stopped before its Job was made");
    assert_eq!(r.svc.get(&id).unwrap().status, RunStatus::Queued);
    assert!(r.svc.unnamed_on_cloud().is_empty());
    assert_eq!(r.svc.take_for_cloud(&s.notes()).unwrap().run_id, id);
    // Its Job was made: adopted, and no longer unnamed.
    r.svc.note_job(&id, "agent-mid-abcde", "jesse/mid");
    assert!(r.svc.unnamed_on_cloud().is_empty());
}
