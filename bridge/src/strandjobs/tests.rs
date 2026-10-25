use super::*;
use crate::devjob::{
    parse_report, render_report, render_report_for, tail_lines, ReportInput, SpecAnnotation,
    LABEL_DEVJOB, LABEL_SLUG, LABEL_STRAND, LABEL_STRAND_ID, REPORT_LOG_LINES,
};
use k8s_openapi::api::batch::v1::Job;

// ---- fixtures ------------------------------------------------------------------------

const NOTE: &str = "Trial-Strand";
const DATE: &str = "20261025";

/// A scratch notes directory: `Strands/`, `Projects/drafts/` (and its `archive/`), `Inbox/`
/// (and its `archive/`).
fn notes() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jesse-strandjobs-{}", random_hex()));
    for d in [
        "Strands",
        "Projects/drafts/archive",
        "Projects/Research",
        "Inbox/archive",
    ] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    dir
}

fn write(root: &Path, rel: &str, text: &str) {
    std::fs::write(root.join(rel), text).unwrap();
}

/// A prompt file whose front matter claims `note`/`id` and opts in with `repo` (when given).
fn prompt(note: &str, id: &str, repo: Option<&str>, extra: &str, body: &str) -> String {
    let repo = repo
        .map(|r| format!("devjob-repo: {r}\n"))
        .unwrap_or_default();
    format!("---\nstrand: {note}\nstrand-id: {id}\n{repo}{extra}---\n{body}")
}

/// A v2 note with `drafts` as its `## Drafts` lines (Later and Done follow).
fn note_with(drafts: &str) -> String {
    format!(
        "---\ngroup: personal\nstate: active\nupdated: 2026-10-24\n---\n\n# Trial Strand\n\n\
         **Now:** trying it.\n\n## Drafts\n\n{drafts}\n### Later\n\n\
         - [ ] **Z9** Later work. [[Projects/drafts/later-prompt]]\n\n### Done\n\n\
         - [x] 2026-10-01 **Z0** Old work. [[Projects/drafts/archive/old-prompt]]\n"
    )
}

fn template() -> StrandTemplate {
    StrandTemplate {
        note: NOTE.into(),
        agent: None,
        model: None,
        timeout_minutes: None,
    }
}

fn judge(root: &Path, note_src: &str) -> Vec<Result<Candidate, Skip>> {
    let strand = parse_strand(NOTE, note_src);
    candidates(root, &template(), &strand, Agent::Claude, DATE)
}

fn nothing_in_flight(_: &StrandRef) -> bool {
    false
}

fn reason_of(root: &Path, line: &str) -> String {
    match choose(judge(root, &note_with(line)), nothing_in_flight) {
        Pick::Nothing(r) => r,
        Pick::Run(c) => panic!("picked {:?} for {line}", c.step),
    }
}

/// An eligible A1 and its prompt.
fn good(root: &Path) -> &'static str {
    write(
        root,
        "Projects/drafts/a1-prompt.md",
        &prompt(
            NOTE,
            "A1",
            Some("jeremyandrews/k3s"),
            "",
            "Read README.md and make no changes.\n",
        ),
    );
    "- [ ] **A1** Read the README. [[Projects/drafts/a1-prompt]]\n"
}

// ---- the five checks, each failing on its own -----------------------------------------

