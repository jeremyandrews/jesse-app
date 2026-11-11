use super::*;
use crate::hostrun::RunStatus;
use crate::strands::parse_strand;

fn fixture(rel: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(rel),
    )
    .unwrap()
}

/// One line of a fixture note, as the parser hands it on.
fn item(rel: &str, id: &str) -> StrandItem {
    parse_strand("Fixture", &fixture(rel))
        .items
        .into_iter()
        .find(|i| i.id == id)
        .unwrap_or_else(|| panic!("{rel} has no {id}"))
}

/// A line of `work/Work-Lines.md`, a fixture note of the shapes Drafts lines take.
fn line(id: &str) -> StrandItem {
    item("work/Work-Lines.md", id)
}

// ---- TITLES ----------------------------------------------------------------------------

#[test]
fn a_title_is_the_text_before_the_first_colon() {
    assert_eq!(
        line_title(&item("strands/vault/Strands/Clean-Strand.md", "B7").raw),
        "Queue worker wedge"
    );
    assert_eq!(line_title(&line("W5").raw), "Beta on k3s through Flux");
    // Literally the rule: the line says `C:`, so its title is `C`.
    assert_eq!(line_title(&line("W6").raw), "C");
}

#[test]
fn a_title_with_no_colon_is_cut_at_57_characters_on_a_word() {
    // From the fixture notes: short and colon free, the whole text without its full stop.
    assert_eq!(
        line_title(&item("strands/vault/Strands/Alpha-Strand.md", "N1").raw),
        "Rack the switch"
    );
    assert_eq!(
        line_title(&item("strands/vault/Strands/Tidy-Strand.md", "T4").raw),
        "Write up what the two runs found"
    );
    // Long and colon free: cut on a word, with the draft link it ends on gone.
    let t = line_title(&line("W3").raw);
    assert_eq!(
        t,
        "Clean rerun on the test host under Podman, fresh\u{2026}"
    );
    assert!(t.chars().count() <= TITLE_MAX + 1);
    // A draft link the line ends on is not part of its name.
    assert_eq!(
        line_title(&line("W8").raw),
        "Light colour control task list"
    );
    assert_eq!(
        line_title(&item("strands/vault/Strands/Clean-Strand.md", "B8").raw),
        "Queue worker wedge campaign. Launched 2026-09-23"
    );
}

#[test]
fn a_title_drops_waits_on_and_backticks() {
    // `(waits on: …)` holds a `": "`; it is gone before the colon rule looks.
    assert_eq!(
        line_title(&item("strands/vault/Strands/Clean-Strand.md", "A2").raw),
        "Extraction to its own repository"
    );
    // An aliased draft link and a waits on clause, both at the end.
    assert_eq!(
        line_title(&item("strands/vault/Strands/Tidy-Strand.md", "T3").raw),
        "Second run on the fixed build"
    );
    assert_eq!(
        line_title(&item("strands/vault/Strands/Clean-Strand.md", "A1d").raw),
        "Guest budget probe on the fixed kernel"
    );
    assert_eq!(
        line_title(&line("W4").raw),
        "Declare item_background = true in the plugin manifest\u{2026}"
    );
    // Emphasis and a Markdown link, then the colon rule.
    assert_eq!(
        line_title(&line("W7").raw),
        "Draft the release notes from the template"
    );
}

#[test]
fn a_line_that_starts_with_a_link_keeps_its_last_path_segment_or_alias() {
    // No alias: the link's last path segment, then the cut (no space to cut at inside it).
    assert_eq!(
        line_title(&line("W1").raw),
        "2026-11-10-1410-Work-Tab-Short-Project-View-Prompt The\u{2026}"
    );
    // With an alias, the alias.
    assert_eq!(
        line_title(&line("W2").raw),
        "Pool light prompt Colour control for the pool lights, one\u{2026}"
    );
    // A line that is only a link keeps it.
    assert_eq!(
        line_title("**X2** [[Projects/Notes/Pool-Lights]]"),
        "Pool-Lights"
    );
}

