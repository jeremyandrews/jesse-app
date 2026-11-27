//! Messages to a running run (`devjob_reply` while it runs): queued in its mailbox, handed to the
//! agent by the steer hook after its current tool call, and never lost when the round ends first.
//! The hook's own logic is tested in `mailbox::tests`; here, the record and the round.

use super::*;

/// The steer hook, as the agent's CLI would run it after a tool call, in the run's mailbox.
fn hook_fires(r: &Rig, run_id: &str, at: &str) -> Option<String> {
    let places = mailbox::HookPlaces {
        mailbox: Some(r.svc.settings.run_dir(run_id).join(mailbox::MAILBOX_DIR)),
        ..mailbox::HookPlaces::default()
    };
    let mut out = Vec::new();
    mailbox::run_hook(&places, at, &mut out);
    (!out.is_empty()).then(|| String::from_utf8(out).unwrap())
}

/// The runner and `jesse-hook` beside it, as deployed, in the scratch folder.
fn place_runner(s: &Scratch) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = s.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["jesse-runner", "jesse-hook"] {
        let p = bin.join(name);
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin.join("jesse-runner")
}

/// A Studio rig whose runner has the steer hook beside it.
fn steer_rig(s: &Scratch) -> Rig {
    let mut st = settings(s);
    st.runner = place_runner(s);
    rig_with(s, st)
}

#[tokio::test]
async fn a_reply_to_a_running_studio_run_is_queued_for_after_the_current_tool_call() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = steer_rig(&s);
    r.svc.start(free("steer")).await.unwrap();
    let id = "s-steer";
    let out = r
        .svc
        .reply_full_and_pump(id, "Narrow it to the README only.")
        .await
        .unwrap();
    // Not a round: the run keeps running its first one.
    assert_eq!(out.record.status, RunStatus::Running);
    assert_eq!(out.record.rounds.len(), 1);
    let m = out.message.clone().expect("queued as a message");
    assert_eq!(m.channel, mailbox::CHANNEL_TOOL_CALL);
    assert_eq!(m.round, 1);
    assert!(m.delivered_at.is_none());
    let said = out.delivery().unwrap();
    assert!(
        said.contains("queued for delivery after the agent's current tool call"),
        "{said}"
    );
    // In the mailbox, as one file named by its time and id.
    let mb = r.svc.settings.run_dir(id).join(mailbox::MAILBOX_DIR);
    let files = mailbox::messages(&mb);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, m.file_name());
    assert_eq!(files[0].1.text, "Narrow it to the README only.");
    // The hook delivers it; the bridge reads the marker into the record.
    let ctx = hook_fires(&r, id, "2026-11-27T15:00:00.000Z").unwrap();
    assert!(ctx.contains("Narrow it to the README only."));
    let rec = r.svc.sync_messages(id).unwrap();
    assert_eq!(
        rec.messages[0].delivered_at.as_deref(),
        Some("2026-11-27T15:00:00.000Z")
    );
    // A reported run's reply is still a round, as before.
    finish_round(&r, id, 0, Some("Done.\n"), "");
    r.svc.pass(now_ms()).await;
    let text =
        std::fs::read_to_string(s.notes().join(format!("Inbox/{}-run-{id}.md", today()))).unwrap();
    assert!(!text.contains("UNDELIVERED"), "{text}");
    let out = r.svc.reply_full_and_pump(id, "Next.").await.unwrap();
    assert!(out.message.is_none());
    assert_eq!(out.record.rounds.len(), 2);
    assert_eq!(out.record.round().unwrap().text.as_deref(), Some("Next."));
}

