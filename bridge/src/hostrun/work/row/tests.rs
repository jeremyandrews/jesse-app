//! The row's two phrases on the shapes real strand notes take, sanitised into
//! `tests/fixtures/work/rows/` (one note per shape), plus the pieces on their own.

use super::*;
use crate::hostrun::work::line_title;
use crate::strands::{parse_strand, StrandSection};

/// The day the fixtures are read on: `Bid.md` names it.
const TODAY: &str = "2026-11-12";

/// `(headline, your_action)` for a fixture note, as `annotate_strands` derives them.
fn phrases(name: &str) -> (Option<String>, Option<String>) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/work/rows")
        .join(format!("{name}.md"));
    let st = parse_strand(name, &std::fs::read_to_string(path).unwrap());
    let lines: Vec<(String, String)> = st
        .items
        .iter()
        .filter(|i| !i.id.is_empty())
        .map(|i| (i.id.clone(), line_title(&i.raw)))
        .collect();
    let texts: Vec<(String, String)> = st
        .items
        .iter()
        .filter(|i| !i.id.is_empty())
        .map(|i| (i.id.clone(), i.text.clone()))
        .collect();
    let next = st
        .items
        .iter()
        .find(|i| !i.checked && i.section == StrandSection::Queue)
        .map(|i| line_title(&i.raw));
    (
        st.now.as_deref().and_then(|n| headline(n, &lines, TODAY)),
        st.waiting
            .as_ref()
            .and_then(|w| your_action(&w.text, &lines, next.as_deref(), &texts)),
    )
}

fn h(name: &str) -> String {
    phrases(name)
        .0
        .unwrap_or_else(|| panic!("{name}: no headline"))
}

fn a(name: &str) -> String {
    phrases(name)
        .1
        .unwrap_or_else(|| panic!("{name}: no action"))
}

// ---- HEADLINE ----------------------------------------------------------------------------

#[test]
fn a_plain_lead_sentence_is_the_headline() {
    assert_eq!(h("Release"), "Two issues drafted, none released");
}

#[test]
fn the_clause_before_a_colon_is_the_headline() {
    assert_eq!(h("Proof"), "Proof v2 is in drafts");
}

#[test]
fn a_leading_id_is_reworded_around_its_title_without_its_branch() {
    // `BX12 merged: PR 828 (…) is on `release-0.19`…`
    assert_eq!(h("Bench"), "Merged: arm64 fallback chain");
    // `OA6 merged (PR 327, App 1.0 (211)): …`
    assert_eq!(
        h("App"),
        "Merged: Offline gate refuses live state questions"
    );
    // `OT1 finished: …`
    assert_eq!(h("Team"), "Finished: Off hours team trial proposal");
}

#[test]
fn a_possessive_lead_id_reads_as_the_step_and_a_running_line_reads_running() {
    // `W32's run is done`, its title `W9 rewrite prompt, …` without its id.
    // … unless the clause after the colon opens on a quoted name: that is the news.
    assert_eq!(h("Reviews"), "Finished: The Bargain");
    let w = vec![
        (
            "W32".to_string(),
            "W9 rewrite prompt, one article".to_string(),
        ),
        ("W9".to_string(), "Blog".to_string()),
    ];
    assert_eq!(
        headline("W32's run is done: the article is ready.", &w, TODAY).as_deref(),
        Some("Finished: The rewrite prompt, one article")
    );
    // `SML1 running (…)`: a line under way that is not a Studio run.
    assert_eq!(
        h("Search"),
        "Running: Package the checkout plus a sanitized copy of the local"
    );
}

#[test]
fn a_long_remainder_after_a_leading_id_drops_the_id() {
    assert_eq!(h("Sandbox"), "Round 4 stopped in pre-flight again");
}

