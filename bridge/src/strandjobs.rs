//! **STRAND ENTRIES** — a strand's `## Drafts` queue, fed to the dev-job scheduler.
//!
//! A schedule entry with `kind = "devjob"` and `strand = "<Note>"` names no prompt and no
//! repository. At each fire [`fire`] reads `Strands/<Note>.md` fresh, parses it with
//! [`crate::strands::parse_strand`], walks its Queue lines in file order and starts the first
//! eligible one's prompt as a dev job, through [`DevJobs::start_run`], exactly as
//! `devjob_start` and the fixed form do. At most one job per fire.
//!
//! ---- ELIGIBILITY ---------------------------------------------------------------------
//!
//! Checked in this order, so a skip names the first that failed ([`eligibility`]):
//!
//! 1. the line is a Queue line (above `### Later`, not running), unchecked, with an id that
//!    can be a Kubernetes label value;
//! 2. it has no `(waits on: …)`;
//! 3. its link resolves ([`resolve_target`]) to a file under `Projects/drafts/` with no
//!    `archive` segment;
//! 4. that file's front matter has `strand` equal to the note's slug and `strand-id` equal to
//!    the line's id;
//! 5. that file's front matter has a `devjob-repo` that is `owner/name`. This key is the opt
//!    in: a prompt written for a person at a desk has none and is never run.
//!
//! `devjob-agent`, `devjob-model`, `devjob-test` and `devjob-timeout` (minutes) override the
//! entry's defaults, each validated as [`DevJobSpec::validate`] validates the same field.
//!
//! ---- IN FLIGHT -----------------------------------------------------------------------
//!
//! An eligible line is still refused (`in flight`) while a Job labelled with its strand and id
//! exists in any phase, or while a report naming the step sits directly in `Inbox/`. That is
//! the whole guard against running a step twice: **the bridge writes nothing under
//! `Strands/` or `Projects/drafts/`**. Its only vault write stays the dev job's Inbox report,
//! whose front matter names the step, and the vault's morning routine moves or blocks the line
//! after it reads that report (the precedent is [`crate::strandticks`]: the bridge detects and
//! triggers, the vault skill closes the step).

use crate::devjob::{
    carries_strand_step, valid_label_value, valid_repo, Agent, DevJobArgs, DevJobSpec, DevJobs,
    Started, StrandRef, MAX_SLUG_CHARS,
};
use crate::schedule::StrandTemplate;
use crate::strands::{
    frontmatter_value, parse_strand, resolve_target, Strand, StrandSection, ARCHIVE_SEGMENT,
    STRANDS_DIR,
};
use crate::*;

/// The folder a strand entry runs prompts from. A link anywhere else is not a step to run.
pub const DRAFTS_DIR: &str = "Projects/drafts";

/// The start of the reason a fire with nothing eligible records. A `ran` that is not pushed:
/// a quiet night is the queue working.
pub const NOTHING_ELIGIBLE: &str = "nothing eligible in strand ";

/// The reason a line is refused when its step is already running or reported.
pub const IN_FLIGHT: &str = "in flight";

/// One eligible line, ready to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub step: StrandRef,
    pub spec: DevJobSpec,
}

/// One Queue line that was not eligible: its id (or `line N` when it has none) and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
    pub id: String,
    pub why: String,
}

/// What a walk of the queue settled on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Run(Box<Candidate>),
    /// The one line reason: the first Queue line's id and the first check it failed, or
    /// `no queue lines`.
    Nothing(String),
}

/// What one fire of a strand entry came to.
#[derive(Debug)]
pub enum StrandFire {
    Started {
        started: Started,
        step: StrandRef,
        spec: Box<DevJobSpec>,
    },
    /// Nothing eligible: `ran`, no push, no vault write. Carries the full reason.
    Nothing(String),
    /// The note is missing or done, or the cluster could not be asked or refused the start.
    Failed(String),
}

