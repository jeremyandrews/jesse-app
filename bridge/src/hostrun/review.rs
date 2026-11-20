//! **A SECOND MODEL READS A STUDIO RUN'S WORK** (`hostrun::review`): the pure half of the cross
//! model review loop. The state machine lives on [`super::HostRuns`]; this module holds what it
//! says and what it checks, so every word is a constant a test pins byte for byte.
//!
//! ---- THE LOOP -------------------------------------------------------------------------
//!
//! A run started with a `reviewer` (a [`crate::ModelRegistry`] id) is a REVIEWED run. Its served
//! prompt gains [`REVIEWED_RUN_SENTENCE`] (or [`REVIEWED_RUN_SENTENCE_LOCAL`]): draft pull
//! request, no merge, a further round follows. When its implementing round reports, the bridge
//! posts nothing and tells nobody; the run goes `reviewing`, the reviewer reads the diff from
//! the merge base with the default branch to the head, read only, in its own detached worktree,
//! and answers JSON ([`parse_review`]). The bridge renders the findings into an Inbox note
//! ([`render_note`]) and hands them to the run's own session as the next round
//! ([`handoff_reply`]): verify each one, fix what holds, say nothing about any of it outside the
//! report. That answering round is the one Jeremy is told about. A review that cannot run hands
//! off [`unreviewed_reply`] instead and the report's first line says [`UNREVIEWED_PREFIX`].
//!
//! ---- WHAT THE REVIEWER CAN DO ------------------------------------------------------------
//!
//! Nothing but read. [`review_capability`] names its posture per harness: `read` on Claude Code
//! (`Read`, `Grep` and `Glob` scoped to the reviewer's worktree, no shell, no web) and on Codex
//! (the OS read-only sandbox, network off, web search off), `basic` on the direct harness (no
//! tools at all: the diff in the prompt is everything it sees, because its read tools are rooted
//! at the vault, not at a worktree, and its `fetch_url` is a network tool). It loads no MCP
//! server, so no devjob, gh, git or network tool reaches it on any harness. Each of those three
//! postures is a row the containment battery already records and passes.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Capability, CLAUDE_CODE_ID, CODEX_ID, DIRECT_ID};

// ---- CONSTANTS -----------------------------------------------------------------------------

/// The longest `focus` (and `review_focus`) a review takes, in bytes.
pub const MAX_FOCUS_BYTES: usize = 4 * 1024;
/// The most diff a reviewer is handed, in bytes; past this it is cut at a line and says so.
pub const MAX_DIFF_BYTES: usize = 192 * 1024;
/// `[hostrun] review_timeout_minutes` when unset.
pub const DEFAULT_REVIEW_TIMEOUT_MINUTES: u64 = 30;
/// The longest previous answer a retry quotes back, in bytes.
pub const MAX_RETRY_ECHO_BYTES: usize = 8 * 1024;
/// [`super::Round::auto`] of the round a review hands off.
pub const AUTO_REVIEW: &str = "review";
/// [`Review::origin`] of a reviewed run's own review.
pub const ORIGIN_START: &str = "start";
/// [`Review::origin`] of a review asked for after the fact (`devjob_review`).
pub const ORIGIN_REQUEST: &str = "request";
/// The first line of the final report of a reviewed run whose review could not run.
pub const UNREVIEWED_PREFIX: &str = "UNREVIEWED:";
/// The line a leak check puts at the top of the answering round's Inbox report.
pub const REVIEW_LEAK_PREFIX: &str = "REVIEW LEAK:";
/// The words the leak check looks for besides the reviewer and the finding ids, lowercase.
pub const LEAK_WORDS: [&str; 3] = ["reviewer", "code review", "review finding"];

/// What a reviewed run's served prompt says after its generated paragraph, exactly.
pub const REVIEWED_RUN_SENTENCE: &str = "Open any pull request as a draft pull request, do not \
merge it and do not mark it ready, and write your report as usual: a further round will follow \
before you finish.";