#[test]
fn a_step_id_in_the_lead_parenthetical_names_the_clause_unless_it_already_does() {
    // `kc1 fixed for good (CL16c): …`
    assert_eq!(h("Cluster"), "kc1 fixed for good");
    // Without a shared word the title names it.
    let lines = vec![("CL9".to_string(), "Rotate the token".to_string())];
    assert_eq!(
        headline("Done at last (CL9): the datastore agrees.", &lines, TODAY).as_deref(),
        Some("Done at last: Rotate the token")
    );
}

#[test]
fn a_one_word_label_before_the_colon_is_skipped() {
    // `Next: carta d'identità in town (…) and the health card (…). …`
    assert_eq!(h("Errand"), "The library card in town and the gym pass");
    // `F3 is next: …` keeps the word, and the title names the step.
    assert_eq!(h("Ledger"), "Next: Watch the recorded August walkthrough");
}

#[test]
fn a_one_word_sentence_gives_way_to_the_next_and_mid_ids_go() {
    // `example.rs. The visual rework, the A1b cleanup and …`
    assert_eq!(
        h("Site"),
        "The visual rework, the cleanup and the pin bump have all merged and nothing is running"
    );
}

#[test]
fn a_sentence_naming_today_wins_and_keeps_its_time() {
    assert_eq!(h("Bid"), "Feedback call set: 17:00 to 17:10 CET, Teams");
    // Any other day, the lead sentence.
    let plain = "Not shortlisted (3 Nov). Feedback call set: Thu 12 Nov 17:00. The rest.";
    assert_eq!(
        headline(plain, &[], "2026-11-13").as_deref(),
        Some("Not shortlisted")
    );
    assert_eq!(
        headline(plain, &[], "2026-11-12").as_deref(),
        Some("Feedback call set: 17:00")
    );
}

#[test]
fn a_dated_lead_verb_keeps_only_the_verb() {
    // `I2 sent 16 October 13:39: …` and `EB10 handoff launched 2026-11-01: …`.
    let lines = vec![
        (
            "I2".to_string(),
            "Email to the accountant on the notice".to_string(),
        ),
        ("EB10".to_string(), "Video handoff".to_string()),
    ];
    assert_eq!(
        headline(
            "I2 sent 16 October 13:39: the notice is out.",
            &lines,
            TODAY
        )
        .as_deref(),
        Some("Sent: Email to the accountant on the notice")
    );
    assert_eq!(
        headline(
            "EB10 handoff launched 2026-11-01: a new session.",
            &lines,
            TODAY
        )
        .as_deref(),
        Some("Launched: Video handoff")
    );
}

#[test]
fn a_lone_verb_or_a_no_sentence_gives_way_to_the_next() {
    // `PR 208 merged 16:39` scrubs to `Merged 16:39`… a bare `merged` names nothing.
    assert_eq!(
        headline(
            "PR 327 merged. The offline gate refuses live questions.",
            &[],
            TODAY
        )
        .as_deref(),
        Some("The offline gate refuses live questions")
    );
    assert_eq!(
        headline(
            "No queued step. Ongoing: the wine in the garage.",
            &[],
            TODAY
        )
        .as_deref(),
        Some("The wine in the garage")
    );
    // Alone, it is still said.
    assert_eq!(
        headline("No queued step.", &[], TODAY).as_deref(),
        Some("No queued step")
    );
}

#[test]
fn a_count_in_a_dated_sentence_is_not_a_date() {
    assert_eq!(
        headline("Shipped 12 Nov: 3 fixes, 2 open.", &[], TODAY).as_deref(),
        Some("Shipped: 3 fixes, 2 open")
    );
}

