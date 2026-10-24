use super::*;

fn args(repo: &str, slug: &str) -> DevJobArgs {
    DevJobArgs {
        repo: Some(repo.into()),
        prompt: Some("Read README.md and make no changes.".into()),
        agent: Some("claude".into()),
        branch_slug: Some(slug.into()),
        ..DevJobArgs::default()
    }
}

fn spec() -> DevJobSpec {
    DevJobSpec::validate(args("jeremyandrews/k3s", "read-readme"), Agent::Claude).unwrap()
}

// ---- spec validation -------------------------------------------------------------

#[test]
fn the_repo_is_checked_by_pattern_only() {
    for ok in [
        "jeremyandrews/k3s",
        "tag1consulting/goose",
        "a/b",
        "some-org/repo.name_1",
        "x/.github",
    ] {
        assert!(valid_repo(ok), "{ok}");
        assert!(
            DevJobSpec::validate(args(ok, "s"), Agent::Claude).is_ok(),
            "{ok}"
        );
    }
    for bad in [
        "",
        "k3s",
        "a/b/c",
        "/b",
        "a/",
        "-a/b",
        "a b/c",
        "a/..",
        "a/.",
        "https://github.com/a/b",
        "a/b;rm",
        "a/$(x)",
    ] {
        assert!(!valid_repo(bad), "{bad}");
        assert!(
            DevJobSpec::validate(args(bad, "s"), Agent::Claude).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn the_slug_is_lowercase_letters_digits_and_hyphens_at_most_sixty() {
    for ok in ["a", "read-readme", "2026-10-24-nightly", &"a".repeat(60)] {
        assert!(valid_slug(ok), "{ok}");
    }
    for bad in [
        "",
        "Read",
        "a_b",
        "a b",
        "-a",
        "a-",
        "a/b",
        "a.b",
        &"a".repeat(61),
        "é",
    ] {
        assert!(!valid_slug(bad), "{bad}");
        assert!(
            DevJobSpec::validate(args("a/b", bad), Agent::Claude).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn the_prompt_must_be_non_empty_and_at_most_64_kib() {
    let mut a = args("a/b", "s");
    a.prompt = Some("   \n".into());
    assert!(DevJobSpec::validate(a.clone(), Agent::Claude).is_err());
    a.prompt = None;
    assert!(DevJobSpec::validate(a.clone(), Agent::Claude).is_err());
    a.prompt = Some("x".repeat(MAX_PROMPT_BYTES));
    assert!(DevJobSpec::validate(a.clone(), Agent::Claude).is_ok());
    a.prompt = Some("x".repeat(MAX_PROMPT_BYTES + 1));
    assert!(DevJobSpec::validate(a, Agent::Claude)
        .unwrap_err()
        .contains("65536"));
}

#[test]
fn the_test_command_is_at_most_1_kib() {
    let mut a = args("a/b", "s");
    a.test_command = Some("x".repeat(MAX_TEST_COMMAND_BYTES));
    assert!(DevJobSpec::validate(a.clone(), Agent::Claude).is_ok());
    a.test_command = Some("x".repeat(MAX_TEST_COMMAND_BYTES + 1));
    assert!(DevJobSpec::validate(a.clone(), Agent::Claude).is_err());
    a.test_command = Some("   ".into());
    assert_eq!(
        DevJobSpec::validate(a, Agent::Claude).unwrap().test_command,
        None
    );
}

#[test]
fn the_timeout_defaults_to_two_hours_and_is_capped_at_four() {
    let mut a = args("a/b", "s");
    let s = DevJobSpec::validate(a.clone(), Agent::Claude).unwrap();
    assert_eq!(s.timeout_minutes, 120);
    assert_eq!(s.agent_timeout_secs(), 7200);
    // The template's own pair: 7200 for the agent, 14400 for Kubernetes.
    assert_eq!(s.active_deadline_secs(), 14400);
    a.timeout_minutes = Some(30);
    assert_eq!(
        DevJobSpec::validate(a.clone(), Agent::Claude)
            .unwrap()
            .timeout_minutes,
        30
    );
    a.timeout_minutes = Some(10_000);
    let capped = DevJobSpec::validate(a.clone(), Agent::Claude).unwrap();
    assert_eq!(capped.timeout_minutes, 240);
    assert_eq!(capped.agent_timeout_secs(), 4 * 3600);
    a.timeout_minutes = Some(0);
    assert!(DevJobSpec::validate(a, Agent::Claude).is_err());
}

#[test]
fn the_agent_is_one_of_three_and_defaults_from_the_config() {
    for (raw, want) in [
        ("claude", Agent::Claude),
        ("codex", Agent::Codex),
        ("gemini", Agent::Gemini),
    ] {
        let mut a = args("a/b", "s");
        a.agent = Some(raw.into());
        assert_eq!(DevJobSpec::validate(a, Agent::Claude).unwrap().agent, want);
    }
    for bad in ["Claude", "gpt", "", "aider"] {
        let mut a = args("a/b", "s");
        a.agent = Some(bad.into());
        assert!(DevJobSpec::validate(a, Agent::Claude).is_err(), "{bad}");
    }
    let mut a = args("a/b", "s");
    a.agent = None;
    assert_eq!(
        DevJobSpec::validate(a, Agent::Codex).unwrap().agent,
        Agent::Codex
    );
}

#[test]
fn the_model_is_an_id_or_absent() {
    let mut a = args("a/b", "s");
    a.model = Some("claude-sonnet-5-5".into());
    assert_eq!(
        DevJobSpec::validate(a.clone(), Agent::Claude)
            .unwrap()
            .model
            .as_deref(),
        Some("claude-sonnet-5-5")
    );
    a.model = Some("".into());
    assert_eq!(
        DevJobSpec::validate(a.clone(), Agent::Claude)
            .unwrap()
            .model,
        None
    );
    a.model = Some("a model\nwith lines".into());
    assert!(DevJobSpec::validate(a, Agent::Claude).is_err());
}

/// THE TOOL CANNOT CHOOSE A PATH, A NAMESPACE, AN IMAGE OR A CREDENTIAL: an argument naming
/// one is refused rather than ignored.
#[test]
fn an_argument_outside_the_spec_is_refused() {
    for key in ["namespace", "image", "kubeconfig", "path", "secret", "env"] {
        let v = json!({
            "repo": "a/b", "prompt": "p", "branch_slug": "s", key: "x",
        });
        let e = serde_json::from_value::<DevJobArgs>(v).unwrap_err();
        assert!(e.to_string().contains("unknown field"), "{key}: {e}");
    }
    // The tool's schema says the same.
    let start = &tool_descriptors()[0];
    assert_eq!(start["name"], TOOL_START);
    assert_eq!(start["inputSchema"]["additionalProperties"], false);
    let props: Vec<&str> = start["inputSchema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut props = props;
    props.sort_unstable();
    assert_eq!(
        props,
        [
            "agent",
            "branch_slug",
            "model",
            "prompt",
            "repo",
            "test_command",
            "timeout_minutes"
        ]
    );
}

#[test]
fn the_server_lists_exactly_three_tools() {
    let names: Vec<String> = tool_descriptors()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, DEVJOB_TOOLS);
}

// ---- the report parser -----------------------------------------------------------

const GOOD_LOG: &str = include_str!("../../tests/fixtures/devjob/log-ok.txt");
const NO_BLOCK_LOG: &str = include_str!("../../tests/fixtures/devjob/log-no-report.txt");
const FAKE_MARKER_LOG: &str = include_str!("../../tests/fixtures/devjob/log-fake-marker.txt");
const FAILED_LOG: &str = include_str!("../../tests/fixtures/devjob/log-failed.txt");

#[test]
fn a_log_with_a_report_block_parses_every_key() {
    let r = parse_report(GOOD_LOG).unwrap();
    assert_eq!(r.status, "ok");
    assert!(r.ok());
    assert_eq!(r.failure, None);
    assert_eq!(r.repo.as_deref(), Some("jeremyandrews/k3s"));
    assert_eq!(r.agent.as_deref(), Some("claude"));
    assert_eq!(r.agent_version.as_deref(), Some("2.1.273 (Claude Code)"));
    assert_eq!(r.model.as_deref(), Some("(agent default)"));
    assert_eq!(r.branch.as_deref(), Some("jesse/first-run"));
    assert_eq!(
        r.pull_request.as_deref(),
        Some("https://github.com/jeremyandrews/k3s/pull/7")
    );
    assert_eq!(r.test_command.as_deref(), Some("(none)"));
    assert_eq!(r.test_command_exit.as_deref(), Some("skipped"));
    assert_eq!(r.agent_exit.as_deref(), Some("0"));
    assert_eq!(r.wall_seconds, Some(214));
    let msg = r.agent_final_message.as_deref().unwrap();
    assert!(msg.starts_with("Added one line to the layout block in README.md, under apps/, naming"));
    assert!(msg.ends_with("the only change; the surrounding entries were left alone."));
    assert_eq!(msg.lines().count(), 4);
    assert!(
        !msg.lines().any(|l| l.starts_with(' ')),
        "dedented: {msg:?}"
    );
    assert_eq!(RunOutcome::of(&r), RunOutcome::PullRequestOpened);
    assert_eq!(
        r.pull_request_url(),
        Some("https://github.com/jeremyandrews/k3s/pull/7")
    );
}

#[test]
fn a_log_with_no_end_marker_is_a_failed_run_whose_report_is_missing() {
    let e = parse_report(NO_BLOCK_LOG).unwrap_err();
    assert!(e.contains("JESSE-DEV-REPORT-END"), "{e}");
    let r = DevJobReport::missing(&e);
    assert_eq!(r.status, "failed");
    assert_eq!(RunOutcome::of(&r), RunOutcome::Failed);
    // A start marker alone is not a block either.
    assert!(parse_report("JESSE-DEV-REPORT\nstatus: ok\n").is_err());
    assert!(parse_report("").is_err());
}

/// THE AGENT'S OUTPUT CAN CONTAIN ANYTHING, A WHOLE FAKE BLOCK INCLUDED. Only the last block
/// counts, and a marker that is not a whole line is not a marker.
#[test]
fn a_fake_marker_earlier_in_the_log_is_ignored() {
    let r = parse_report(FAKE_MARKER_LOG).unwrap();
    assert_eq!(r.status, "ok");
    assert_eq!(r.pull_request.as_deref(), Some("none"));
    assert_eq!(r.pull_request_url(), None);
    assert_eq!(r.agent_exit.as_deref(), Some("0"));
    assert_eq!(RunOutcome::of(&r), RunOutcome::NoChanges);
    assert!(!r
        .agent_final_message
        .as_deref()
        .unwrap_or("")
        .contains("evil"));
}

#[test]
fn a_failed_report_names_its_cause() {
    let r = parse_report(FAILED_LOG).unwrap();
    assert_eq!(r.status, "failed");
    assert_eq!(
        r.failure.as_deref(),
        Some("OPENAI_API_KEY is not set (codex needs it)")
    );
    assert_eq!(r.agent_exit.as_deref(), Some("not run"));
    assert_eq!(RunOutcome::of(&r), RunOutcome::Failed);
    assert_eq!(RunOutcome::Failed.title(), "Failed");
}

#[test]
fn a_status_that_is_neither_ok_nor_failed_is_a_failure() {
    let r = parse_report("JESSE-DEV-REPORT\nstatus: maybe\nJESSE-DEV-REPORT-END\n").unwrap();
    assert_eq!(r.status, "failed");
    assert!(r.failure.unwrap().contains("maybe"));
}

#[test]
fn tail_lines_keeps_the_last_n() {
    let log = (1..=300)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let t = tail_lines(&log, REPORT_LOG_LINES);
    assert_eq!(t.len(), 200);
    assert_eq!(t[0], "101");
    assert_eq!(t[199], "300");
    assert_eq!(tail_lines("a\nb", 40), ["a", "b"]);
}

// ---- the objects -----------------------------------------------------------------

/// THE JOB IS THE K3S TEMPLATE, FIELD FOR FIELD. The template at the commit it was read from is
/// committed beside the tests as JSON; filled in with this run's values and the configured image,
/// it must equal what [`build_job`] creates, apart from the bridge's own labels, annotations and
/// `suspend`.
#[test]
fn the_job_matches_the_k3s_template_field_for_field() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/devjob/job-template.b8b6c00.json"
    ))
    .unwrap();
    let s = spec();
    let (job_name, cm_name) = object_names(&s.branch_slug, "abcde");
    fn fill(v: &mut Value, f: &dyn Fn(&str) -> String) {
        match v {
            Value::String(s) => *s = f(s),
            Value::Array(a) => a.iter_mut().for_each(|x| fill(x, f)),
            Value::Object(o) => o.values_mut().for_each(|x| fill(x, f)),
            _ => {}
        }
    }
    let filler = |t: &str| -> String {
        t.replace("agent-<slug>", &job_name)
            .replace("prompt-<slug>", &cm_name)
            .replace("<slug>", &s.branch_slug)
            .replace("<owner/name>", &s.repo)
            .replace("<agent>", s.agent.label())
            .replace("<pr title>", &s.branch_slug)
            .replace(
                "ghcr.io/jeremyandrews/jesse-dev-agent:latest",
                DEFAULT_IMAGE,
            )
    };
    let mut want_job = fixture["documents"][1].clone();
    fill(&mut want_job, &filler);
    let mut want_cm = fixture["documents"][0].clone();
    fill(&mut want_cm, &filler);
    want_cm["data"]["prompt.md"] = json!(s.prompt);

    for suspend in [false, true] {
        let mut got = serde_json::to_value(build_job(
            DEFAULT_NAMESPACE,
            &job_name,
            &cm_name,
            DEFAULT_IMAGE,
            &s,
            suspend,
            "turn",
        ))
        .unwrap();
        // The bridge's own additions, checked here and then set aside.
        let labels = json!({ LABEL_DEVJOB: "true", LABEL_SLUG: s.branch_slug });
        assert_eq!(got["metadata"]["labels"], labels);
        assert_eq!(got["spec"]["template"]["metadata"]["labels"], labels);
        assert_eq!(got["metadata"]["annotations"][ANN_ORIGIN], "turn");
        let ann: SpecAnnotation =
            serde_json::from_str(got["metadata"]["annotations"][ANN_SPEC].as_str().unwrap())
                .unwrap();
        assert_eq!(ann, SpecAnnotation::from(&s));
        assert_eq!(got["spec"]["suspend"], suspend);
        let meta = got["metadata"].as_object_mut().unwrap();
        meta.remove("labels");
        meta.remove("annotations");
        got["spec"].as_object_mut().unwrap().remove("suspend");
        got["spec"]["template"]
            .as_object_mut()
            .unwrap()
            .remove("metadata");
        assert_eq!(got, want_job);
    }

    let mut cm = serde_json::to_value(build_config_map(DEFAULT_NAMESPACE, &cm_name, &s)).unwrap();
    assert_eq!(
        cm["metadata"]["labels"],
        json!({ LABEL_DEVJOB: "true", LABEL_SLUG: s.branch_slug })
    );
    cm["metadata"].as_object_mut().unwrap().remove("labels");
    assert_eq!(cm, want_cm);
}

/// The template schedules nowhere in particular: no toleration, no node selector, no affinity,
/// so a run never lands on a tainted worker or a node under a temporary taint by our choice.
#[test]
fn the_job_adds_no_scheduling_of_its_own() {
    let s = spec();
    let job = serde_json::to_value(build_job(
        "ns",
        "agent-x",
        "prompt-x",
        DEFAULT_IMAGE,
        &s,
        false,
        "turn",
    ))
    .unwrap();
    let pod = &job["spec"]["template"]["spec"];
    for key in [
        "tolerations",
        "nodeSelector",
        "affinity",
        "nodeName",
        "priorityClassName",
    ] {
        assert!(pod.get(key).is_none(), "{key}");
    }
}

#[test]
fn the_run_values_reach_the_env() {
    let mut a = args("a/b", "fix-it");
    a.model = Some("gpt-5-codex".into());
    a.agent = Some("codex".into());
    a.test_command = Some("cargo test".into());
    a.timeout_minutes = Some(30);
    let s = DevJobSpec::validate(a, Agent::Claude).unwrap();
    let job = serde_json::to_value(build_job(
        "ns",
        "agent-x",
        "prompt-x",
        DEFAULT_IMAGE,
        &s,
        false,
        "turn",
    ))
    .unwrap();
    let env: BTreeMap<String, String> = job["spec"]["template"]["spec"]["containers"][0]["env"]
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
    assert_eq!(env["REPO"], "a/b");
    assert_eq!(env["AGENT"], "codex");
    assert_eq!(env["MODEL"], "gpt-5-codex");
    assert_eq!(env["BRANCH_SLUG"], "fix-it");
    assert_eq!(env["TEST_COMMAND"], "cargo test");
    assert_eq!(env["AGENT_TIMEOUT_SECONDS"], "1800");
    assert_eq!(job["spec"]["activeDeadlineSeconds"], 3600);
}

#[test]
fn object_names_stay_within_a_label_value() {
    let long = "a".repeat(60);
    for slug in [
        "s",
        "read-readme",
        long.as_str(),
        "a-b-c-d-e-f-g-h-i-j-k-l-m-n-o-p-q-r-s-t-u-v-w-x-y-zz",
    ] {
        let (job, cm) = object_names(slug, &name_suffix());
        assert!(job.len() <= 63, "{job}");
        assert!(cm.len() <= 63, "{cm}");
        assert!(job.starts_with("agent-") && cm.starts_with("prompt-"));
        assert!(!job.contains("--"), "{job}");
        assert!(job
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    }
    assert_ne!(name_suffix(), name_suffix());
}

// ---- the queue -------------------------------------------------------------------

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
    }
}

#[test]
fn a_third_run_queues_and_starts_when_a_slot_frees() {
    // Two running: a third must queue.
    let running = vec![view("a", 1, false, None), view("b", 2, false, None)];
    assert!(must_queue(&running, 2));
    assert!(!must_queue(&running[..1], 2));
    assert!(!must_queue(&[], 2));
    // Nothing to resume while both run.
    let mut all = running.clone();
    all.push(view("c", 3, true, None));
    assert!(to_resume(&all, 2).is_empty());
    // One finishes: the queued one is resumed.
    all[0].finished = Some(true);
    assert_eq!(to_resume(&all, 2), ["c"]);
    // A finished job never counts, suspended or not.
    all[2].finished = Some(false);
    assert!(to_resume(&all, 2).is_empty());
}

#[test]
fn the_queue_is_first_come_first_served() {
    let views = vec![
        view("late", 30, true, None),
        view("early", 10, true, None),
        view("middle", 20, true, None),
    ];
    assert_eq!(to_resume(&views, 2), ["early", "middle"]);
    // Something already waiting means a newcomer waits too, even with a slot free.
    assert!(must_queue(&views, 5));
}

#[test]
fn a_pod_held_by_a_taint_says_so() {
    let pod: Pod = serde_json::from_value(json!({
        "metadata": { "name": "agent-x-abcde-q1" },
        "status": {
            "phase": "Pending",
            "conditions": [{
                "type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                "message": "0/6 nodes are available: 1 node(s) had untolerated taint {goose-bench: }, 3 node(s) had untolerated taint {worker: true}.",
            }],
        },
    }))
    .unwrap();
    let r = pod_pending_reason(&pod).unwrap();
    assert!(
        r.starts_with("Unschedulable: 0/6 nodes are available"),
        "{r}"
    );
    assert!(r.contains("goose-bench"), "{r}");

    let pulling: Pod = serde_json::from_value(json!({
        "status": {
            "phase": "Pending",
            "conditions": [{ "type": "PodScheduled", "status": "True" }],
            "containerStatuses": [{
                "name": "agent", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
                "state": { "waiting": { "reason": "ImagePullBackOff", "message": "back-off pulling" } },
            }],
        },
    }))
    .unwrap();
    assert_eq!(
        pod_pending_reason(&pulling).as_deref(),
        Some("ImagePullBackOff: back-off pulling")
    );
    let running: Pod = serde_json::from_value(json!({ "status": { "phase": "Running" } })).unwrap();
    assert_eq!(pod_pending_reason(&running), None);
}

#[test]
fn a_job_view_reads_the_cluster_state() {
    let job: Job = serde_json::from_value(json!({
        "metadata": {
            "name": "agent-s-abcde",
            "creationTimestamp": "2026-10-24T10:00:00Z",
            "labels": { LABEL_DEVJOB: "true", LABEL_SLUG: "s" },
            "annotations": {
                ANN_REPORTED: "true",
                ANN_OUTCOME: "no changes",
                ANN_ORIGIN: "schedule:nightly",
                ANN_SPEC: serde_json::to_string(&SpecAnnotation::from(&spec())).unwrap(),
            },
        },
        "spec": {
            "suspend": false,
            "activeDeadlineSeconds": 14400,
            "template": { "spec": {
                "containers": [],
                "volumes": [{ "name": "prompt", "configMap": { "name": "prompt-s-abcde" } }],
            }},
        },
        "status": {
            "startTime": "2026-10-24T10:00:05Z",
            "completionTime": "2026-10-24T10:03:39Z",
            "conditions": [{ "type": "Complete", "status": "True" }],
        },
    }))
    .unwrap();
    let v = JobView::of(&job);
    assert_eq!(v.finished, Some(true));
    assert!(v.reported);
    assert!(!v.is_active() && !v.is_queued());
    assert_eq!(v.config_map.as_deref(), Some("prompt-s-abcde"));
    assert_eq!(v.origin, "schedule:nightly");
    assert_eq!(v.spec.repo, "jeremyandrews/k3s");
    assert_eq!(v.finished_ms.unwrap() - v.started_ms.unwrap(), 214_000);
    assert_eq!(v.outcome.as_deref(), Some("no changes"));
    assert_eq!(v.failure_reason, None);

    let failed: Job = serde_json::from_value(json!({
        "metadata": { "name": "agent-s-zzzzz" },
        "status": { "conditions": [{
            "type": "Failed", "status": "True", "reason": "DeadlineExceeded",
            "message": "Job was active longer than specified deadline",
        }]},
    }))
    .unwrap();
    let v = JobView::of(&failed);
    assert_eq!(v.finished, Some(false));
    assert_eq!(
        v.failure_reason.as_deref(),
        Some("DeadlineExceeded: Job was active longer than specified deadline")
    );
}

// ---- the vault report --------------------------------------------------------------

fn report_text(report: &DevJobReport, log: &[String], prompt: Option<&str>) -> String {
    let s = spec();
    render_report(&ReportInput {
        job_name: "agent-read-readme-abcde",
        slug: "read-readme",
        spec: &SpecAnnotation::from(&s),
        prompt,
        report,
        log_tail: log,
        queued_ms: 1_761_300_000_000,
        started_ms: Some(1_761_300_005_000),
        finished_ms: Some(1_761_300_219_000),
    })
}

#[test]
fn the_report_holds_the_spec_the_table_the_link_and_the_log() {
    let r = parse_report(GOOD_LOG).unwrap();
    let tail = tail_lines(GOOD_LOG, REPORT_LOG_LINES);
    let text = report_text(&r, &tail, Some("Do the thing.\n```\nwith a fence\n```"));
    assert!(text.starts_with("---\ndevjob: agent-read-readme-abcde\n"));
    assert!(text.contains("# Dev job read-readme: pull request opened"));
    assert!(text.contains(
        "Pull request: [https://github.com/jeremyandrews/k3s/pull/7](https://github.com/jeremyandrews/k3s/pull/7)"
    ));
    assert!(text.contains("| Repository | jeremyandrews/k3s |"));
    assert!(text.contains("| status | ok |"));
    assert!(text.contains("| wall_seconds | 214 |"));
    // A prompt with a fence is held in a longer one.
    assert!(text.contains("````text\nDo the thing.\n```\nwith a fence\n```\n````"));
    assert!(text.contains("## Log (last"));
    assert!(text.contains("JESSE-DEV-REPORT-END"));
}

#[test]
fn an_unparseable_log_is_reported_as_a_failure_with_its_tail() {
    let e = parse_report(NO_BLOCK_LOG).unwrap_err();
    let r = DevJobReport::missing(&e);
    let tail = tail_lines(NO_BLOCK_LOG, REPORT_LOG_LINES);
    let text = report_text(&r, &tail, None);
    assert!(text.contains("# Dev job read-readme: failed"));
    assert!(text.contains("| status | failed |"));
    assert!(text.contains("| failure | the log has no JESSE-DEV-REPORT-END line"));
    assert!(text.contains("(the prompt ConfigMap was already gone)"));
    assert!(text.contains(tail.last().unwrap()));
}

#[test]
fn a_second_report_the_same_day_gets_a_suffix() {
    let taken = |set: &'static [&'static str]| move |p: &str| set.contains(&p);
    assert_eq!(
        report_rel_path("2026-10-24", "s", taken(&[])),
        "Inbox/2026-10-24-dev-job-s.md"
    );
    assert_eq!(
        report_rel_path("2026-10-24", "s", taken(&["Inbox/2026-10-24-dev-job-s.md"])),
        "Inbox/2026-10-24-dev-job-s-2.md"
    );
    assert_eq!(
        report_rel_path(
            "2026-10-24",
            "s",
            taken(&[
                "Inbox/2026-10-24-dev-job-s.md",
                "Inbox/2026-10-24-dev-job-s-2.md"
            ])
        ),
        "Inbox/2026-10-24-dev-job-s-3.md"
    );
}

fn scratch_notes() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jesse-devjob-{}", random_hex()));
    std::fs::create_dir_all(dir.join("Inbox")).unwrap();
    dir
}