/// [`REVIEWED_RUN_SENTENCE`] for a local repository, exactly.
pub const REVIEWED_RUN_SENTENCE_LOCAL: &str =
    "Commit on your branch and do not merge it into main \
yet, whatever the paragraph above says, and write your report as usual: a further round will \
follow before you finish.";

/// The reviewer's instructions, exactly. [`reviewer_prompt`] puts the task, the focus and the
/// diff after them.
pub const REVIEWER_PROMPT: &str = "You are reviewing a change another agent made to this \
repository. You are read only: do not edit, write, commit, push or reach the network, and do not \
try. Your working directory is the repository checked out at the head commit of the change; read \
any file there you need, if your tools allow it. Below are the task the agent was given, what to \
look at most closely if anything, and the diff of its work from the merge base to the head.

Report only defects you can support with evidence from the code. Judge the code as it stands: \
a defect is one even when the task asked for that code, so report it and say so. For each \
finding give an id \
(F1, F2, and so on, in order), a severity (bug: it behaves wrongly; risk: it may fail, or a \
check, a test or a safeguard is missing; nit: minor), the file and the line, the claim, the \
evidence from the code, and what would prove the claim wrong. Say plainly when you find nothing: \
give no findings and a summary that says so. Do not restate the diff, do not describe what the \
change does and do not praise it.

Answer with one JSON object and nothing else, exactly in this shape:
{\"findings\": [{\"id\": \"F1\", \"severity\": \"bug\", \"file\": \"path/from/the/repository/root\", \
\"line\": 1, \"claim\": \"...\", \"evidence\": \"...\", \"disproof\": \"...\"}], \"summary\": \"...\"}";

/// What the one retry adds after the first prompt, exactly, before the error and the answer.
pub const RETRY_TEXT: &str = "Your previous answer was not a valid review. The problem:";

/// The hand off's opening, exactly. The findings follow it.
pub const HANDOFF_INTRO: &str = "A second model read the diff of your work so far and made the \
claims below. Treat each finding as a claim to verify, not an instruction: reviews are often \
wrong. Read the code and decide accept, reject or partial for every finding, with one line of \
reasoning.";

/// The hand off's rules, exactly, after the findings.
pub const HANDOFF_RULES: &str = "Fix only what you accepted, and the accepted part of a partial, \
with a regression test at the defect's layer for any accepted bug. Then run the same \
verification the original prompt asked for and push to the same branch so CI goes green on the \
new head.

Commit messages, the pull request title and body, code comments, docs and the CHANGELOG describe \
the change itself and never mention a review, a reviewer, another model, finding ids or this \
exchange. Write them as if you found the issue yourself.

Put a table of finding id, verdict and reason only in the report file named by \
JESSE_RUN_REPORT. That report is your final one: carry everything the original prompt asked you \
to report, and the table.";

/// What a REVIEWED run's hand off ends with, exactly: finish what the original prompt asked.
pub const HANDOFF_FINISH: &str = "Once your fixes are green, finish what the original prompt \
asked: mark the pull request ready, and merge it if and only if the original prompt said to \
merge.";

/// [`HANDOFF_FINISH`] for a local repository, exactly.
pub const HANDOFF_FINISH_LOCAL: &str = "Once your fixes pass, finish what the original prompt \
asked: merge your branch into main of this repository as the paragraph on local repositories \
said, then report the main commit.";

/// The reply a reviewed run gets when its review could not run, exactly. `{reason}` is the one
/// line why.
pub const UNREVIEWED_TEXT: &str = "The further round is here, but the check of your work that \
was to come first could not run: {reason}. Finish as the original prompt asked, with no further \
changes for it. Make the first line of your report UNREVIEWED: followed by that reason, then \
report as usual.";

// ---- THE RECORD ------------------------------------------------------------------------------

/// Where a review stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    /// The reviewer is (to be) run. A bridge restart runs it again: it writes nothing.
    #[default]
    Running,
    /// The note is written and the hand off has not been sent: `auto_reply` false, or the
    /// bridge stopped between the two.
    Ready,
    /// The answering round is queued or running.
    HandedOff,
    /// The answering round reported; the note is in `Inbox/archive/`.
    Answered,
    /// The review could not run (`error` says why). A reviewed run was handed off unreviewed.
    Failed,
    /// Never handed off, and the run was closed or archived: the note was archived with it.
    Dropped,
}