#[test]
fn the_next_title_is_the_lines_whole_first_clause() {
    let lines = vec![("OT1".to_string(), "Trial".to_string())];
    // The title stops at 57 characters; the text says what the 87 is.
    assert_eq!(
        next_title(
            "Tape the waist at the navel and record it against the 87 cm decider.",
            "Tape the waist at the navel and record it against the 87\u{2026}",
            &lines
        )
        .as_deref(),
        Some("Tape the waist at the navel and record it against the 87 cm decider")
    );
    // A long head before the colon is the name; a one or two word label keeps its tail.
    assert_eq!(
        next_title(
            "Review the OT1 Slack draft (220 words) and answer its questions: hours.",
            "",
            &lines
        )
        .as_deref(),
        Some("Review the Slack draft and answer its questions")
    );
    // A label keeps the first clause of its tail.
    let w = vec![("W9".to_string(), "Blog".to_string())];
    assert_eq!(
        next_title(
            "Blog: The Bargain, the rewrite of W9; 76 sources.",
            "Blog",
            &w
        )
        .as_deref(),
        Some("Blog: The Bargain")
    );
    // A label whose tail is made of step ids is the label alone.
    let ids = vec![
        ("B30".to_string(), "x".to_string()),
        ("B33".to_string(), "y".to_string()),
    ];
    assert_eq!(
        next_title(
            "Release 0.105.0: tag main after B30 and B33, so the pin moves.",
            "",
            &ids
        )
        .as_deref(),
        Some("Release 0.105.0")
    );
    // No text: the title, cleaned.
    assert_eq!(
        next_title(
            "",
            "Clean rerun on the host under Podman, fresh\u{2026}",
            &[]
        )
        .as_deref(),
        Some("Clean rerun on the host under Podman")
    );
    assert_eq!(next_title("(only an aside)", "", &[]), None);
}

#[test]
fn an_empty_or_markup_only_now_has_no_headline() {
    assert_eq!(headline("", &[], TODAY), None);
    assert_eq!(headline("   ", &[], TODAY), None);
    assert_eq!(headline("(only an aside)", &[], TODAY), None);
}

#[test]
fn links_backticks_hashes_and_pr_numbers_leave_the_headline() {
    let now = "[[todo-list/Projects/Thing|The thing]] shipped in `2b908fb` with PR 1602 merged.";
    assert_eq!(
        headline(now, &[], TODAY).as_deref(),
        Some("The thing shipped in with merged")
    );
}

#[test]
fn an_abbreviation_does_not_end_the_sentence() {
    assert_eq!(
        first_sentence("Next is the ask at St. Clare's desk, in person. Then the rest."),
        "Next is the ask at St. Clare's desk, in person"
    );
    assert_eq!(first_sentence("Done. then more"), "Done");
}

// ---- YOUR ACTION -------------------------------------------------------------------------

#[test]
fn an_action_is_the_first_gate_without_its_asides() {
    assert_eq!(
        a("Release"),
        "generate the signing keypair per Release Runbook section 1"
    );
    assert_eq!(
        a("Proof"),
        "read the two remaining essays and pick what to keep"
    );
}

#[test]
fn a_multi_gate_waiting_on_names_the_first_gate_without_its_lead_in() {
    // `for S23, Part 0: the `reader` read only user …; for D2b, …; for P27, …`
    assert_eq!(
        a("Cluster"),
        "the reader read only user on the router and the laptop password file"
    );
}

#[test]
fn a_branch_reads_as_its_kind_in_an_action_and_leaves_a_headline() {
    // In the action the branch is what is being protected; in the headline only where.
    assert_eq!(
        a("Bench"),
        "branch protection on the release branch matching main's"
    );
    assert_eq!(h("Bench"), "Merged: arm64 fallback chain");
}

#[test]
fn a_build_number_stays_and_ing_verbs_read_as_instructions() {
    assert_eq!(
        a("App"),
        "install App 1.0 (203) and sign the Mac app with Push Notifications"
    );
    assert_eq!(
        a("Search"),
        "run npm run flush:expand-cache against the shared KV now that the expansion prompt is live"
    );
}

#[test]
fn a_gate_led_by_a_steps_possessive_id_is_that_step() {
    // `OT2's five questions (…); publishing …`, named from OT2's whole text.
    assert_eq!(
        a("Team"),
        "review the Slack draft and answer its five questions"
    );
}