#[tokio::test]
async fn an_undelivered_message_is_named_in_the_report_and_joins_the_next_round() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = steer_rig(&s);
    r.svc.start(free("late")).await.unwrap();
    let id = "s-late";
    let first = r.svc.reply_to(id, "First correction.").unwrap();
    let second = r.svc.reply_to(id, "Second correction.").unwrap();
    // The round ends before any tool call: the hook never ran.
    finish_round(&r, id, 0, Some("Finished the original scope.\n"), "");
    r.svc.pass(now_ms()).await;
    let rec = r.svc.get(id).unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    let text =
        std::fs::read_to_string(s.notes().join(format!("Inbox/{}-run-{id}.md", today()))).unwrap();
    assert!(
        text.contains("UNDELIVERED MESSAGES: 2 messages"),
        "the report names them: {text}"
    );
    assert!(text.contains(&first.message.as_ref().unwrap().id));
    assert!(text.contains("Second correction."));
    // The push leads with it too: a run with undelivered messages is not done silently.
    assert!(lock(&r.rec.pushes)[0].1.starts_with("UNDELIVERED MESSAGES"));
    // The next round's text starts with both, in the order sent, then the reply.
    let out = r.svc.reply_full_and_pump(id, "Carry on.").await.unwrap();
    let round = out.record.round().unwrap();
    assert_eq!(round.n, 2);
    let t = round.text.as_deref().unwrap();
    assert!(t.starts_with(mailbox::HEADER), "{t}");
    let (a, b) = (
        t.find("First correction.").unwrap(),
        t.find("Second correction.").unwrap(),
    );
    assert!(a < b);
    assert!(t.ends_with("\n\nCarry on."), "{t}");
    for m in &out.record.messages {
        assert_eq!(m.carried_to_round, Some(2));
        assert_eq!(m.channel, mailbox::CHANNEL_NEXT_ROUND);
        // Delivered when the round that carries them started.
        assert_eq!(m.delivered_at, round.started_at);
    }
    // Out of the mailbox, so round 2's hook never hands them over a second time.
    let mb = r.svc.settings.run_dir(id).join(mailbox::MAILBOX_DIR);
    assert!(mailbox::messages(&mb).is_empty());
    assert_eq!(hook_fires(&r, id, "2026-11-27T15:00:00.000Z"), None);
    let _ = second;
}

#[tokio::test]
async fn an_interrupted_round_carries_its_waiting_message_into_the_automatic_round() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = steer_rig(&s);
    r.svc.start(free("cut")).await.unwrap();
    let id = "s-cut";
    r.svc.reply_to(id, "Use the other branch.").unwrap();
    let dir = r.svc.settings.run_dir(id);
    update_run(&dir, |x| {
        x.round_mut().unwrap().started_at = Some(rfc(now_ms() - 60_000));
    })
    .unwrap();
    r.launcher.gone(id);
    r.svc.pass(now_ms()).await;
    let rec = r.svc.get(id).unwrap();
    assert_eq!(rec.rounds.len(), 2);
    let t = rec.round().unwrap().text.clone().unwrap();
    assert!(t.contains("Use the other branch."), "{t}");
    assert!(t.ends_with(RESTART_TEXT), "{t}");
}

/// The cloud's Jobs, faked: every message put into a run's ConfigMap, and the default agent.
struct SteerCloud {
    agent: crate::devjob::Agent,
    fail: bool,
    steered: Mutex<Vec<(String, String, String)>>,
}

impl SteerCloud {
    fn new(agent: crate::devjob::Agent) -> SteerCloud {
        SteerCloud {
            agent,
            fail: false,
            steered: Mutex::new(Vec::new()),
        }
    }
}

impl CloudJobs for SteerCloud {
    fn cancel<'a>(
        &'a self,
        _job: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<CloudCancel, String>> + Send + 'a>> {
        Box::pin(async { Ok(CloudCancel::Stopping) })
    }

    fn steer<'a>(
        &'a self,
        run_id: &'a str,
        key: &'a str,
        body: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        lock(&self.steered).push((run_id.into(), key.into(), body.into()));
        let fail = self.fail;
        Box::pin(async move {
            if fail {
                Err("kc1 did not answer".into())
            } else {
                Ok(())
            }
        })
    }

    fn default_agent(&self) -> crate::devjob::Agent {
        self.agent
    }
}