#[test]
fn markdown_becomes_plain_words() {
    assert_eq!(
        plain_text("**Bold** and *it* and _it_ and __b__ in `code`"),
        "Bold and it and it and b in code"
    );
    assert_eq!(
        plain_text("see [the docs](https://example.com/x) and [[a/b/C#Head]]"),
        "see the docs and C"
    );
    // An underscore inside a word is part of it.
    assert_eq!(
        plain_text("set item_background now"),
        "set item_background now"
    );
    // A line with no bold id is its own text.
    assert_eq!(
        line_title(&item("strands/vault/Strands/Messy-Strand.md", "").raw),
        "A line with no bold id at all"
    );
}

#[test]
fn a_cut_never_splits_a_word_when_a_space_is_there_to_cut_at() {
    let s = "word ".repeat(20);
    let t = title_of(&s);
    assert!(t.ends_with("word\u{2026}"), "{t}");
    assert!(t.chars().count() <= TITLE_MAX + 1);
    // Exactly 57 characters is not cut.
    let exact = "a".repeat(57);
    assert_eq!(title_of(&exact), exact);
}

#[test]
fn a_prompt_heading_loses_its_leading_id() {
    let draft = "---\nstrand: Goose\nstrand-id: GO256\n---\n# GO256 Goose ablation (A2): measure what each change since 0.18.1 bought\n\nBody.\n";
    assert_eq!(h1_title(draft).as_deref(), Some("Goose ablation (A2)"));
    assert_eq!(
        h1_title("# [HO2] The Work view\n").as_deref(),
        Some("The Work view")
    );
    assert_eq!(
        h1_title("# K04: model proxy\n").as_deref(),
        Some("model proxy")
    );
    assert_eq!(
        h1_title("# A1c \u{2014} Clean rerun\n").as_deref(),
        Some("Clean rerun")
    );
    // A heading that does not start with an id is kept whole.
    assert_eq!(
        h1_title("# Tidewater styles\n").as_deref(),
        Some("Tidewater styles")
    );
    assert_eq!(h1_title("No heading.\n## PROMPT\n"), None);
    // Front matter is not a heading.
    assert_eq!(h1_title("---\ntitle: x\n---\nplain\n"), None);
}

#[test]
fn a_branch_title_is_words() {
    assert_eq!(
        branch_title("go256-ablation-a2-four-workloads-in-interleave"),
        "Go256 ablation a2 four workloads in interleave"
    );
    assert_eq!(branch_title(""), "");
}

#[test]
fn a_run_title_is_the_line_then_the_heading_then_the_branch_never_the_id() {
    let step = RunRecord {
        run_id: "GO256".into(),
        strand: Some("Goose".into()),
        item_id: Some("GO256".into()),
        branch: "go256-ablation".into(),
        ..RunRecord::default()
    };
    let line = |_: &str, _: &str| Some("Ablation A2".to_string());
    let none = |_: &str, _: &str| None;
    let h1 = || Some("Goose ablation (A2)".to_string());
    assert_eq!(run_title(&step, line, h1), "Ablation A2");
    assert_eq!(run_title(&step, none, h1), "Goose ablation (A2)");
    assert_eq!(run_title(&step, none, || None), "Go256 ablation");
    let bare = RunRecord {
        run_id: "s-x".into(),
        ..RunRecord::default()
    };
    let t = run_title(&bare, none, || None);
    assert_ne!(t, "s-x");
    assert_eq!(t, "Untitled run");
}

// ---- LINES -------------------------------------------------------------------------------

#[test]
fn the_last_line_is_the_last_non_blank_one_clipped() {
    let lines: Vec<String> = ["Reading.", "Bash: cargo test", "   ", ""]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(last_line_of(&lines).as_deref(), Some("Bash: cargo test"));
    assert_eq!(last_line_of(&[]), None);
    let long = vec!["y".repeat(500)];
    let got = last_line_of(&long).unwrap();
    assert_eq!(got.chars().count(), LAST_LINE_MAX);
}