#[test]
fn an_eligible_line_is_picked_with_its_step_and_spec() {
    let root = notes();
    let line = good(&root);
    let Pick::Run(c) = choose(judge(&root, &note_with(line)), nothing_in_flight) else {
        panic!("not picked");
    };
    assert_eq!(
        c.step,
        StrandRef {
            note: NOTE.into(),
            id: "A1".into(),
            prompt: "Projects/drafts/a1-prompt.md".into(),
        }
    );
    assert_eq!(c.spec.repo, "jeremyandrews/k3s");
    assert_eq!(c.spec.prompt, "Read README.md and make no changes.\n");
    assert_eq!(c.spec.agent, Agent::Claude);
    assert_eq!(c.spec.branch_slug, "trial-strand-a1-20261025");
    assert_eq!(
        c.spec.timeout_minutes,
        crate::devjob::DEFAULT_TIMEOUT_MINUTES
    );
    assert_eq!(c.spec.test_command, None);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn check_one_a_checked_line_or_one_with_no_id_is_skipped() {
    let root = notes();
    good(&root);
    assert_eq!(
        reason_of(
            &root,
            "- [x] **A1** Done already. [[Projects/drafts/a1-prompt]]\n"
        ),
        "A1: checked"
    );
    assert_eq!(
        reason_of(&root, "- [ ] No id here. [[Projects/drafts/a1-prompt]]\n"),
        "line 13: no id"
    );
    assert_eq!(
        reason_of(
            &root,
            "- [ ] **A 1** Spaced id. [[Projects/drafts/a1-prompt]]\n"
        ),
        "A 1: the id is not a Kubernetes label value"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn check_two_a_line_that_waits_is_skipped() {
    let root = notes();
    good(&root);
    assert_eq!(
        reason_of(
            &root,
            "- [ ] **A1** Read the README. [[Projects/drafts/a1-prompt]] (waits on: test)\n"
        ),
        "A1: waits on: test"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn check_three_the_link_must_be_a_live_draft() {
    let root = notes();
    good(&root);
    assert_eq!(reason_of(&root, "- [ ] **A1** No link.\n"), "A1: no link");
    assert_eq!(
        reason_of(&root, "- [ ] **A1** Dead. [[Projects/drafts/nope]]\n"),
        "A1: link [[Projects/drafts/nope]] does not resolve"
    );
    write(
        &root,
        "Projects/Research/a1-prompt.md",
        &prompt(NOTE, "A1", Some("a/b"), "", "x\n"),
    );
    assert_eq!(
        reason_of(
            &root,
            "- [ ] **A1** Elsewhere. [[Projects/Research/a1-prompt]]\n"
        ),
        "A1: link Projects/Research/a1-prompt.md is not under Projects/drafts/"
    );
    write(
        &root,
        "Projects/drafts/archive/a1-prompt.md",
        &prompt(NOTE, "A1", Some("a/b"), "", "x\n"),
    );
    assert_eq!(
        reason_of(
            &root,
            "- [ ] **A1** Old. [[todo-list/Projects/drafts/archive/a1-prompt]]\n"
        ),
        "A1: link Projects/drafts/archive/a1-prompt.md is archived"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn check_four_the_prompt_must_be_this_step() {
    let root = notes();
    let line = "- [ ] **A1** Read. [[Projects/drafts/p]]\n";
    write(
        &root,
        "Projects/drafts/p.md",
        &prompt("Other-Strand", "A1", Some("a/b"), "", "x\n"),
    );
    assert_eq!(
        reason_of(&root, line),
        "A1: prompt Projects/drafts/p.md belongs to strand Other-Strand"
    );
    write(
        &root,
        "Projects/drafts/p.md",
        &prompt(NOTE, "B2", Some("a/b"), "", "x\n"),
    );
    assert_eq!(
        reason_of(&root, line),
        "A1: prompt Projects/drafts/p.md is step B2, not A1"
    );
    write(
        &root,
        "Projects/drafts/p.md",
        "---\ndevjob-repo: a/b\n---\nx\n",
    );
    assert_eq!(
        reason_of(&root, line),
        "A1: prompt Projects/drafts/p.md has no strand"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn check_five_a_prompt_without_devjob_repo_is_never_run() {
    let root = notes();
    let line = "- [ ] **A1** Read. [[Projects/drafts/p]]\n";
    write(
        &root,
        "Projects/drafts/p.md",
        &prompt(NOTE, "A1", None, "", "For a person.\n"),
    );
    assert_eq!(
        reason_of(&root, line),
        "A1: prompt Projects/drafts/p.md has no devjob-repo"
    );
    write(
        &root,
        "Projects/drafts/p.md",
        &prompt(NOTE, "A1", Some("not a repo"), "", "x\n"),
    );
    assert_eq!(
        reason_of(&root, line),
        "A1: prompt Projects/drafts/p.md: devjob-repo \"not a repo\" is not owner/name"
    );
    let _ = std::fs::remove_dir_all(root);
}

// ---- the walk ----------------------------------------------------------------------------

#[test]
fn the_first_eligible_line_wins_and_later_running_checked_or_waiting_lines_never_do() {
    let root = notes();
    for id in ["R1", "C1", "W1", "B2", "B3", "Z9"] {
        write(
            &root,
            &format!("Projects/drafts/{}-prompt.md", id.to_lowercase()),
            &prompt(NOTE, id, Some("a/b"), "", &format!("Do {id}.\n")),
        );
    }
    let drafts = "\
- [ ] **R1** Running. Launched 2026-10-20. [[Projects/drafts/r1-prompt]]
- [x] **C1** Checked. [[Projects/drafts/c1-prompt]]
- [ ] **W1** Waiting. [[Projects/drafts/w1-prompt]] (waits on: R1)
- [ ] **B2** The one. [[Projects/drafts/b2-prompt]]
- [ ] **B3** Also eligible. [[Projects/drafts/b3-prompt]]
";
    let Pick::Run(c) = choose(judge(&root, &note_with(drafts)), nothing_in_flight) else {
        panic!("not picked");
    };
    assert_eq!(c.step.id, "B2");
    // With B2 and B3 gone, nothing is left: the Later Z9 is never considered, and the reason
    // names the first Queue line (the running R1 is not one).
    let rest = "\
- [ ] **R1** Running. Launched 2026-10-20. [[Projects/drafts/r1-prompt]]
- [x] **C1** Checked. [[Projects/drafts/c1-prompt]]
- [ ] **W1** Waiting. [[Projects/drafts/w1-prompt]] (waits on: R1)
";
    assert_eq!(
        choose(judge(&root, &note_with(rest)), nothing_in_flight),
        Pick::Nothing("C1: checked".into())
    );
    assert_eq!(
        choose(judge(&root, &note_with("")), nothing_in_flight),
        Pick::Nothing("no queue lines".into())
    );
    let _ = std::fs::remove_dir_all(root);
}

// ---- in flight -------------------------------------------------------------------------

fn job_with(labels: Value, suspended: bool) -> Job {
    serde_json::from_value(json!({
        "metadata": { "name": "agent-x-abcde", "labels": labels },
        "spec": { "suspend": suspended, "template": { "spec": { "containers": [] } } },
    }))
    .unwrap()
}

#[test]
fn a_labelled_job_in_any_phase_holds_its_step_in_flight() {
    let root = notes();
    let line = good(&root);
    let b2 = "- [ ] **B2** Next. [[Projects/drafts/b2-prompt]]\n";
    write(
        &root,
        "Projects/drafts/b2-prompt.md",
        &prompt(NOTE, "B2", Some("a/b"), "", "B2.\n"),
    );
    let labels = json!({
        LABEL_DEVJOB: "true", LABEL_SLUG: "s", LABEL_STRAND: NOTE, LABEL_STRAND_ID: "A1",
    });
    for suspended in [false, true] {
        let jobs = [job_with(labels.clone(), suspended)];
        let fly = |s: &StrandRef| jobs.iter().any(|j| carries_strand_step(j, &s.note, &s.id));
        // A1 alone: nothing, in flight.
        assert_eq!(
            choose(judge(&root, &note_with(line)), fly),
            Pick::Nothing("A1: in flight".into())
        );
        // A1 then B2: the walk goes on to B2.
        let Pick::Run(c) = choose(judge(&root, &note_with(&format!("{line}{b2}"))), fly) else {
            panic!("B2 not picked");
        };
        assert_eq!(c.step.id, "B2");
    }
    // Another step's labels, or another strand's, do not hold A1.
    for other in [
        json!({ LABEL_STRAND: NOTE, LABEL_STRAND_ID: "B2" }),
        json!({ LABEL_STRAND: "Other", LABEL_STRAND_ID: "A1" }),
        json!({ LABEL_DEVJOB: "true" }),
    ] {
        let jobs = [job_with(other, false)];
        let fly = |s: &StrandRef| jobs.iter().any(|j| carries_strand_step(j, &s.note, &s.id));
        assert!(matches!(
            choose(judge(&root, &note_with(line)), fly),
            Pick::Run(_)
        ));
    }
    // A finished Job still holds it: any phase.
    let finished: Job = serde_json::from_value(json!({
        "metadata": { "name": "agent-x", "labels": labels },
        "status": { "conditions": [{ "type": "Complete", "status": "True" }] },
    }))
    .unwrap();
    assert!(carries_strand_step(&finished, NOTE, "A1"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_unarchived_inbox_report_holds_its_step_and_an_archived_one_does_not() {
    let root = notes();
    let line = good(&root);
    let fly = |s: &StrandRef| inbox_reports_step(&root, &s.note, &s.id);
    let report = format!(
        "---\ndevjob: agent-x\nrepo: a/b\noutcome: no changes\nstrand: {NOTE}\nstrand-id: A1\n\
         prompt: Projects/drafts/a1-prompt.md\n---\n\n# Dev job\n"
    );
    write(&root, "Inbox/archive/2026-10-24-dev-job-x.md", &report);
    assert!(matches!(
        choose(judge(&root, &note_with(line)), fly),
        Pick::Run(_)
    ));
    // A report for another step, or a file not named as a dev job report, does not hold it.
    write(
        &root,
        "Inbox/2026-10-24-dev-job-y.md",
        &report.replace("strand-id: A1", "strand-id: B2"),
    );
    write(&root, "Inbox/2026-10-24-notes.md", &report);
    assert!(matches!(
        choose(judge(&root, &note_with(line)), fly),
        Pick::Run(_)
    ));
    write(&root, "Inbox/2026-10-24-dev-job-x.md", &report);
    assert_eq!(
        choose(judge(&root, &note_with(line)), fly),
        Pick::Nothing("A1: in flight".into())
    );
    let _ = std::fs::remove_dir_all(root);
}

// ---- overrides -------------------------------------------------------------------------

#[test]
fn front_matter_overrides_the_entry_defaults() {
    let root = notes();
    let line = "- [ ] **A1** Read. [[Projects/drafts/p]]\n";
    let t = StrandTemplate {
        note: NOTE.into(),
        agent: Some(Agent::Gemini),
        model: Some("entry-model".into()),
        timeout_minutes: Some(45),
    };
    let strand = |src: &str| parse_strand(NOTE, src);
    // The entry's defaults, with none overridden.
    write(
        &root,
        "Projects/drafts/p.md",
        &prompt(NOTE, "A1", Some("a/b"), "", "x\n"),
    );
    let c = eligibility(
        &root,
        &t,
        &strand(&note_with(line)).items[0],
        Agent::Claude,
        DATE,
    )
    .unwrap();
    assert_eq!(c.spec.agent, Agent::Gemini);
    assert_eq!(c.spec.model.as_deref(), Some("entry-model"));
    assert_eq!(c.spec.timeout_minutes, 45);
    // Every override.
    write(
        &root,
        "Projects/drafts/p.md",
        &prompt(
            NOTE,
            "A1",
            Some("\"a/b\""),
            "devjob-agent: codex\ndevjob-model: gpt-5-codex\ndevjob-test: cargo test\ndevjob-timeout: 30\n",
            "x\n",
        ),
    );
    let c = eligibility(
        &root,
        &t,
        &strand(&note_with(line)).items[0],
        Agent::Claude,
        DATE,
    )
    .unwrap();
    assert_eq!(c.spec.repo, "a/b");
    assert_eq!(c.spec.agent, Agent::Codex);
    assert_eq!(c.spec.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(c.spec.test_command.as_deref(), Some("cargo test"));
    assert_eq!(c.spec.timeout_minutes, 30);
    // With no entry agent, the bridge's default.
    write(
        &root,
        "Projects/drafts/p.md",
        &prompt(NOTE, "A1", Some("a/b"), "", "x\n"),
    );
    let c = eligibility(
        &root,
        &template(),
        &strand(&note_with(line)).items[0],
        Agent::Codex,
        DATE,
    )
    .unwrap();
    assert_eq!(c.spec.agent, Agent::Codex);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_invalid_override_fails_that_line_with_the_field_named() {
    let root = notes();
    let line = "- [ ] **A1** Read. [[Projects/drafts/p]]\n";
    for (extra, field) in [
        ("devjob-agent: aider\n", "devjob-agent"),
        ("devjob-model: a model with spaces\n", "devjob-model"),
        (
            &*format!(
                "devjob-test: {}\n",
                "x".repeat(crate::devjob::MAX_TEST_COMMAND_BYTES + 1)
            ),
            "devjob-test",
        ),
        ("devjob-timeout: 0\n", "devjob-timeout"),
        ("devjob-timeout: soon\n", "devjob-timeout"),
    ] {
        write(
            &root,
            "Projects/drafts/p.md",
            &prompt(NOTE, "A1", Some("a/b"), extra, "x\n"),
        );
        let r = reason_of(&root, line);
        assert!(
            r.starts_with(&format!("A1: prompt Projects/drafts/p.md: {field}: ")),
            "{extra}: {r}"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

// ---- the prompt body -------------------------------------------------------------------

#[test]
fn the_body_loses_its_front_matter_and_archive_footer_and_nothing_else() {
    let src = "---\nstrand: X\nstrand-id: A1\n---\n# Task\n\nDo it.\n\n---\n\n- [x] Launched 2026-10-24\n- [ ] Reported\n";
    assert_eq!(prompt_body(src), "# Task\n\nDo it.\n");
    // A rule inside the body stays, with the footer after it still removed.
    let src = "---\nstrand: X\n---\nPart one.\n\n***\n\nPart two.\n\n---\n- [ ] archived\n";
    assert_eq!(prompt_body(src), "Part one.\n\n***\n\nPart two.\n");
    // A last rule followed by prose is not a footer.
    let src = "---\nk: v\n---\nOne.\n\n---\n\nTwo.\n- [ ] a list in the task\n";
    assert_eq!(
        prompt_body(src),
        "One.\n\n---\n\nTwo.\n- [ ] a list in the task\n"
    );
    // Checkboxes with no rule before them are the task's own.
    let src = "---\nk: v\n---\nSteps:\n- [ ] one\n- [ ] two\n";
    assert_eq!(prompt_body(src), "Steps:\n- [ ] one\n- [ ] two\n");
    // A trailing rule with nothing after it is not a footer either.
    assert_eq!(prompt_body("Body.\n\n---\n"), "Body.\n\n---\n");
    // No front matter: the whole file.
    assert_eq!(prompt_body("Just text.\n"), "Just text.\n");
}

// ---- the branch slug ---------------------------------------------------------------------

#[test]
fn the_branch_slug_fits_sixty_by_cutting_the_note_from_the_left() {
    assert_eq!(
        branch_slug("Devjob-Live", "L1", DATE),
        "devjob-live-l1-20261025"
    );
    assert_eq!(
        branch_slug("Jesse_App v2", "B.3", DATE),
        "jesse-app-v2-b-3-20261025"
    );
    let long = format!("{}-Tail", "Head".repeat(20));
    let s = branch_slug(&long, "A1", DATE);
    assert_eq!(s.len(), MAX_SLUG_CHARS);
    assert!(s.ends_with("headhead-tail-a1-20261025"), "{s}");
    assert!(crate::devjob::valid_slug(&s), "{s}");
    // Exactly at the limit, nothing is cut.
    let exact = "n".repeat(MAX_SLUG_CHARS - "-a1-20261025".len());
    assert_eq!(branch_slug(&exact, "A1", DATE).len(), MAX_SLUG_CHARS);
    assert!(branch_slug(&exact, "A1", DATE).starts_with(&exact));
    // One over: one character goes, from the left.
    let over = format!("m{exact}");
    assert_eq!(
        branch_slug(&over, "A1", DATE),
        branch_slug(&exact, "A1", DATE)
    );
    // A cut that lands on a hyphen does not start the slug with one.
    let hy = format!("x-{exact}");
    let s = branch_slug(&hy, "A1", DATE);
    assert!(crate::devjob::valid_slug(&s), "{s}");
}

// ---- the note ---------------------------------------------------------------------------

#[test]
fn a_missing_or_done_note_fails() {
    let root = notes();
    let e = load_strand(&root, NOTE).unwrap_err();
    assert!(e.contains("Strands/Trial-Strand.md is missing"), "{e}");
    write(
        &root,
        "Strands/Trial-Strand.md",
        &note_with("").replace("state: active", "state: done"),
    );
    let e = load_strand(&root, NOTE).unwrap_err();
    assert!(e.contains("is done"), "{e}");
    write(&root, "Strands/Trial-Strand.md", &note_with(""));
    assert!(load_strand(&root, NOTE).is_ok());
    let _ = std::fs::remove_dir_all(root);
}

// ---- the entry ---------------------------------------------------------------------------

fn entries(text: &str) -> Schedule {
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
fn a_strand_entry_parses_with_its_defaults() {
    let s = entries(
        "[[schedule]]\nid = \"q\"\nat = \"02:20\"\nkind = \"devjob\"\nstrand = \"Jesse-App\"\n\
         agent = \"codex\"\nmodel = \"gpt-5-codex\"\ntimeout = 60\nnotify = false\n",
    );
    assert!(s.invalid.is_empty(), "{:?}", s.invalid);
    let j = s.get("q").unwrap();
    assert!(j.devjob.is_none());
    assert!(j.is_devjob());
    assert_eq!(
        j.devjob_strand,
        Some(StrandTemplate {
            note: "Jesse-App".into(),
            agent: Some(Agent::Codex),
            model: Some("gpt-5-codex".into()),
            timeout_minutes: Some(60),
        })
    );
    // The dev agent's model, not a registry id.
    assert_eq!(j.model, None);
    let bare =
        entries("[[schedule]]\nid = \"q\"\nat = \"02:20\"\nkind = \"devjob\"\nstrand = \"X\"\n");
    assert!(bare.invalid.is_empty(), "{:?}", bare.invalid);
    assert_eq!(
        bare.get("q").unwrap().devjob_strand.as_ref().unwrap().agent,
        None
    );
}

#[test]
fn every_key_a_strand_entry_refuses_is_named() {
    let base = "[[schedule]]\nid = \"q\"\nat = \"02:20\"\nkind = \"devjob\"\nstrand = \"X\"\n";
    for (line, key) in [
        ("repo = \"a/b\"", "repo"),
        ("prompt_file = \"p.md\"", "prompt_file"),
        ("branch_slug = \"s\"", "branch_slug"),
        ("test_command = \"t\"", "test_command"),
        ("prompt = \"p\"", "prompt"),
        ("mode = \"tell\"", "mode"),
        ("builtin = \"speech-model-update\"", "builtin"),
        ("timeout_secs = 60", "timeout_secs"),
        ("expect_output = [\"Inbox/x.md\"]", "expect_output"),
    ] {
        let s = entries(&format!("{base}{line}\n"));
        assert_eq!(s.invalid.len(), 1, "{line}");
        assert!(
            s.invalid[0]
                .reason
                .contains(&format!("`{key}` is set on a strand entry")),
            "{line}: {}",
            s.invalid[0].reason
        );
    }
    for (line, why) in [
        ("agent = \"aider\"", "agent"),
        ("timeout = 0", "timeout"),
        ("model = \"a b\"", "model"),
    ] {
        let s = entries(&format!("{base}{line}\n"));
        assert_eq!(s.invalid.len(), 1, "{line}");
        assert!(
            s.invalid[0].reason.contains(why),
            "{line}: {}",
            s.invalid[0].reason
        );
    }
    for bad in ["", "../Today", "a/b", ".hidden", "Has Space", "-dash"] {
        let s = entries(&format!(
            "[[schedule]]\nid = \"q\"\nat = \"02:20\"\nkind = \"devjob\"\nstrand = \"{bad}\"\n"
        ));
        assert_eq!(s.invalid.len(), 1, "{bad}");
        assert!(
            s.invalid[0].reason.contains("`strand`"),
            "{bad}: {}",
            s.invalid[0].reason
        );
    }
}

#[test]
fn strand_is_refused_on_the_fixed_form_and_on_a_turn() {
    let s = entries(
        "[[schedule]]\nid = \"x\"\nat = \"02:00\"\nkind = \"devjob\"\nrepo = \"a/b\"\n\
         agent = \"claude\"\nbranch_slug = \"s\"\nprompt_file = \"p.md\"\n",
    );
    assert!(s.invalid.is_empty(), "{:?}", s.invalid);
    let turn =
        entries("[[schedule]]\nid = \"x\"\nat = \"02:00\"\nprompt = \"p\"\nstrand = \"X\"\n");
    assert!(
        turn.invalid[0]
            .reason
            .contains("`strand` is set on a turn job"),
        "{:?}",
        turn.invalid
    );
    // The fixed form with `strand` reads as a strand entry, which refuses the fixed keys by
    // name: one form or the other, never both.
    let both = entries(
        "[[schedule]]\nid = \"x\"\nat = \"02:00\"\nkind = \"devjob\"\nstrand = \"X\"\n\
         repo = \"a/b\"\nagent = \"claude\"\nbranch_slug = \"s\"\nprompt_file = \"p.md\"\n",
    );
    assert_eq!(both.invalid.len(), 1);
    assert!(both.invalid[0]
        .reason
        .contains("`repo` is set on a strand entry"));
}

// ---- the report ------------------------------------------------------------------------

const GOOD_LOG: &str = include_str!("../../tests/fixtures/devjob/log-ok.txt");
const GOLDEN: &str = include_str!("../../tests/fixtures/devjob/report-fixed.golden.md");

fn local(ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms as i64)
        .single()
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string()
}

fn input<'a>(
    spec: &'a SpecAnnotation,
    report: &'a crate::devjob::DevJobReport,
    tail: &'a [String],
) -> ReportInput<'a> {
    ReportInput {
        job_name: "agent-read-readme-abcde",
        slug: "read-readme",
        spec,
        prompt: Some("Read README.md and make no changes."),
        report,
        log_tail: tail,
        queued_ms: 1_761_300_000_000,
        started_ms: Some(1_761_300_005_000),
        finished_ms: Some(1_761_300_219_000),
    }
}

fn spec_annotation() -> SpecAnnotation {
    SpecAnnotation::from(
        &DevJobSpec::validate(
            DevJobArgs {
                repo: Some("jeremyandrews/k3s".into()),
                prompt: Some("Read README.md and make no changes.".into()),
                agent: Some("claude".into()),
                branch_slug: Some("read-readme".into()),
                ..DevJobArgs::default()
            },
            Agent::Claude,
        )
        .unwrap(),
    )
}

/// A REPORT FROM THE TOOL OR A FIXED ENTRY IS BYTE FOR BYTE TODAY'S. The golden file was
/// rendered by `render_report` before strand entries existed; only the three local timestamps
/// are placeholders, because they render in the machine's zone.
#[test]
fn a_report_without_a_strand_is_byte_for_byte_unchanged() {
    let report = parse_report(GOOD_LOG).unwrap();
    let tail = tail_lines(GOOD_LOG, REPORT_LOG_LINES);
    let spec = spec_annotation();
    let want = GOLDEN
        .replace("{QUEUED}", &local(1_761_300_000_000))
        .replace("{STARTED}", &local(1_761_300_005_000))
        .replace("{FINISHED}", &local(1_761_300_219_000));
    assert_eq!(render_report(&input(&spec, &report, &tail)), want);
    assert_eq!(render_report_for(&input(&spec, &report, &tail), None), want);
}

#[test]
fn a_report_with_a_strand_adds_three_keys_and_one_row() {
    let report = parse_report(GOOD_LOG).unwrap();
    let tail = tail_lines(GOOD_LOG, REPORT_LOG_LINES);
    let spec = spec_annotation();
    let step = StrandRef {
        note: "Devjob-Live".into(),
        id: "L1".into(),
        prompt: "Projects/drafts/devjob-live-prompt.md".into(),
    };
    let plain = render_report(&input(&spec, &report, &tail));
    let text = render_report_for(&input(&spec, &report, &tail), Some(&step));
    assert!(text.starts_with(
        "---\ndevjob: agent-read-readme-abcde\nrepo: jeremyandrews/k3s\noutcome: pull request opened\n\
         strand: Devjob-Live\nstrand-id: L1\nprompt: Projects/drafts/devjob-live-prompt.md\n---\n\n"
    ));
    assert!(text.contains("| --- | --- |\n| Strand | Devjob-Live L1 |\n| Repository |"));
    // Nothing else differs: drop the three keys and the row and it is the plain report.
    let back = text
        .replace(
            "strand: Devjob-Live\nstrand-id: L1\nprompt: Projects/drafts/devjob-live-prompt.md\n",
            "",
        )
        .replace("| Strand | Devjob-Live L1 |\n", "");
    assert_eq!(back, plain);
    // And the in-flight guard reads its own report back.
    let root = notes();
    write(&root, "Inbox/2026-10-25-dev-job-read-readme.md", &text);
    assert!(inbox_reports_step(&root, "Devjob-Live", "L1"));
    assert!(!inbox_reports_step(&root, "Devjob-Live", "L2"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_job_carries_the_step_back_after_a_restart() {
    let job: Job = serde_json::from_value(json!({
        "metadata": {
            "name": "agent-x",
            "labels": { LABEL_DEVJOB: "true", LABEL_STRAND: "Devjob-Live", LABEL_STRAND_ID: "L1" },
            "annotations": { crate::devjob::ANN_PROMPT: "Projects/drafts/p.md" },
        },
    }))
    .unwrap();
    assert_eq!(
        crate::devjob::strand_of(&job),
        Some(StrandRef {
            note: "Devjob-Live".into(),
            id: "L1".into(),
            prompt: "Projects/drafts/p.md".into(),
        })
    );
    let plain = job_with(json!({ LABEL_DEVJOB: "true" }), false);
    assert_eq!(crate::devjob::strand_of(&plain), None);
}