impl ReviewStatus {
    pub fn label(self) -> &'static str {
        match self {
            ReviewStatus::Running => "running",
            ReviewStatus::Ready => "ready",
            ReviewStatus::HandedOff => "handed_off",
            ReviewStatus::Answered => "answered",
            ReviewStatus::Failed => "failed",
            ReviewStatus::Dropped => "dropped",
        }
    }
}

/// How bad a finding says it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Bug,
    Risk,
    Nit,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Bug => "bug",
            Severity::Risk => "risk",
            Severity::Nit => "nit",
        }
    }
}

/// One finding, as the reviewer gave it and the schema accepted it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub file: String,
    pub line: u64,
    pub claim: String,
    pub evidence: String,
    /// What would prove the claim wrong.
    pub disproof: String,
}

/// The reviewer's whole answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOutput {
    pub findings: Vec<Finding>,
    pub summary: String,
}

/// One review of a run, in `run.json` and in the run's JSON (`reviews`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Review {
    /// 1, 2, …: the note's `-review-<n>`.
    pub n: u32,
    /// The registry id, exactly as the model picker names it.
    pub reviewer: String,
    /// The registry entry's label when the review was asked for.
    #[serde(default)]
    pub reviewer_label: String,
    /// `start` (a reviewed run's own review) or `request` (`devjob_review`).
    pub origin: String,
    pub status: ReviewStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    #[serde(default = "yes")]
    pub auto_reply: bool,
    pub requested_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// The commit reviewed, and the merge base the diff starts from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The note, under the notes directory, as written (the routes serve where it is now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Why it failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The round the hand off queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_round: Option<u32>,
    /// When the answering round's report was written (and the note archived).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<String>,
    /// The run's status when a requested review began, restored when it does not hand off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_status: Option<super::RunStatus>,
    /// What the leak check found after the answering round.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leaks: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Review {
    /// The findings count the note's front matter names.
    pub fn count(&self) -> usize {
        self.findings.len()
    }

    /// Whether round `n` is this review's answering round still to be reported: the round the
    /// hand off queued, or a later one that continued it (after a usage limit or a restart).
    pub fn answers(&self, n: u32) -> bool {
        self.answered_at.is_none() && self.answer_round.is_some_and(|a| n >= a)
    }
}

// ---- WHO MAY REVIEW, AND HOW -----------------------------------------------------------------

/// The posture a reviewer runs at on `harness`: the most it may read that is still read only
/// and offline. `Err` names why a harness cannot review at all; never a fallback to another one.
pub fn review_capability(harness: &str) -> Result<Capability, String> {
    match harness {
        CLAUDE_CODE_ID | CODEX_ID => Ok(Capability::Read),
        // Its read tools are rooted at the vault, not a worktree, and `fetch_url` is a network
        // tool, so `read` would hand it the wrong tree and the network. `basic` is no tools at
        // all: it reviews the diff in its prompt.
        DIRECT_ID => Ok(Capability::Basic),
        other => Err(format!(
            "harness {other:?} has no read only posture this bridge can give a reviewer"
        )),
    }
}

/// Whether `id` can name a registry model (the same shape `devjob-model` takes).
pub fn valid_reviewer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '@'))
}