/// A RESTART BETWEEN WRITING A REPORT AND ANNOTATING ITS JOB WRITES NO SECOND REPORT: the file
/// names its Job, and the watcher finds it before writing.
#[tokio::test]
async fn a_report_already_written_for_a_job_is_found_and_not_written_twice() {
    let notes = scratch_notes();
    let sink = ReportSink {
        notes_root: notes.clone(),
        broker: Arc::new(LockBroker::new()),
        notifier: None,
    };
    let r = parse_report(GOOD_LOG).unwrap();
    let text = report_text(&r, &[], Some("p"));
    assert_eq!(
        find_existing_report(&notes, "read-readme", "agent-read-readme-abcde"),
        None
    );
    let rel = write_report(
        &sink,
        "agent-read-readme-abcde",
        "2026-10-24",
        "read-readme",
        &text,
    )
    .await
    .unwrap();
    assert_eq!(rel, "Inbox/2026-10-24-dev-job-read-readme.md");
    assert_eq!(
        find_existing_report(&notes, "read-readme", "agent-read-readme-abcde").as_deref(),
        Some(rel.as_str())
    );
    // Another run with the same slug is a different Job: not found, and written beside it.
    assert_eq!(
        find_existing_report(&notes, "read-readme", "agent-read-readme-zzzzz"),
        None
    );
    let second = write_report(
        &sink,
        "agent-read-readme-zzzzz",
        "2026-10-24",
        "read-readme",
        &text,
    )
    .await
    .unwrap();
    assert_eq!(second, "Inbox/2026-10-24-dev-job-read-readme-2.md");
    // The write lock was released both times.
    assert_eq!(sink.broker.held_count(), 0);
    let _ = std::fs::remove_dir_all(&notes);
}