/// One front matter scalar, with one pair of surrounding quotes removed.
fn fm(src: &str, key: &str) -> Option<String> {
    let v = frontmatter_value(src, key)?;
    let unquoted = ['"', '\'']
        .iter()
        .find_map(|q| v.strip_prefix(*q).and_then(|r| r.strip_suffix(*q)))
        .unwrap_or(&v)
        .trim()
        .to_string();
    (!unquoted.is_empty()).then_some(unquoted)
}

/// Lowercase letters, digits and single hyphens: anything else becomes a hyphen.
fn slug_part(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '-'
        };
        if c == '-' && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        out.push(c);
    }
    out.trim_end_matches('-').to_string()
}

/// `<note>-<id>-<YYYYMMDD>`, lowercased, any character a branch slug may not hold made a
/// hyphen, and cut from the left of the note part only when the whole would pass
/// [`MAX_SLUG_CHARS`].
pub fn branch_slug(note: &str, id: &str, yyyymmdd: &str) -> String {
    let suffix = format!("{}-{yyyymmdd}", slug_part(id));
    let room = MAX_SLUG_CHARS.saturating_sub(suffix.len() + 1);
    let note = slug_part(note);
    let note = if note.len() > room {
        note[note.len() - room..].trim_start_matches('-')
    } else {
        note.as_str()
    };
    if note.is_empty() {
        suffix
    } else {
        format!("{note}-{suffix}")
    }
}

fn is_rule(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

fn is_checkbox(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("- [ ]") || t.starts_with("- [x]") || t.starts_with("- [X]")
}

/// The prompt a file holds: its body with the front matter removed, and with a trailing
/// archive footer removed. The footer is a horizontal rule followed only by checkbox lines
/// (blank lines aside); the rule, those lines and the blank lines just before the rule go, and
/// nothing else. A rule anywhere else in the body is part of the prompt.
pub fn prompt_body(src: &str) -> String {
    let lines: Vec<&str> = src.split_inclusive('\n').collect();
    let mut start = 0;
    if lines.first().map(|l| l.trim()) == Some("---") {
        if let Some(p) = lines.iter().skip(1).position(|l| l.trim() == "---") {
            start = p + 2;
        }
    }
    let body = &lines[start..];
    let footer = body.iter().rposition(|l| is_rule(l)).filter(|&hr| {
        let after = &body[hr + 1..];
        after.iter().any(|l| is_checkbox(l))
            && after.iter().all(|l| l.trim().is_empty() || is_checkbox(l))
    });
    match footer {
        None => body.concat(),
        Some(mut hr) => {
            while hr > 0 && body[hr - 1].trim().is_empty() {
                hr -= 1;
            }
            body[..hr].concat()
        }
    }
}

/// Whether `notes_root/Inbox/` (not its `archive/`) holds a dev job report naming this step.
pub fn inbox_reports_step(notes_root: &Path, note: &str, id: &str) -> bool {
    let inbox = notes_root.join(crate::vaultwrites::INBOX_DIR);
    let Ok(entries) = std::fs::read_dir(&inbox) else {
        return false;
    };
    entries.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".md") || !name.contains("-dev-job-") {
            return false;
        }
        if !e.file_type().is_ok_and(|t| t.is_file()) {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            return false;
        };
        fm(&text, "strand").as_deref() == Some(note)
            && fm(&text, "strand-id").as_deref() == Some(id)
    })
}

/// Read `Strands/<note>.md` fresh and parse it. A missing note, or one whose `state` is
/// `done`, is an error the fire records as `failed`.
pub fn load_strand(notes_root: &Path, note: &str) -> Result<Strand, String> {
    let rel = format!("{STRANDS_DIR}/{note}.md");
    let src = std::fs::read_to_string(notes_root.join(&rel))
        .map_err(|e| format!("strand note {rel} is missing or unreadable: {e}"))?;
    let strand = parse_strand(note, &src);
    if strand.state.trim() == "done" {
        return Err(format!(
            "strand {note} is done (its front matter says state: done)"
        ));
    }
    Ok(strand)
}

/// One validated override, or the error naming its key.
fn checked<T>(key: &str, r: Result<T, String>) -> Result<T, String> {
    r.map_err(|e| format!("{key}: {e}"))
}

