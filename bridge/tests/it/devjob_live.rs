//! ONE REAL DEV JOB, end to end, through the bridge's own code: the spec, the ConfigMap and the
//! Job, the watcher, the report in a scratch notes directory, the push, the annotation and the
//! ConfigMap's deletion. Behind the `devjob-live` feature, and skipped when the kubeconfig is
//! absent (`JESSE_DEVJOB_KUBECONFIG`, else `~/.kube/ks-jesse.yaml`).
//!
//! The run asks the agent to read `README.md` in `jeremyandrews/k3s` and change nothing, so it
//! must end `status: ok` with `pull_request: none`, and opens no pull request.
//!
//! The second test does the same through a strand entry: a scratch `Strands/Devjob-Live.md`
//! with one eligible line, picked and started by `strandjobs::fire`, reported with the strand
//! keys, and a second fire that finds the step `in flight` while its report sits in the Inbox.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jesse_bridge::devjob::{
    DevJobArgs, DevJobSettings, DevJobSpec, DevJobToml, DevJobs, Notifier, ReportSink, ANN_REPORTED,
};
use jesse_bridge::strandjobs::{self, StrandFire};
use jesse_bridge::{LockBroker, StrandTemplate};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::ConfigMap;

/// THE TWO RUNS TAKE TURNS. Each test's `reconcile` reports every finished, unreported Job in the
/// namespace into its own scratch directory, as the one watcher a bridge runs would; two at once
/// would each report the other's run as well.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Default)]
struct Recorder(Mutex<Vec<(String, String, String)>>);

impl Notifier for Recorder {
    fn notify(
        &self,
        title: String,
        body: String,
        job: String,
    ) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        self.0.lock().unwrap().push((title, body, job));
        Box::pin(async {})
    }
}

fn kubeconfig() -> Option<PathBuf> {
    let p = std::env::var("JESSE_DEVJOB_KUBECONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".kube/ks-jesse.yaml")
        });
    p.is_file().then_some(p)
}