#[test]
fn the_push_says_the_outcome_the_slug_and_the_wall_time() {
    let p: Value = serde_json::from_slice(&build_devjob_payload(
        RunOutcome::NoChanges.title(),
        &format!("read-readme · {}", human_secs(214)),
        "agent-x",
    ))
    .unwrap();
    assert_eq!(p["aps"]["alert"]["title"], "No changes");
    assert_eq!(p["aps"]["alert"]["body"], "read-readme · 3m 34s");
    assert_eq!(p["devjob"], "agent-x");
    assert_eq!(human_secs(59), "59s");
    assert_eq!(human_secs(3720), "1h 02m");
    let r = DevJobReport::default();
    assert_eq!(wall_secs(&r, Some(1_000), Some(215_000)), Some(214));
}

// ---- settings and the boot refusals ------------------------------------------------

fn kubeconfig_file() -> PathBuf {
    let p = std::env::temp_dir().join(format!("jesse-devjob-kc-{}.yaml", random_hex()));
    std::fs::write(
        &p,
        "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster:\n    server: https://127.0.0.1:6443\n\
         contexts:\n- name: x\n  context:\n    cluster: c\n    user: u\ncurrent-context: x\n\
         users:\n- name: u\n  user:\n    token: not-a-real-token\n",
    )
    .unwrap();
    p
}