/// A cloud run on `agent` (or the cloud's default), taken and named its Job `job`.
fn on_cloud_as(r: &Rig, s: &Scratch, slug: &str, job: &str, agent: Option<&str>) -> String {
    let part = CloudPart {
        agent: agent.map(str::to_string),
        ..CloudPart::default()
    };
    let id = r
        .svc
        .create_on(&free_in("o/n", slug), Some(part))
        .unwrap()
        .run_id;
    assert_eq!(r.svc.take_for_cloud(&s.notes()).unwrap().run_id, id);
    r.svc.note_job(&id, job, &format!("jesse/{slug}")).unwrap();
    id
}

fn cloud_rig(s: &Scratch, cloud: Arc<SteerCloud>) -> Rig {
    let mut st = settings(s);
    st.max_concurrent = 10;
    st.max_per_repo = 10;
    let r = rig_with(s, st);
    r.svc.attach_cloud(cloud);
    r
}

#[tokio::test]
async fn a_message_to_a_running_cloud_run_goes_into_its_config_map_as_one_key() {
    let s = Scratch::new();
    vault_fixture(&s);
    let cloud = Arc::new(SteerCloud::new(crate::devjob::Agent::Claude));
    let r = cloud_rig(&s, cloud.clone());
    let id = on_cloud_as(&r, &s, "c", "job-c", None);
    let out = r
        .svc
        .reply_full_and_pump(&id, "Stop at the parser.")
        .await
        .unwrap();
    let m = out.message.clone().unwrap();
    assert_eq!(m.channel, mailbox::CHANNEL_TOOL_CALL);
    assert!(out
        .delivery()
        .unwrap()
        .contains("after the agent's current tool call"));
    let steered = lock(&cloud.steered).clone();
    assert_eq!(steered.len(), 1);
    assert_eq!(steered[0].0, id);
    assert_eq!(steered[0].1, m.file_name());
    let body: mailbox::MailMessage = serde_json::from_str(&steered[0].2).unwrap();
    assert_eq!(body.text, "Stop at the parser.");
    // No Studio mailbox for a cloud run.
    assert!(!r
        .svc
        .settings
        .run_dir(&id)
        .join(mailbox::MAILBOX_DIR)
        .exists());
    // The pod's log says it was delivered: `delivered_at`, from the line's own time.
    let log = format!(
        "building\n{}\nmore\n",
        mailbox::delivery_line(&m.id, "2026-11-27T15:00:07.000Z")
    );
    let rec = r
        .svc
        .cloud_messages_delivered("job-c", &mailbox::parse_delivered_lines(&log))
        .unwrap();
    assert_eq!(
        rec.messages[0].delivered_at.as_deref(),
        Some("2026-11-27T15:00:07.000Z")
    );
    // A Codex run takes messages the same way.
    let codex = on_cloud_as(&r, &s, "x", "job-x", Some("codex"));
    let out = r
        .svc
        .reply_full_and_pump(&codex, "Also this.")
        .await
        .unwrap();
    assert_eq!(out.message.unwrap().channel, mailbox::CHANNEL_TOOL_CALL);
}

#[tokio::test]
async fn an_agent_without_added_context_gets_the_queued_for_next_round_answer() {
    let s = Scratch::new();
    vault_fixture(&s);
    let cloud = Arc::new(SteerCloud::new(crate::devjob::Agent::Gemini));
    let r = cloud_rig(&s, cloud.clone());
    let id = on_cloud_as(&r, &s, "g", "job-g", None);
    let out = r
        .svc
        .reply_full_and_pump(&id, "Change course.")
        .await
        .unwrap();
    let m = out.message.clone().unwrap();
    assert_eq!(m.channel, mailbox::CHANNEL_NEXT_ROUND);
    let said = out.delivery().unwrap();
    assert!(said.contains("queued for the next round only"), "{said}");
    assert!(!said.contains("current tool call"), "{said}");
    // Never put where no hook would read it.
    assert!(lock(&cloud.steered).is_empty());
    // The Job ends: the report path names it, and the next round carries it.
    r.svc.cloud_ended(&CloudEnd {
        job: "job-g".into(),
        status: RunStatus::Reported,
        report_path: Some("Inbox/x.md".into()),
        continued_as: None,
        error: None,
        leaks: Vec::new(),
    });
    let rec = r.svc.get(&id).unwrap();
    assert!(undelivered_note(&rec).unwrap().contains("Change course."));
    // What the Job pushed, on origin: the next round continues from it.
    remote_fixture(&s);
    sh(
        &s.base.join("remote/o/n.git"),
        &["branch", "jesse/g", "main"],
    );
    let next = r.svc.reply(&id, "Go on.").unwrap();
    let t = next.round().unwrap().text.clone().unwrap();
    assert!(
        t.starts_with(mailbox::HEADER) && t.contains("Change course."),
        "{t}"
    );
}

