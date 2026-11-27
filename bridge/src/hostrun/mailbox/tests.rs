use super::*;

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("jesse-mailbox-{}", crate::random_hex()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn msg(id: &str, text: &str, ms: u64) -> (String, MailMessage) {
    (
        file_name(ms, id),
        MailMessage {
            id: id.into(),
            text: text.into(),
            queued_at: crate::hostrun::rfc(ms),
        },
    )
}

/// The hook as the binary runs it, its stdout captured: what it printed, if anything.
fn hook(p: &HookPlaces, at: &str) -> Option<String> {
    let mut out = Vec::new();
    let got = run_hook(p, at, &mut out);
    let text = String::from_utf8(out).unwrap();
    assert_eq!(
        got.is_empty(),
        text.is_empty(),
        "printed exactly when it delivered"
    );
    (!got.is_empty()).then(|| text.trim_end().to_string())
}

/// A stdout that is gone: the CLI that ran the hook died, or closed the pipe.
struct Broken;

impl std::io::Write for Broken {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn put(dir: &Path, m: &(String, MailMessage)) {
    write_message(dir, &m.0, &m.1).unwrap();
}

/// The `additionalContext` a hook output carries.
fn context(out: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn file_names_sort_in_the_order_sent_and_carry_their_id() {
    let a = file_name(1_795_000_000_001, "maaa");
    let b = file_name(1_795_000_000_010, "m000");
    assert!(a < b, "{a} {b}");
    assert_eq!(id_of(&a), Some("maaa"));
    assert_eq!(id_of("notes.txt"), None);
    assert_eq!(id_of("x-m1.delivered"), None);
    assert!(a
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-._".contains(c)));
    let id = new_id();
    assert!(valid_id(&id), "{id}");
}

#[test]
fn the_hook_delivers_each_message_once_in_order_across_a_restart() {
    let dir = scratch();
    let mailbox = dir.join("mailbox");
    put(&mailbox, &msg("mbbb", "second", 1_795_000_000_200));
    put(&mailbox, &msg("maaa", "first", 1_795_000_000_100));
    let places = HookPlaces {
        mailbox: Some(mailbox.clone()),
        ..HookPlaces::default()
    };
    let out = hook(&places, "2026-11-27T15:00:00.000Z").expect("two waiting");
    let ctx = context(&out);
    let (first, second) = (ctx.find("first").unwrap(), ctx.find("second").unwrap());
    assert!(first < second, "in the order sent: {ctx}");
    assert!(ctx.starts_with(HEADER), "{ctx}");
    assert!(ctx.ends_with(FOOTER), "{ctx}");
    assert_eq!(ctx.matches(HEADER).count(), 2);
    // A second tool call, or a hook in a new process after a restart: the markers on disk say
    // both were delivered, so nothing is handed over again.
    let again = HookPlaces::from_env(|k| (k == MAILBOX_ENV).then(|| mailbox.display().to_string()));
    assert_eq!(hook(&again, "2026-11-27T15:00:05.000Z"), None);
    // A third message later is delivered alone.
    put(&mailbox, &msg("mccc", "third", 1_795_000_000_300));
    let ctx = context(&hook(&again, "2026-11-27T15:00:09.000Z").unwrap());
    assert!(ctx.contains("third") && !ctx.contains("first"), "{ctx}");
    let marks = delivered_marks(&mailbox);
    assert_eq!(marks["maaa"], "2026-11-27T15:00:00.000Z");
    assert_eq!(marks["mccc"], "2026-11-27T15:00:09.000Z");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_hooks_at_once_never_both_deliver_a_message() {
    let dir = scratch();
    put(&dir, &msg("maaa", "only once", 1_795_000_000_100));
    assert!(claim(&dir, "maaa"));
    assert!(!claim(&dir, "maaa"));
    assert!(claim_waiting(&dir, &dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_mailbox_or_none_named_is_a_no_op() {
    let dir = scratch();
    assert_eq!(hook(&HookPlaces::default(), "t"), None);
    let gone = HookPlaces {
        mailbox: Some(dir.join("not-mounted")),
        marks: Some(dir.join("marks")),
        log: None,
    };
    assert_eq!(hook(&gone, "t"), None);
    assert!(!dir.join("marks").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_read_only_mailbox_marks_elsewhere_and_prints_one_line_per_delivery() {
    let dir = scratch();
    let (mailbox, marks, log) = (dir.join("steer"), dir.join("marks"), dir.join("pod.log"));
    put(&mailbox, &msg("maaa", "one", 1_795_000_000_100));
    put(&mailbox, &msg("mbbb", "two", 1_795_000_000_200));
    std::fs::write(&log, "agent output\n").unwrap();
    let p = HookPlaces {
        mailbox: Some(mailbox.clone()),
        marks: Some(marks.clone()),
        log: Some(log.clone()),
    };
    assert!(hook(&p, "2026-11-27T15:00:00.000Z").is_some());
    assert!(hook(&p, "2026-11-27T15:00:01.000Z").is_none());
    // Nothing was written into the mailbox itself.
    assert_eq!(std::fs::read_dir(&mailbox).unwrap().count(), 2);
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        text,
        "agent output\nJESSE_MESSAGE_DELIVERED maaa 2026-11-27T15:00:00.000Z\n\
         JESSE_MESSAGE_DELIVERED mbbb 2026-11-27T15:00:00.000Z\n"
    );
    let parsed = parse_delivered_lines(&text);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed["maaa"].as_deref(), Some("2026-11-27T15:00:00.000Z"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_log_parser_takes_only_whole_delivery_lines() {
    let log = "JESSE_MESSAGE_DELIVERED m1\n\
               echo JESSE_MESSAGE_DELIVERED m2\n\
               JESSE_MESSAGE_DELIVERED ../etc 2026-11-27T15:00:00Z\n\
               JESSE_MESSAGE_DELIVEREDm3\n\
               JESSE_MESSAGE_DELIVERED m4 not-a-time\n";
    let got = parse_delivered_lines(log);
    assert_eq!(got.keys().cloned().collect::<Vec<_>>(), vec!["m1", "m4"]);
    assert_eq!(got["m1"], None);
    assert_eq!(got["m4"], None);
}

#[test]
fn the_steer_config_map_name_is_a_kubernetes_name() {
    assert_eq!(config_map_name("DJ33"), "steer-dj33");
    assert_eq!(config_map_name("gd5-b"), "steer-gd5-b");
}

#[test]
fn a_message_is_marked_delivered_only_once_the_hook_output_is_written() {
    let dir = scratch();
    put(&dir, &msg("maaa", "the correction", 1_795_000_000_100));
    let p = HookPlaces {
        mailbox: Some(dir.clone()),
        ..HookPlaces::default()
    };
    // The write fails: nothing is marked, the claim is given up, and the next hook delivers it.
    assert!(run_hook(&p, "2026-11-27T15:00:00.000Z", &mut Broken).is_empty());
    assert!(delivered_marks(&dir).is_empty(), "never claimed delivered");
    let ctx = context(&hook(&p, "2026-11-27T15:00:05.000Z").unwrap());
    assert!(ctx.contains("the correction"));
    assert_eq!(delivered_marks(&dir)["maaa"], "2026-11-27T15:00:05.000Z");
    assert!(!dir.join("maaa.claim").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_hook_killed_after_its_claim_leaves_the_message_waiting_for_the_bridge() {
    let dir = scratch();
    put(&dir, &msg("maaa", "the correction", 1_795_000_000_100));
    // A hook claimed it and died before printing: no marker, so the bridge still counts it as
    // waiting (and carries it into the next round), and no other hook hands it over twice.
    assert!(claim(&dir, "maaa"));
    assert!(delivered_marks(&dir).is_empty());
    let p = HookPlaces {
        mailbox: Some(dir.clone()),
        ..HookPlaces::default()
    };
    assert_eq!(hook(&p, "t"), None);
    let _ = std::fs::remove_dir_all(&dir);
}