fn enabled(kubeconfig: &Path) -> DevJobSettings {
    DevJobSettings::from_toml(
        Some(DevJobToml {
            enabled: true,
            kubeconfig: Some(kubeconfig.to_path_buf()),
            ..DevJobToml::default()
        }),
        "/home/u",
    )
}

#[test]
fn off_by_default_and_off_checks_nothing() {
    let s = DevJobSettings::default();
    assert!(!s.enabled);
    assert!(s.problems(false).is_empty());
    assert_eq!(s.namespace, "jesse-dev");
    assert_eq!(s.max_concurrent, 2);
    assert_eq!(s.image, DEFAULT_IMAGE);
    assert!(is_digest_reference(DEFAULT_IMAGE));
    assert_eq!(s.default_agent, Ok(Agent::Claude));
    let toml_default = DevJobSettings::from_toml(None, "/home/u");
    assert_eq!(
        toml_default.kubeconfig,
        PathBuf::from("/home/u/.kube/ks-jesse.yaml")
    );
}

#[test]
fn a_ready_table_boots() {
    let kc = kubeconfig_file();
    assert!(
        enabled(&kc).problems(true).is_empty(),
        "{:?}",
        enabled(&kc).problems(true)
    );
    let _ = std::fs::remove_file(kc);
}