/// A `focus` checked: trimmed, `None` when blank, refused past [`MAX_FOCUS_BYTES`].
pub fn checked_focus(focus: Option<&str>) -> Result<Option<String>, String> {
    match focus.map(str::trim) {
        None | Some("") => Ok(None),
        Some(f) if f.len() > MAX_FOCUS_BYTES => Err(format!(
            "focus is {} bytes; the most is {MAX_FOCUS_BYTES}",
            f.len()
        )),
        Some(f) => Ok(Some(f.to_string())),
    }
}

// ---- THE REVIEWER'S PROMPT -------------------------------------------------------------------

/// A Markdown fence longer than any run of backticks in `text`.
fn fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// `diff` cut to [`MAX_DIFF_BYTES`] at a line boundary, and whether it was cut.
pub fn clip_diff(diff: &str) -> (&str, bool) {
    if diff.len() <= MAX_DIFF_BYTES {
        return (diff, false);
    }
    let mut end = MAX_DIFF_BYTES;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    let end = diff[..end].rfind('\n').map(|i| i + 1).unwrap_or(end);
    (&diff[..end], true)
}

/// The reviewer's whole prompt: [`REVIEWER_PROMPT`], the task the run was served, the focus when
/// there is one, and the diff, each under its own heading.
pub fn reviewer_prompt(
    section: &str,
    focus: Option<&str>,
    base: &str,
    head: &str,
    diff: &str,
) -> String {
    let (diff, cut) = clip_diff(diff);
    let mut s = String::from(REVIEWER_PROMPT);
    s.push_str("\n\n## The task the agent was given\n\n");
    let task_fence = fence_for(section);
    s.push_str(&format!(
        "{task_fence}markdown\n{}\n{task_fence}\n",
        section.trim_matches('\n')
    ));
    if let Some(f) = focus {
        s.push_str(&format!("\n## Look most closely at\n\n{f}\n"));
    }
    s.push_str(&format!("\n## The diff, {base}..{head}\n\n"));
    let fence = fence_for(diff);
    s.push_str(&format!("{fence}diff\n{}", diff));
    if !diff.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(&format!("{fence}\n"));
    if cut {
        s.push_str(&format!(
            "\nThe diff is cut at {MAX_DIFF_BYTES} bytes; read the rest from the working \
             directory if your tools allow it.\n"
        ));
    }
    s
}

/// The one retry's prompt: the first prompt, [`RETRY_TEXT`], the problem, and the previous
/// answer (at most [`MAX_RETRY_ECHO_BYTES`]).
pub fn retry_prompt(first: &str, raw: &str, error: &str) -> String {
    let mut end = raw.len().min(MAX_RETRY_ECHO_BYTES);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    let echo = &raw[..end];
    let fence = fence_for(echo);
    format!(
        "{first}\n## Your previous answer\n\n{RETRY_TEXT} {error}\n\n{fence}\n{echo}\n{fence}\n\n\
         Answer again with the one JSON object only.\n"
    )
}

// ---- THE SCHEMA ------------------------------------------------------------------------------

/// The JSON in a reviewer's answer: the whole text, else the body of its first ```json fence,
/// else the outermost braces.
fn json_of(text: &str) -> Option<&str> {
    let t = text.trim();
    if t.starts_with('{') && t.ends_with('}') {
        return Some(t);
    }
    for open in ["```json", "```JSON", "```"] {
        if let Some(i) = t.find(open) {
            let body = &t[i + open.len()..];
            if let Some(j) = body.find("```") {
                let inner = body[..j].trim();
                if inner.starts_with('{') && inner.ends_with('}') {
                    return Some(inner);
                }
            }
        }
    }
    let (a, b) = (t.find('{')?, t.rfind('}')?);
    (a < b).then(|| &t[a..=b])
}