#[test]
fn an_id_list_is_counted_as_drafts() {
    assert_eq!(a("Reviews"), "review 6 Blog drafts");
    let lines = vec![
        ("W1".to_string(), "One".to_string()),
        ("W2".to_string(), "Two".to_string()),
    ];
    assert_eq!(
        your_action("you: review W1 and W2.", &lines, None, &[]).as_deref(),
        Some("review 2 drafts")
    );
}

#[test]
fn a_remark_about_the_note_gives_way_to_the_first_queued_line() {
    assert_eq!(
        a("Household"),
        "email to the accountant sending the yearly report"
    );
    // `… is yours to run`: the remark's verb carries over.
    assert_eq!(a("Site"), "run the launch checklist");
    // With no queued line, the remark is all there is.
    assert_eq!(
        your_action("you: every line is yours.", &[], None, &[]).as_deref(),
        Some("every line is yours")
    );
}

#[test]
fn a_dropped_id_takes_its_preposition_with_it() {
    let lines = vec![("GD5".to_string(), "Run the thing".to_string())];
    assert_eq!(
        your_action(
            "you: pick `main` or a release branch for GD5 (asked 9 Nov); then the rest.",
            &lines,
            None,
            &[]
        )
        .as_deref(),
        Some("pick main or a release branch")
    );
    // Mid clause, an id with words after it leaves the preposition alone.
    assert_eq!(
        your_action(
            "you: the H1 Apple account setup.",
            &[("H1".into(), "Apple".into())],
            None,
            &[]
        )
        .as_deref(),
        Some("the Apple account setup")
    );
}

#[test]
fn a_machine_leaves_and_jeremy_is_you() {
    assert_eq!(
        next_title("Clean rerun on ks1 under Podman, fresh database.", "", &[]).as_deref(),
        Some("Clean rerun under Podman, fresh database")
    );
    assert_eq!(
        headline(
            "The link graph tool Jeremy calls \"tree search\". It runs.",
            &[],
            TODAY
        )
        .as_deref(),
        Some("The link graph tool you call \"tree search\"")
    );
    assert_eq!(
        headline("Jeremy's health beyond the diet logging. More.", &[], TODAY).as_deref(),
        Some("Your health beyond the diet logging")
    );
}

#[test]
fn whether_reads_as_decide_whether() {
    assert_eq!(
        a("Links"),
        "decide whether jesse-app is the right home for it"
    );
}

#[test]
fn paths_read_as_file_names_and_urls_as_hosts() {
    assert_eq!(
        your_action(
            "you: trust `~/.kube/home-ca.crt` on the laptop and log in at https://box.home/login as me (not boarded).",
            &[],
            None,
            &[]
        )
        .as_deref(),
        Some("trust home-ca.crt on the laptop and log in at box.home as me")
    );
    assert_eq!(
        your_action(
            "you: clone `org/repo` to `~/jesse/Code/org/repo`. Nothing runs until then.",
            &[],
            None,
            &[]
        )
        .as_deref(),
        Some("clone org/repo")
    );
}

#[test]
fn a_gate_that_is_not_his_has_no_action() {
    assert_eq!(phrases("Bid").1, None);
    assert_eq!(your_action("the partner: a letter", &[], None, &[]), None);
    assert_eq!(your_action("you:", &[], None, &[]), None);
    assert_eq!(your_action("you", &[], None, &[]), None);
    assert_eq!(your_action("", &[], None, &[]), None);
    // Any case.
    assert_eq!(
        your_action("You: Say yes.", &[], None, &[]).as_deref(),
        Some("Say yes")
    );
}

#[test]
fn a_long_phrase_is_cut_on_a_word() {
    let long = format!("you: {}", "word ".repeat(40));
    let got = your_action(&long, &[], None, &[]).unwrap();
    assert!(got.chars().count() <= ROW_PHRASE_MAX + 1, "{got}");
    assert!(got.ends_with("word\u{2026}"), "{got}");
    assert!(got.chars().count() > 100, "{got}");
}