#[test]
fn it_refuses_to_boot_without_the_gateway() {
    let kc = kubeconfig_file();
    let p = enabled(&kc).problems(false);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].contains("[devjob] enabled needs [gateway] enabled"),
        "{p:?}"
    );
    let _ = std::fs::remove_file(kc);
}

#[test]
fn it_refuses_to_boot_on_an_unreadable_kubeconfig() {
    let missing = std::env::temp_dir().join("jesse-devjob-no-such-kubeconfig.yaml");
    let p = enabled(&missing).problems(true);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].contains("[devjob] kubeconfig") && p[0].contains("unreadable"),
        "{p:?}"
    );
    let garbage = std::env::temp_dir().join(format!("jesse-devjob-garbage-{}.yaml", random_hex()));
    std::fs::write(&garbage, ": : not yaml [").unwrap();
    assert!(enabled(&garbage).problems(true)[0].contains("unreadable"));
    let _ = std::fs::remove_file(garbage);
}

#[test]
fn it_refuses_to_boot_on_an_image_not_pinned_by_digest() {
    let kc = kubeconfig_file();
    for image in [
        "ghcr.io/jeremyandrews/jesse-dev-agent:latest",
        "ghcr.io/jeremyandrews/jesse-dev-agent",
        "ghcr.io/jeremyandrews/jesse-dev-agent@sha256:abc",
        "ghcr.io/jeremyandrews/jesse-dev-agent@sha512:90c3d190f98d7fca5b2f31b90585a7528d203ab797696b98a77ce4ca4f328304",
        "@sha256:90c3d190f98d7fca5b2f31b90585a7528d203ab797696b98a77ce4ca4f328304",
    ] {
        let mut s = enabled(&kc);
        s.image = image.into();
        let p = s.problems(true);
        assert_eq!(p.len(), 1, "{image}: {p:?}");
        assert!(p[0].contains("[devjob] image") && p[0].contains("digest"), "{p:?}");
    }
    let _ = std::fs::remove_file(kc);
}