#[tokio::test]
async fn a_message_the_cloud_refuses_waits_for_the_next_round_and_says_so() {
    let s = Scratch::new();
    vault_fixture(&s);
    let cloud = Arc::new(SteerCloud {
        fail: true,
        ..SteerCloud::new(crate::devjob::Agent::Claude)
    });
    let r = cloud_rig(&s, cloud);
    let id = on_cloud_as(&r, &s, "f", "job-f", None);
    let out = r.svc.reply_full_and_pump(&id, "Hello.").await.unwrap();
    assert_eq!(
        out.message.clone().unwrap().channel,
        mailbox::CHANNEL_NEXT_ROUND
    );
    let said = out.delivery().unwrap();
    assert!(
        said.contains("kc1 did not answer") && said.contains("next round only"),
        "{said}"
    );
    assert_eq!(
        r.svc.get(&id).unwrap().messages[0].channel,
        mailbox::CHANNEL_NEXT_ROUND
    );
}

#[test]
fn a_record_with_no_messages_serializes_as_before() {
    let rec = RunRecord::default();
    let v = serde_json::to_value(&rec).unwrap();
    assert!(v.get("messages").is_none());
}

#[test]
fn the_agent_gets_the_mailbox_variable_and_a_settings_file_with_the_steer_hook() {
    let dir = Path::new("/r/runs/GD5");
    let env = runner::agent_env(dir, "GD5", |_| None);
    let mb: Vec<&(String, String)> = env
        .iter()
        .filter(|(k, _)| k == mailbox::MAILBOX_ENV)
        .collect();
    assert_eq!(mb.len(), 1);
    assert_eq!(mb[0].1, "/r/runs/GD5/mailbox");
    // Nothing else of the environment moved: the run's own three, then the mailbox.
    let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        [
            "JESSE_RUN_ID",
            "JESSE_RUN_REPORT",
            "CARGO_TARGET_DIR",
            mailbox::MAILBOX_ENV
        ]
    );
    // The attribution settings as they were, plus one PostToolUse hook on every tool.
    let hook = Path::new("/u/bin/jesse-hook");
    let doc = runner::agent_settings(true, Some(hook)).unwrap();
    let mut without = doc.clone();
    without.as_object_mut().unwrap().remove("hooks");
    assert_eq!(without, crate::attribution::settings_value());
    let post = &doc["hooks"]["PostToolUse"];
    assert_eq!(post.as_array().unwrap().len(), 1);
    assert_eq!(post[0]["matcher"], "*");
    assert_eq!(
        post[0]["hooks"][0]["command"],
        "/u/bin/jesse-hook steer --harness claude-code"
    );
    assert!(
        doc["hooks"].get("PreToolUse").is_none(),
        "nothing before a tool call"
    );
    // Without the hook binary, exactly the attribution settings; with neither, no file.
    assert_eq!(
        runner::agent_settings(true, None).unwrap(),
        crate::attribution::settings_value()
    );
    assert!(runner::agent_settings(false, None).is_none());
    // The argv names the settings file and is otherwise what it was.
    let rec = RunRecord {
        permission_mode: "bypassPermissions".into(),
        ..RunRecord::default()
    };
    let p = dir.join(runner::AGENT_SETTINGS);
    let args = runner::claude_args_with(&rec, true, "sid", Some(&p));
    assert_eq!(
        args,
        [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--session-id",
            "sid",
            "--permission-mode",
            "bypassPermissions",
            "--settings",
            "/r/runs/GD5/agent-settings.json"
        ]
    );
}