#[tokio::test]
async fn a_real_claude_run_reads_the_readme_and_reports_no_changes() {
    let _serial = SERIAL.lock().await;
    let Some(kc) = kubeconfig() else {
        eprintln!("devjob_live: no kubeconfig; skipped");
        return;
    };
    let settings = DevJobSettings::from_toml(
        Some(DevJobToml {
            enabled: true,
            kubeconfig: Some(kc.clone()),
            ..DevJobToml::default()
        }),
        "",
    );
    assert!(
        settings.problems(true).is_empty(),
        "{:?}",
        settings.problems(true)
    );
    let namespace = settings.namespace.clone();
    let devjobs = Arc::new(DevJobs::connect(settings).await.expect("a client"));
    let notes = std::env::temp_dir().join(format!(
        "jesse-devjob-live-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    std::fs::create_dir_all(notes.join("Inbox")).unwrap();
    let pushes = Arc::new(Recorder::default());
    devjobs.attach(ReportSink {
        notes_root: notes.clone(),
        broker: Arc::new(LockBroker::new()),
        notifier: Some(pushes.clone()),
    });

    let spec = DevJobSpec::validate(
        DevJobArgs {
            repo: Some("jeremyandrews/k3s".into()),
            prompt: Some("Read README.md and make no changes.".into()),
            agent: Some("claude".into()),
            branch_slug: Some("devjob-live-check".into()),
            timeout_minutes: Some(30),
            ..DevJobArgs::default()
        },
        devjobs.default_agent(),
    )
    .unwrap();
    let asked = Instant::now();
    let started = devjobs.start(&spec, "turn").await.expect("the run starts");
    eprintln!(
        "devjob_live: started {} (queued: {}) in {:?}",
        started.job_name,
        started.queued,
        asked.elapsed()
    );
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "a job name in seconds"
    );

    // The watcher's own pass, driven here so the test owns its pace.
    let deadline = Instant::now() + Duration::from_secs(45 * 60);
    let view = loop {
        devjobs.reconcile().await;
        let v = devjobs
            .view(&started.job_name)
            .await
            .expect("the Job reads")
            .expect("the Job exists");
        if v.reported {
            break v;
        }
        if let Ok(Some(r)) = devjobs.run(&started.job_name).await {
            eprintln!(
                "devjob_live: {:?} {}",
                r.phase,
                r.waiting_reason.unwrap_or_default()
            );
        }
        assert!(
            Instant::now() < deadline,
            "the run reported within 45 minutes"
        );
        tokio::time::sleep(Duration::from_secs(15)).await;
    };

    let run = devjobs.run(&started.job_name).await.unwrap().unwrap();
    let report = run.report.expect("the finished run's report");
    let rel = view
        .report_path
        .clone()
        .expect("the report path annotation");
    let text = std::fs::read_to_string(notes.join(&rel)).expect("the Inbox report");
    println!("==== {rel} ====\n{text}==== end ====");
    assert_eq!(report.status, "ok", "{report:?}");
    assert_eq!(report.pull_request.as_deref(), Some("none"), "{report:?}");
    assert!(rel.starts_with("Inbox/") && rel.contains("-dev-job-devjob-live-check"));
    assert!(text.contains(&format!("devjob: {}", started.job_name)));
    assert_eq!(view.outcome.as_deref(), Some("no changes"));

    // The ConfigMap is gone and the Job carries the annotation.
    let client = kube::Client::try_from(
        kube::Config::from_custom_kubeconfig(
            kube::config::Kubeconfig::read_from(&kc).unwrap(),
            &kube::config::KubeConfigOptions::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let cms: kube::Api<ConfigMap> = kube::Api::namespaced(client.clone(), &namespace);
    let cm = view
        .config_map
        .clone()
        .expect("the Job names its ConfigMap");
    assert!(
        cms.get_opt(&cm).await.unwrap().is_none(),
        "{cm} was deleted"
    );
    let jobs: kube::Api<Job> = kube::Api::namespaced(client, &namespace);
    let job = jobs.get(&started.job_name).await.unwrap();
    assert_eq!(
        job.metadata
            .annotations
            .unwrap_or_default()
            .get(ANN_REPORTED)
            .map(String::as_str),
        Some("true")
    );

    // One push, titled with the outcome; a second pass writes and pushes nothing more.
    devjobs.reconcile().await;
    let p = pushes.0.lock().unwrap().clone();
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0].0, "No changes");
    assert!(p[0].1.starts_with("devjob-live-check · "), "{p:?}");
    let inbox: Vec<_> = std::fs::read_dir(notes.join("Inbox")).unwrap().collect();
    assert_eq!(inbox.len(), 1, "exactly one Inbox report");
    eprintln!("devjob_live: notes directory {}", notes.display());
}

/// Drive the watcher until `job` is reported, for at most 45 minutes.
async fn wait_reported(devjobs: &DevJobs, job: &str) -> jesse_bridge::devjob::JobView {
    let deadline = Instant::now() + Duration::from_secs(45 * 60);
    loop {
        devjobs.reconcile().await;
        let v = devjobs
            .view(job)
            .await
            .expect("the Job reads")
            .expect("the Job exists");
        if v.reported {
            return v;
        }
        if let Ok(Some(r)) = devjobs.run(job).await {
            eprintln!(
                "devjob_live: {:?} {}",
                r.phase,
                r.waiting_reason.unwrap_or_default()
            );
        }
        assert!(
            Instant::now() < deadline,
            "the run reported within 45 minutes"
        );
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[tokio::test]
async fn a_strand_entry_runs_its_next_step_and_then_finds_it_in_flight() {
    let _serial = SERIAL.lock().await;
    let Some(kc) = kubeconfig() else {
        eprintln!("devjob_live: no kubeconfig; skipped");
        return;
    };
    let settings = DevJobSettings::from_toml(
        Some(DevJobToml {
            enabled: true,
            kubeconfig: Some(kc.clone()),
            ..DevJobToml::default()
        }),
        "",
    );
    let namespace = settings.namespace.clone();
    let devjobs = Arc::new(DevJobs::connect(settings).await.expect("a client"));
    // An earlier run of this test leaves its Job for the TTL (two days), and that Job holds
    // `Devjob-Live L1` in flight, as it must. Delete this test's own leftovers, and only ones
    // already reported, so the test can run again.
    {
        let client = kube::Client::try_from(
            kube::Config::from_custom_kubeconfig(
                kube::config::Kubeconfig::read_from(&kc).unwrap(),
                &kube::config::KubeConfigOptions::default(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        let jobs: kube::Api<Job> = kube::Api::namespaced(client, &namespace);
        for j in devjobs
            .strand_jobs("Devjob-Live")
            .await
            .expect("the Jobs list")
        {
            let name = j.metadata.name.clone().unwrap_or_default();
            let reported = j
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(ANN_REPORTED))
                .is_some_and(|v| v == "true");
            assert!(
                reported,
                "{name} from an earlier run is not reported yet; wait for it"
            );
            jobs.delete(&name, &kube::api::DeleteParams::background())
                .await
                .expect("the leftover Job deletes");
            eprintln!("devjob_live: deleted leftover {name}");
        }
        let gone = Instant::now() + Duration::from_secs(120);
        while !devjobs.strand_jobs("Devjob-Live").await.unwrap().is_empty() {
            assert!(
                Instant::now() < gone,
                "the leftover Jobs went within two minutes"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    let notes = std::env::temp_dir().join(format!(
        "jesse-devjob-strand-live-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    for d in ["Inbox", "Strands", "Projects/drafts"] {
        std::fs::create_dir_all(notes.join(d)).unwrap();
    }
    std::fs::write(
        notes.join("Strands/Devjob-Live.md"),
        "---\ngroup: personal\nstate: active\nupdated: 2026-10-25\n---\n\n# Devjob Live\n\n\
         **Now:** the live strand check.\n\n## Drafts\n\n\
         - [ ] **L1** Read the README. [[Projects/drafts/devjob-live-prompt]]\n",
    )
    .unwrap();
    std::fs::write(
        notes.join("Projects/drafts/devjob-live-prompt.md"),
        "---\nstrand: Devjob-Live\nstrand-id: L1\ndevjob-repo: jeremyandrews/k3s\n---\n\
         Read README.md and make no changes.\n",
    )
    .unwrap();
    let pushes = Arc::new(Recorder::default());
    devjobs.attach(ReportSink {
        notes_root: notes.clone(),
        broker: Arc::new(LockBroker::new()),
        notifier: Some(pushes.clone()),
    });
    let template = StrandTemplate {
        note: "Devjob-Live".into(),
        agent: None,
        model: None,
        timeout_minutes: Some(30),
    };
    let date = chrono::Local::now().format("%Y%m%d").to_string();
    let origin = "schedule:devjob-live-strand";

    let fired = strandjobs::fire(&devjobs, &notes, &template, &date, origin).await;
    let StrandFire::Started {
        started,
        step,
        spec,
    } = fired
    else {
        panic!("the first fire did not start a run: {fired:?}");
    };
    eprintln!(
        "devjob_live: strand started {} (queued: {}) for {} {}",
        started.job_name, started.queued, step.note, step.id
    );
    assert_eq!(step.id, "L1");
    assert_eq!(step.prompt, "Projects/drafts/devjob-live-prompt.md");
    assert_eq!(spec.repo, "jeremyandrews/k3s");
    assert_eq!(spec.prompt, "Read README.md and make no changes.\n");
    assert_eq!(spec.branch_slug, format!("devjob-live-l1-{date}"));

    let view = wait_reported(&devjobs, &started.job_name).await;
    let run = devjobs.run(&started.job_name).await.unwrap().unwrap();
    let report = run.report.expect("the finished run's report");
    let rel = view
        .report_path
        .clone()
        .expect("the report path annotation");
    let text = std::fs::read_to_string(notes.join(&rel)).expect("the Inbox report");
    println!("==== {rel} ====\n{text}==== end ====");
    assert_eq!(report.status, "ok", "{report:?}");
    assert_eq!(report.pull_request.as_deref(), Some("none"), "{report:?}");
    assert!(text.contains(&format!("devjob: {}\n", started.job_name)));
    assert!(text.contains(
        "\nstrand: Devjob-Live\nstrand-id: L1\nprompt: Projects/drafts/devjob-live-prompt.md\n---\n"
    ));
    assert!(text.contains("| Strand | Devjob-Live L1 |"));
    // A scheduled run is pushed by the scheduler, never by the watcher.
    assert!(pushes.0.lock().unwrap().is_empty());

    // The report sits in Inbox/: a second fire picks nothing, and says why.
    match strandjobs::fire(&devjobs, &notes, &template, &date, origin).await {
        StrandFire::Nothing(reason) => {
            eprintln!("devjob_live: second fire: {reason}");
            assert_eq!(
                reason,
                "nothing eligible in strand Devjob-Live: L1: in flight"
            );
        }
        other => panic!("the second fire was not nothing-eligible: {other:?}"),
    }
    // The bridge wrote nothing but the report: the note and the prompt are as written.
    let inbox: Vec<_> = std::fs::read_dir(notes.join("Inbox")).unwrap().collect();
    assert_eq!(inbox.len(), 1, "exactly one Inbox report");
    eprintln!("devjob_live: notes directory {}", notes.display());
}