#[test]
fn the_table_parses_and_rejects_unknown_keys() {
    let t: DevJobToml = toml::from_str(
        "enabled = true\nkubeconfig = \"~/.kube/x.yaml\"\nnamespace = \"jesse-dev\"\nmax_concurrent = 1\ndefault_agent = \"codex\"\n",
    )
    .unwrap();
    let s = DevJobSettings::from_toml(Some(t), "/home/u");
    assert_eq!(s.kubeconfig, PathBuf::from("/home/u/.kube/x.yaml"));
    assert_eq!(s.max_concurrent, 1);
    assert_eq!(s.default_agent, Ok(Agent::Codex));
    assert!(toml::from_str::<DevJobToml>("enabled = true\nrepos = [\"a/b\"]\n").is_err());
    let bad_agent = DevJobSettings::from_toml(
        Some(DevJobToml {
            enabled: true,
            default_agent: Some("aider".into()),
            ..DevJobToml::default()
        }),
        "/home/u",
    );
    assert!(bad_agent
        .problems(true)
        .iter()
        .any(|p| p.contains("default_agent")));
}

#[test]
fn validate_devjob_names_the_table() {
    let kc = kubeconfig_file();
    let cfg = Config {
        devjob: enabled(&kc),
        ..crate::testutil::test_config()
    };
    let errors = validate_devjob(&cfg);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("[gateway] enabled"));
    let _ = std::fs::remove_file(kc);
}