#[tokio::test]
async fn a_studio_runner_without_the_steer_hook_gets_the_next_round_answer() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    // `/bin/sh` as the runner: no `jesse-hook` beside it, so the agent would have no hook.
    let r = rig(&s);
    r.svc.start(free("nohook")).await.unwrap();
    let out = r
        .svc
        .reply_full_and_pump("s-nohook", "Change course.")
        .await
        .unwrap();
    let m = out.message.clone().unwrap();
    assert_eq!(m.channel, mailbox::CHANNEL_NEXT_ROUND);
    let said = out.delivery().unwrap();
    assert!(said.contains("queued for the next round only"), "{said}");
    assert!(!said.contains("current tool call"), "{said}");
    // Nothing in a mailbox no hook reads.
    let mb = r
        .svc
        .settings
        .run_dir("s-nohook")
        .join(mailbox::MAILBOX_DIR);
    assert!(mailbox::messages(&mb).is_empty());
}

#[tokio::test]
async fn a_round_that_cannot_start_leaves_its_carried_message_for_the_next_one() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = steer_rig(&s);
    let runner = r.svc.settings.runner.clone();
    r.svc.start(free("nostart")).await.unwrap();
    let id = "s-nostart";
    r.svc.reply_to(id, "Use the other branch.").unwrap();
    finish_round(&r, id, 0, Some("Done.\n"), "");
    r.svc.pass(now_ms()).await;
    // Round 2 carries the message, and its runner is gone: the round fails without an agent.
    std::fs::remove_file(&runner).unwrap();
    let out = r.svc.reply_full_and_pump(id, "Carry on.").await.unwrap();
    assert_eq!(out.record.rounds.len(), 2);
    let rec = r.svc.get(id).unwrap();
    assert!(rec.error.is_some(), "the launch failed: {rec:?}");
    let m = &rec.messages[0];
    assert_eq!(m.delivered_at, None, "no agent read round 2's text");
    assert_eq!(m.carried_to_round, None, "it waits again");
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get(id).unwrap().status, RunStatus::Failed);
    // The runner is back: round 3 carries it, and it is delivered when that round starts.
    place_runner(&s);
    let out = r.svc.reply_full_and_pump(id, "Again.").await.unwrap();
    let round = out.record.round().unwrap().clone();
    assert_eq!(round.n, 3);
    assert!(round.text.unwrap().contains("Use the other branch."));
    let m = &out.record.messages[0];
    assert_eq!(m.carried_to_round, Some(3));
    assert_eq!(m.delivered_at, round.started_at);
}

#[tokio::test]
async fn messages_keep_the_order_they_were_sent_whatever_the_clock_says() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = steer_rig(&s);
    r.svc.start(free("order")).await.unwrap();
    let id = "s-order";
    let first = r.svc.reply_to(id, "First.").unwrap().message.unwrap();
    // The first message's time, an hour ahead: a second sent now still sorts after it.
    let ahead = rfc(now_ms() + 3_600_000);
    let dir = r.svc.settings.run_dir(id);
    update_run(&dir, |x| x.messages[0].queued_at = ahead.clone()).unwrap();
    let mb = dir.join(mailbox::MAILBOX_DIR);
    std::fs::remove_file(mb.join(first.file_name())).unwrap();
    let moved = r.svc.get(id).unwrap().messages[0].clone();
    mailbox::write_message(&mb, &moved.file_name(), &moved.mail()).unwrap();
    let second = r.svc.reply_to(id, "Second.").unwrap().message.unwrap();
    assert!(
        ms_of(&second.queued_at).unwrap() > ms_of(&ahead).unwrap(),
        "{} after {ahead}",
        second.queued_at
    );
    let ctx = hook_fires(&r, id, "2026-11-27T15:00:00.000Z").unwrap();
    assert!(
        ctx.find("First.").unwrap() < ctx.find("Second.").unwrap(),
        "{ctx}"
    );
}