/// Validate a reviewer's answer against the schema: one object with exactly `findings` and
/// `summary`; each finding exactly the seven fields, ids `F1`, `F2`, … in order, a known
/// severity, a file, a line of at least 1, and every text non-empty; and a summary that is not
/// blank. `Err` says the first thing wrong, for the retry.
pub fn parse_review(text: &str) -> Result<ReviewOutput, String> {
    let Some(json) = json_of(text) else {
        return Err("the answer holds no JSON object".into());
    };
    let out: ReviewOutput =
        serde_json::from_str(json).map_err(|e| format!("the JSON does not fit the shape: {e}"))?;
    if out.summary.trim().is_empty() {
        return Err("summary is empty".into());
    }
    for (i, f) in out.findings.iter().enumerate() {
        let want = format!("F{}", i + 1);
        if f.id != want {
            return Err(format!(
                "finding {} has id {:?}; it must be {want}",
                i + 1,
                f.id
            ));
        }
        if f.file.trim().is_empty() {
            return Err(format!("{want} names no file"));
        }
        if f.line == 0 {
            return Err(format!("{want}: line must be 1 or more"));
        }
        for (name, v) in [
            ("claim", &f.claim),
            ("evidence", &f.evidence),
            ("disproof", &f.disproof),
        ] {
            if v.trim().is_empty() {
                return Err(format!("{want}: {name} is empty"));
            }
        }
    }
    Ok(out)
}

// ---- WHAT THE SESSION IS HANDED ----------------------------------------------------------------

/// The findings as the hand off and the note list them.
pub fn render_findings(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "No findings.\n".to_string();
    }
    let mut s = String::new();
    for f in findings {
        s.push_str(&format!(
            "### {} ({}) {}:{}\n\nClaim: {}\n\nEvidence: {}\n\nWhat would prove it wrong: {}\n\n",
            f.id,
            f.severity.label(),
            f.file,
            f.line,
            f.claim.trim(),
            f.evidence.trim(),
            f.disproof.trim()
        ));
    }
    s
}

/// The hand off reply, exactly: [`HANDOFF_INTRO`], the findings, [`HANDOFF_RULES`], and `finish`
/// ([`HANDOFF_FINISH`] or [`HANDOFF_FINISH_LOCAL`]) for a reviewed run.
pub fn handoff_reply(findings: &[Finding], finish: Option<&str>) -> String {
    let mut s = format!("{HANDOFF_INTRO}\n\n{}", render_findings(findings));
    if !s.ends_with("\n\n") {
        s.push('\n');
    }
    s.push_str(HANDOFF_RULES);
    if let Some(f) = finish {
        s.push_str("\n\n");
        s.push_str(f);
    }
    s.push('\n');
    s
}

/// The reply a reviewed run gets when its review could not run, exactly: [`UNREVIEWED_TEXT`]
/// with the reason, and `finish` after it.
pub fn unreviewed_reply(reason: &str, finish: Option<&str>) -> String {
    let mut s = UNREVIEWED_TEXT.replace("{reason}", &one_line(reason));
    if let Some(f) = finish {
        s.push_str("\n\n");
        s.push_str(f);
    }
    s.push('\n');
    s
}

/// `reason` on one line, trimmed of its final full stop, at most 300 characters.
pub fn one_line(reason: &str) -> String {
    let joined = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = joined.chars().take(300).collect();
    t.trim_end_matches('.').to_string()
}

/// `UNREVIEWED: <reason>` as the report's first line, unless the session already wrote one.
pub fn ensure_unreviewed(report: &str, reason: &str) -> String {
    if super::first_line(report).starts_with(UNREVIEWED_PREFIX) {
        return report.to_string();
    }
    format!("{UNREVIEWED_PREFIX} {}\n\n{report}", one_line(reason))
}

// ---- THE NOTE --------------------------------------------------------------------------------

/// `Inbox/<date>-run-<run_id>-review-<n>.md`.
pub fn note_rel_path(date: &str, run_id: &str, n: u32) -> String {
    format!(
        "{}/{date}-run-{run_id}-review-{n}.md",
        crate::vaultwrites::INBOX_DIR
    )
}