/// The five checks, in order, for one Queue line; then the overrides and the spec. `Err` is the
/// reason the line is skipped.
pub fn eligibility(
    notes_root: &Path,
    t: &StrandTemplate,
    item: &crate::strands::StrandItem,
    default_agent: Agent,
    yyyymmdd: &str,
) -> Result<Candidate, String> {
    // 1. A Queue line, unchecked, with an id.
    if item.section != StrandSection::Queue {
        return Err("not a queue line".into());
    }
    if item.checked {
        return Err("checked".into());
    }
    if item.id.is_empty() {
        return Err("no id".into());
    }
    if !valid_label_value(&item.id) {
        return Err("the id is not a Kubernetes label value".into());
    }
    // 2. Not waiting.
    if let Some(w) = &item.waits_on {
        return Err(format!("waits on: {w}"));
    }
    // 3. A live prompt under Projects/drafts/.
    let Some(link) = item.link.as_deref() else {
        return Err("no link".into());
    };
    let Some(path) = resolve_target(notes_root, link) else {
        return Err(format!("link [[{link}]] does not resolve"));
    };
    if !path.starts_with(&format!("{DRAFTS_DIR}/")) {
        return Err(format!("link {path} is not under {DRAFTS_DIR}/"));
    }
    if path.split('/').any(|seg| seg == ARCHIVE_SEGMENT) {
        return Err(format!("link {path} is archived"));
    }
    let src = std::fs::read_to_string(notes_root.join(&path))
        .map_err(|e| format!("prompt {path} is unreadable: {e}"))?;
    // 4. The prompt is this step.
    match fm(&src, "strand") {
        None => return Err(format!("prompt {path} has no strand")),
        Some(s) if s != t.note => return Err(format!("prompt {path} belongs to strand {s}")),
        Some(_) => {}
    }
    match fm(&src, "strand-id") {
        None => return Err(format!("prompt {path} has no strand-id")),
        Some(s) if s != item.id => {
            return Err(format!("prompt {path} is step {s}, not {}", item.id))
        }
        Some(_) => {}
    }
    // 5. The opt in.
    let Some(repo) = fm(&src, "devjob-repo") else {
        return Err(format!("prompt {path} has no devjob-repo"));
    };
    if !valid_repo(&repo) {
        return Err(format!(
            "prompt {path}: devjob-repo {repo:?} is not owner/name"
        ));
    }

    // The overrides, each checked on its own so the error names its key.
    let base = DevJobArgs {
        repo: Some(repo.clone()),
        prompt: Some("x".into()),
        branch_slug: Some("x".into()),
        ..DevJobArgs::default()
    };
    let one = |args: DevJobArgs| DevJobSpec::validate(args, default_agent).map(|_| ());
    let agent = match fm(&src, "devjob-agent") {
        Some(a) => {
            checked("devjob-agent", Agent::parse(&a)).map_err(|e| format!("prompt {path}: {e}"))?
        }
        None => t.agent.unwrap_or(default_agent),
    };
    let model = match fm(&src, "devjob-model") {
        Some(m) => {
            checked(
                "devjob-model",
                one(DevJobArgs {
                    model: Some(m.clone()),
                    ..base.clone()
                }),
            )
            .map_err(|e| format!("prompt {path}: {e}"))?;
            Some(m)
        }
        None => t.model.clone(),
    };
    let test_command = match fm(&src, "devjob-test") {
        Some(c) => {
            checked(
                "devjob-test",
                one(DevJobArgs {
                    test_command: Some(c.clone()),
                    ..base.clone()
                }),
            )
            .map_err(|e| format!("prompt {path}: {e}"))?;
            Some(c)
        }
        None => None,
    };
    let timeout_minutes = match fm(&src, "devjob-timeout") {
        Some(raw) => {
            let m = checked(
                "devjob-timeout",
                raw.parse::<u64>()
                    .map_err(|_| format!("{raw:?} is not a whole number of minutes")),
            )
            .map_err(|e| format!("prompt {path}: {e}"))?;
            checked(
                "devjob-timeout",
                one(DevJobArgs {
                    timeout_minutes: Some(m),
                    ..base.clone()
                }),
            )
            .map_err(|e| format!("prompt {path}: {e}"))?;
            Some(m)
        }
        None => t.timeout_minutes,
    };
    let spec = DevJobSpec::validate(
        DevJobArgs {
            repo: Some(repo),
            prompt: Some(prompt_body(&src)),
            agent: Some(agent.label().to_string()),
            model,
            branch_slug: Some(branch_slug(&t.note, &item.id, yyyymmdd)),
            test_command,
            timeout_minutes,
        },
        default_agent,
    )
    .map_err(|e| format!("prompt {path}: {e}"))?;
    Ok(Candidate {
        step: StrandRef {
            note: t.note.clone(),
            id: item.id.clone(),
            prompt: path,
        },
        spec,
    })
}