#[test]
fn the_report_line_is_the_first_body_line() {
    let note = "---\ndevjob: K04\noutcome: needs you\n---\n\nNEEDS YOU: Will you grant it?\n\nStatus: more.\n";
    assert_eq!(
        report_line_of(note).as_deref(),
        Some("NEEDS YOU: Will you grant it?")
    );
    assert_eq!(report_line_of("Merged.\n").as_deref(), Some("Merged."));
    assert_eq!(report_line_of("---\na: b\n---\n\n\n"), None);
}

// ---- STARTABLE ---------------------------------------------------------------------------

const STUDIO_DRAFT: &str = "---\nstrand: S\nstrand-id: S1\ndevjob-host: studio\n---\n# S1\n";
const NO_HOST_DRAFT: &str = "---\nstrand: S\nstrand-id: S1\n---\n# S1\n";

/// A v2 note's lines, one above `### Later` and one under it.
fn note_item(id: &str) -> StrandItem {
    let src = "---\nstate: active\n---\n# S\n\n## Drafts\n\
- [ ] **S1** Go now. [[todo-list/Projects/drafts/s1]]\n\
- [ ] **S2** After S1. [[todo-list/Projects/drafts/s2]] (waits on: S1)\n\
### Later\n\
- [ ] **S3** Someday. [[todo-list/Projects/drafts/s3]]\n";
    parse_strand("S", src)
        .items
        .into_iter()
        .find(|i| i.id == id)
        .unwrap()
}

fn ok() -> Result<String, String> {
    Ok("Projects/drafts/s1.md".into())
}

#[test]
fn startable_needs_every_check() {
    assert_eq!(
        startable(&note_item("S1"), &ok(), Some(STUDIO_DRAFT), false),
        Ok(())
    );
}

#[test]
fn startable_refuses_a_line_that_is_not_runnable() {
    let e = Err("Projects/drafts/s1.md has no ## PROMPT section".to_string());
    assert_eq!(
        startable(&note_item("S1"), &e, Some(STUDIO_DRAFT), false),
        Err("Projects/drafts/s1.md has no ## PROMPT section".into())
    );
}

#[test]
fn startable_refuses_a_line_under_later() {
    let s3 = note_item("S3");
    assert_eq!(s3.section, StrandSection::Later);
    assert_eq!(
        startable(&s3, &ok(), Some(STUDIO_DRAFT), false),
        Err("Under Later.".into())
    );
}

#[test]
fn startable_refuses_a_line_that_waits_on_something() {
    assert_eq!(
        startable(&note_item("S2"), &ok(), Some(STUDIO_DRAFT), false),
        Err("Waits on S1.".into())
    );
}

#[test]
fn startable_refuses_a_draft_that_does_not_target_this_machine() {
    let e = startable(&note_item("S1"), &ok(), Some(NO_HOST_DRAFT), false).unwrap_err();
    assert!(e.contains("devjob-host"), "{e}");
    // The app shows this reason on the project view, where the machine is never named.
    assert!(!e.contains("Studio"), "{e}");
    assert!(startable(&note_item("S1"), &ok(), None, false).is_err());
}

#[test]
fn startable_refuses_a_step_with_a_live_run() {
    assert_eq!(
        startable(&note_item("S1"), &ok(), Some(STUDIO_DRAFT), true),
        Err("Already has a live run.".into())
    );
    // Live is the bridge's own: the statuses a second start is refused for.
    assert!(RunStatus::NeedsYou.is_live());
    assert!(!RunStatus::Interrupted.is_live());
}

// ---- LAST LOOK ---------------------------------------------------------------------------

#[test]
fn the_last_look_round_trips_through_its_file() {
    let dir = std::env::temp_dir().join(format!("jesse-last-look-{}", std::process::id()));
    let path = dir.join(LAST_LOOK_FILE);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(read_last_look(&path), None);
    write_last_look(&path, "2026-11-11T10:00:00.000Z").unwrap();
    assert_eq!(
        read_last_look(&path).as_deref(),
        Some("2026-11-11T10:00:00.000Z")
    );
    write_last_look(&path, "2026-11-11T11:00:00.000Z").unwrap();
    assert_eq!(
        read_last_look(&path).as_deref(),
        Some("2026-11-11T11:00:00.000Z")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