/// The review note: front matter naming the run, the reviewer's id and label, the head
/// reviewed and the finding count, then the summary and the findings. It carries no `host` or
/// `strand-id`, so nothing reads it as a round's report.
pub fn render_note(run_id: &str, repo: &str, branch: &str, review: &Review) -> String {
    let head = review.head.as_deref().unwrap_or("");
    let base = review.base.as_deref().unwrap_or("");
    let mut s = format!(
        "---\nrun: {run_id}\nreview: {}\nreviewer: {}\nreviewer-label: {}\nrepo: {repo}\n\
         branch: {branch}\nhead: {head}\nbase: {base}\nfindings: {}\n---\n\n",
        review.n,
        review.reviewer,
        review.reviewer_label,
        review.count()
    );
    s.push_str(&format!(
        "# Review {} of run {run_id}\n\n{} (`{}`) read `{base}..{head}`.\n\n",
        review.n, review.reviewer_label, review.reviewer
    ));
    if let Some(f) = &review.focus {
        s.push_str(&format!("Focus: {}\n\n", one_line(f)));
    }
    if let Some(sum) = &review.summary {
        s.push_str(sum.trim());
        s.push_str("\n\n");
    }
    s.push_str("## Findings\n\n");
    s.push_str(&render_findings(&review.findings));
    while s.ends_with("\n\n") {
        s.pop();
    }
    s
}

/// The line the answering round's Inbox report carries about the note: where it is now.
pub fn note_link(rel: &str) -> String {
    let target = rel.strip_suffix(".md").unwrap_or(rel);
    format!("Review note: [[{target}]]")
}

// ---- THE LEAK CHECK --------------------------------------------------------------------------

/// What the leak check looks for: the reviewer's registry id and label, every finding id as the
/// note wrote it, and [`LEAK_WORDS`].
pub fn leak_terms(review: &Review) -> Vec<String> {
    let mut v = vec![review.reviewer.clone(), review.reviewer_label.clone()];
    v.extend(review.findings.iter().map(|f| f.id.clone()));
    v.extend(LEAK_WORDS.iter().map(|w| w.to_string()));
    v.retain(|t| !t.trim().is_empty());
    let mut seen = std::collections::HashSet::new();
    v.retain(|t| seen.insert(t.to_lowercase()));
    v
}

/// Whether `term` stands in `text` as a whole word (case blind): not inside a longer word or
/// identifier, so `F1` does not match `F12` or `xF1`.
fn has_word(text: &str, term: &str) -> bool {
    let (hay, needle) = (text.to_lowercase(), term.to_lowercase());
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(i) = hay[from..].find(&needle) {
        let at = from + i;
        let end = at + needle.len();
        let before = hay[..at].chars().next_back();
        let after = hay[end..].chars().next();
        if !before.is_some_and(word) && !after.is_some_and(word) {
            return true;
        }
        from = at + needle.len().max(1);
        while !hay.is_char_boundary(from) {
            from += 1;
        }
    }
    false
}

/// Every hit: `<where>: <term>`, once per place and term. `places` is (where, text).
pub fn leak_hits(terms: &[String], places: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (place, text) in places {
        let found: Vec<&str> = terms
            .iter()
            .filter(|t| has_word(text, t))
            .map(String::as_str)
            .collect();
        if !found.is_empty() {
            out.push(format!("{place}: {}", found.join(", ")));
        }
    }
    out
}

/// The line at the top of the round's Inbox report, when there is a hit.
pub fn leak_line(hits: &[String]) -> Option<String> {
    (!hits.is_empty()).then(|| format!("{REVIEW_LEAK_PREFIX} {}", hits.join("; ")))
}