// ---- the schedule entry kind ----------------------------------------------------------

fn schedule(text: &str) -> Schedule {
    #[derive(Deserialize)]
    struct File {
        schedule: Vec<ScheduleToml>,
    }
    let f: File = toml::from_str(text).unwrap();
    let ids = vec!["opus".to_string()];
    validate_schedule_with(
        &f.schedule,
        &ValidationContext {
            model_ids: Some(&ids),
            ..ValidationContext::default()
        },
    )
}

#[test]
fn a_devjob_entry_parses() {
    let s = schedule(
        r#"
[[schedule]]
id = "nightly-deps"
at = "02:10"
kind = "devjob"
repo = "jeremyandrews/k3s"
agent = "codex"
model = "gpt-5-codex"
branch_slug = "deps-{date}"
test_command = "cargo test"
timeout = 90
prompt_file = "Prompts/deps.md"
expect_output = ["Inbox/{date}-dev-job-*.md"]
profiles = ["home"]
notify = false
"#,
    );
    assert!(s.invalid.is_empty(), "{:?}", s.invalid);
    let j = s.get("nightly-deps").unwrap();
    let d = j.devjob.as_ref().unwrap();
    assert_eq!(d.repo, "jeremyandrews/k3s");
    assert_eq!(d.agent, Agent::Codex);
    // The dev agent's model, not checked against the registry (which knows only "opus").
    assert_eq!(d.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(j.model, None);
    assert_eq!(d.branch_slug, "deps-{date}");
    assert_eq!(d.timeout_minutes, Some(90));
    assert_eq!(j.expect_output, ["Inbox/{date}-dev-job-*.md"]);
    assert!(!j.notify);
    let spec = d
        .spec("deps-2026-10-24".into(), "bump them".into())
        .unwrap();
    assert_eq!(spec.branch_slug, "deps-2026-10-24");
    assert_eq!(spec.test_command.as_deref(), Some("cargo test"));
    assert_eq!(spec.timeout_minutes, 90);
}

#[test]
fn a_devjob_link_keeps_after_and_its_chain() {
    let s = schedule(
        r#"
[[schedule]]
id = "head"
at = "03:00"
prompt = "do the day"

[[schedule]]
id = "dev"
after = "head"
after_on = "any"
kind = "devjob"
repo = "a/b"
agent = "claude"
branch_slug = "nightly"
prompt_file = "p.md"
"#,
    );
    assert!(s.invalid.is_empty(), "{:?}", s.invalid);
    assert_eq!(s.chain("head"), ["head", "dev"]);
    assert!(s.get("dev").unwrap().devjob.is_some());
    assert!(s.get("head").unwrap().devjob.is_none());
}

#[test]
fn a_devjob_entry_that_is_wrong_is_disabled_by_name() {
    let base = "id = \"x\"\nat = \"02:00\"\nkind = \"devjob\"\n";
    for (extra, why) in [
        ("agent = \"claude\"\nbranch_slug = \"s\"\nprompt_file = \"p.md\"\n", "owner/name"),
        ("repo = \"a/b\"\nbranch_slug = \"s\"\nprompt_file = \"p.md\"\n", "needs `agent`"),
        ("repo = \"a/b\"\nagent = \"claude\"\nprompt_file = \"p.md\"\n", "branch_slug"),
        ("repo = \"a/b\"\nagent = \"claude\"\nbranch_slug = \"s\"\n", "needs `prompt_file`"),
        ("repo = \"a/b\"\nagent = \"claude\"\nbranch_slug = \"S_{date}\"\nprompt_file = \"p.md\"\n", "branch_slug"),
        ("repo = \"a/b\"\nagent = \"aider\"\nbranch_slug = \"s\"\nprompt_file = \"p.md\"\n", "agent"),
        ("repo = \"a/b\"\nagent = \"claude\"\nbranch_slug = \"s\"\nprompt = \"inline\"\n", "prompt"),
        ("repo = \"a/b\"\nagent = \"claude\"\nbranch_slug = \"s\"\nprompt_file = \"p.md\"\nmode = \"tell\"\n", "mode"),
        ("repo = \"a/b\"\nagent = \"claude\"\nbranch_slug = \"s\"\nprompt_file = \"p.md\"\ntimeout_secs = 60\n", "timeout_secs"),
        ("repo = \"a/b\"\nagent = \"claude\"\nbranch_slug = \"s\"\nprompt_file = \"p.md\"\ntimeout = 0\n", "timeout"),
    ] {
        let s = schedule(&format!("[[schedule]]\n{base}{extra}"));
        assert_eq!(s.invalid.len(), 1, "{extra}");
        assert!(s.invalid[0].reason.contains(why), "{extra}: {}", s.invalid[0].reason);
    }
    let unknown =
        schedule("[[schedule]]\nid = \"x\"\nat = \"02:00\"\nkind = \"pod\"\nprompt = \"p\"\n");
    assert!(unknown.invalid[0]
        .reason
        .contains("the only kind is \"devjob\""));
}

#[test]
fn a_turn_entry_with_devjob_keys_is_disabled_by_name() {
    let s = schedule("[[schedule]]\nid = \"x\"\nat = \"02:00\"\nprompt = \"p\"\nrepo = \"a/b\"\n");
    assert!(
        s.invalid[0].reason.contains("`repo` is set on a turn job"),
        "{:?}",
        s.invalid
    );
    // A turn's `model` is still checked against the registry.
    let s = schedule(
        "[[schedule]]\nid = \"x\"\nat = \"02:00\"\nprompt = \"p\"\nmodel = \"gpt-5-codex\"\n",
    );
    assert!(
        s.invalid[0].reason.contains("model registry"),
        "{:?}",
        s.invalid
    );
}

#[test]
fn a_scheduled_devjob_outcome_comes_from_its_report_and_its_contract() {
    let (o, r) = devjob_outcome(
        "agent-x",
        Some("pull request opened"),
        Some("https://github.com/a/b/pull/1"),
        None,
    );
    assert_eq!(o, Outcome::Ran);
    assert_eq!(
        r,
        "dev job agent-x: pull request opened (https://github.com/a/b/pull/1)"
    );
    assert_eq!(
        devjob_outcome("agent-x", Some("no changes"), None, None).0,
        Outcome::Ran
    );
    assert_eq!(
        devjob_outcome("agent-x", Some("failed"), None, None).0,
        Outcome::Failed
    );
    // A failed run is failed even when the contract is unmet: the report is the stronger fact.
    assert_eq!(
        devjob_outcome("agent-x", Some("failed"), None, Some("Inbox/x.md")).0,
        Outcome::Failed
    );
    let (o, r) = devjob_outcome(
        "agent-x",
        Some("no changes"),
        None,
        Some("Inbox/2026-10-24-dev-job-*.md"),
    );
    assert_eq!(o, Outcome::FiredNoOutput);
    assert!(r.contains("Inbox/2026-10-24-dev-job-*.md"));
}
