//! ONE REAL DEV JOB, end to end, through the bridge's own code: the spec, the ConfigMap and the
//! Job, the watcher, the report in a scratch notes directory, the push, the annotation and the
//! ConfigMap's deletion. Behind the `devjob-live` feature, and skipped when the kubeconfig is
//! absent (`JESSE_DEVJOB_KUBECONFIG`, else `~/.kube/ks-jesse.yaml`).
//!
//! The run asks the agent to read `README.md` in `jeremyandrews/k3s` and change nothing, so it
//! must end `status: ok` with `pull_request: none`, and opens no pull request.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jesse_bridge::devjob::{
    DevJobArgs, DevJobSettings, DevJobSpec, DevJobToml, DevJobs, Notifier, ReportSink, ANN_REPORTED,
};
use jesse_bridge::LockBroker;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::ConfigMap;

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