/// Every Queue line of the note, in file order, judged. Later, Running and Done lines are not
/// considered at all.
pub fn candidates(
    notes_root: &Path,
    t: &StrandTemplate,
    strand: &Strand,
    default_agent: Agent,
    yyyymmdd: &str,
) -> Vec<Result<Candidate, Skip>> {
    strand
        .items
        .iter()
        .filter(|i| i.section == StrandSection::Queue)
        .map(|i| {
            eligibility(notes_root, t, i, default_agent, yyyymmdd).map_err(|why| Skip {
                id: if i.id.is_empty() {
                    format!("line {}", i.line)
                } else {
                    i.id.clone()
                },
                why,
            })
        })
        .collect()
}

/// The first eligible line that is not in flight, or the reason for the first Queue line.
pub fn choose(cands: Vec<Result<Candidate, Skip>>, in_flight: impl Fn(&StrandRef) -> bool) -> Pick {
    let mut first: Option<Skip> = None;
    for c in cands {
        let skip = match c {
            Ok(c) if !in_flight(&c.step) => return Pick::Run(Box::new(c)),
            Ok(c) => Skip {
                id: c.step.id,
                why: IN_FLIGHT.into(),
            },
            Err(s) => s,
        };
        first.get_or_insert(skip);
    }
    Pick::Nothing(match first {
        Some(s) => format!("{}: {}", s.id, s.why),
        None => "no queue lines".into(),
    })
}

/// One fire of a strand entry: read the note, pick, and start at most one run. `yyyymmdd` is
/// the occurrence's date, for the branch slug; `origin` is `schedule:<id>`.
pub async fn fire(
    svc: &DevJobs,
    notes_root: &Path,
    t: &StrandTemplate,
    yyyymmdd: &str,
    origin: &str,
) -> StrandFire {
    let strand = match load_strand(notes_root, &t.note) {
        Ok(s) => s,
        Err(e) => return StrandFire::Failed(e),
    };
    let cands = candidates(notes_root, t, &strand, svc.default_agent(), yyyymmdd);
    // The cluster is asked only when some line could run.
    let jobs = if cands.iter().any(Result::is_ok) {
        match svc.strand_jobs(&t.note).await {
            Ok(j) => j,
            Err(e) => return StrandFire::Failed(e),
        }
    } else {
        Vec::new()
    };
    let in_flight = |step: &StrandRef| {
        jobs.iter()
            .any(|j| carries_strand_step(j, &step.note, &step.id))
            || inbox_reports_step(notes_root, &step.note, &step.id)
    };
    match choose(cands, in_flight) {
        Pick::Nothing(why) => StrandFire::Nothing(format!("{NOTHING_ELIGIBLE}{}: {why}", t.note)),
        Pick::Run(c) => match svc.start_run(&c.spec, origin, Some(&c.step)).await {
            Ok(started) => StrandFire::Started {
                started,
                step: c.step,
                spec: Box::new(c.spec),
            },
            Err(e) => StrandFire::Failed(format!("the dev job did not start: {e}")),
        },
    }
}

#[cfg(test)]
mod tests;