/// The added lines of a unified diff, each with the file it is in: `(file, line)`.
pub fn added_lines(diff: &str) -> Vec<(String, String)> {
    let mut file = String::new();
    let mut out = Vec::new();
    for l in diff.lines() {
        if let Some(f) = l.strip_prefix("+++ ") {
            file = f.strip_prefix("b/").unwrap_or(f).to_string();
        } else if l.starts_with("--- ") || l.starts_with("diff --git") {
            continue;
        } else if let Some(added) = l.strip_prefix('+') {
            out.push((file.clone(), added.to_string()));
        }
    }
    out
}

// ---- GIT ---------------------------------------------------------------------------------------

/// The reviewer's worktree is `<run dir>/review-<n>`.
pub const REVIEW_WORKTREE_PREFIX: &str = "review-";

/// `review-<n>-answer-<k>.txt`: the reviewer's raw answer, kept in the run directory.
pub fn answer_file(n: u32, k: u32) -> String {
    format!("{REVIEW_WORKTREE_PREFIX}{n}-answer-{k}.txt")
}

/// Whether the reviewer's worktree is exactly as it was given: still at `head`, detached,
/// nothing changed and nothing added (ignored files included).
pub fn worktree_untouched(worktree: &Path, head: &str) -> Result<(), String> {
    let git = |args: &[&str]| super::git::git(worktree, args).map(|s| s.trim().to_string());
    let now = git(&["rev-parse", "HEAD"])?;
    if now != head {
        return Err(format!("its HEAD moved from {head} to {now}"));
    }
    if git(&["symbolic-ref", "-q", "HEAD"]).is_ok() {
        return Err("it is on a branch".into());
    }
    let status = git(&[
        "status",
        "--porcelain",
        "--ignored",
        "--untracked-files=all",
    ])?;
    if !status.is_empty() {
        let first: Vec<&str> = status.lines().take(3).collect();
        return Err(format!("it has changes: {}", first.join("; ")));
    }
    Ok(())
}

/// The base and head a review reads, from the run's worktree: its `HEAD`, and the merge base of
/// that with `main_ref` (`origin/<default>`, or a local repository's `main`). When the head is
/// already in `main_ref` (the run merged), the run's own `base_sha` is used instead, so a merged
/// run can still be reviewed. `Err` when there is no commit between the two.
pub fn review_range(
    worktree: &Path,
    main_ref: &str,
    base_sha: Option<&str>,
) -> Result<(String, String), String> {
    let git = |args: &[&str]| super::git::git(worktree, args).map(|s| s.trim().to_string());
    let head = git(&["rev-parse", "HEAD"])?;
    let merge_base = git(&["merge-base", "HEAD", main_ref]).ok();
    let base = match merge_base {
        Some(b) if b != head => b,
        _ => match base_sha {
            Some(b) if b != head => b.to_string(),
            _ => return Err("it has no commits beyond its base".into()),
        },
    };
    let count = git(&["rev-list", "--count", &format!("{base}..{head}")])?;
    if count.trim() == "0" {
        return Err("it has no commits beyond its base".into());
    }
    Ok((base, head))
}

/// `git diff <base>..<head>` in `dir`, with no color and no external diff driver.
pub fn diff(dir: &Path, base: &str, head: &str) -> Result<String, String> {
    super::git::git(
        dir,
        &[
            "-c",
            "core.quotepath=off",
            "diff",
            "--no-color",
            "--no-ext-diff",
            &format!("{base}..{head}"),
        ],
    )
}

/// The full messages of the commits in `<from>..<to>`, each with its short id.
pub fn commit_messages(dir: &Path, from: &str, to: &str) -> Vec<(String, String)> {
    let Ok(out) = super::git::git(
        dir,
        &["log", "--format=%h%x1f%B%x1e", &format!("{from}..{to}")],
    ) else {
        return Vec::new();
    };
    out.split('\u{1e}')
        .filter_map(|c| {
            let (id, body) = c.trim_start_matches('\n').split_once('\u{1f}')?;
            Some((id.to_string(), body.to_string()))
        })
        .collect()
}
