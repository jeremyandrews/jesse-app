//! **THE WORK VIEW'S FIELDS** — what the app's Work board reads beside the run records and the
//! strand snapshot, derived and never written back: a plain `title` for a Drafts line and for a
//! run, a run's `last_line` and `report_line`, a Drafts line's `startable`, and the one shared
//! `last_look` timestamp.
//!
//! Every function here but the last look store is pure or reads only: no title is ever written
//! into a note, and no model is called.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::{draft_is_studio, stream, RunRecord};

pub mod row;
use crate::strands::{StrandItem, StrandSection};
use crate::{check_auth, ApiError, AppState};

// ---- TITLES ----------------------------------------------------------------------------

/// The longest title before it is cut, in characters (the `…` comes after).
pub const TITLE_MAX: usize = 57;

/// The longest `last_line`, in characters.
pub const LAST_LINE_MAX: usize = 200;

/// The longest `report_line`, in characters.
pub const REPORT_LINE_MAX: usize = 500;

/// A Drafts line's title. `raw` is the line after its checkbox and date: the bold id is
/// removed, then any `(waits on: …)` and the wiki links the line ends on, then the Markdown (emphasis, wiki links kept as their
/// alias or last path segment, `[text](url)` kept as its text, backticks), then the text before
/// the first `": "` is kept, cut at [`TITLE_MAX`] characters on a word boundary.
pub fn line_title(raw: &str) -> String {
    let after_id = match bold_id(raw) {
        Some(after) => after,
        None => raw,
    };
    title_of(drop_trailing_links(&drop_waits_on(after_id)))
}

/// `s` without the wiki links that end it: the pointer to the line's draft, which most lines
/// end on, is where the work lives and not what it is called. A link at the start or in the
/// middle of the words is part of them and stays (as its alias or last path segment).
fn drop_trailing_links(s: &str) -> &str {
    let mut t = s.trim_end();
    while let Some(body) = t.strip_suffix("]]") {
        match body.rfind("[[") {
            Some(open) if !body[..open].trim().is_empty() => t = body[..open].trim_end(),
            _ => break,
        }
    }
    t
}

/// The title rule after the id is gone: plain words, the part before the first `": "`, cut.
pub fn title_of(text: &str) -> String {
    let plain = plain_text(text);
    let head = match plain.split_once(": ") {
        Some((h, _)) if !h.trim().is_empty() => h.trim(),
        _ => plain.trim(),
    };
    cut(head)
}

/// A prompt file's H1 as a title: the first `# ` line, any leading id removed, then
/// [`title_of`]. `None` when the file has no H1 or it is only an id.
pub fn h1_title(src: &str) -> Option<String> {
    let body = strip_front_matter(src);
    let h1 = body
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("# "))?
        .trim();
    let rest = strip_leading_id(h1);
    let t = title_of(rest);
    (!t.is_empty()).then_some(t)
}

/// A branch as a title: hyphens to spaces, the first letter capitalized.
pub fn branch_title(branch: &str) -> String {
    let words = branch
        .split(['-', '_', '/'])
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut cs = words.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().chain(cs).collect(),
        None => String::new(),
    }
}

/// What a run is called on screen. Never its run id: its Drafts line's title when it is a strand
/// step whose line is found, else its prompt file's H1, else its branch.
pub fn run_title(
    r: &RunRecord,
    mut line_of: impl FnMut(&str, &str) -> Option<String>,
    prompt_h1: impl Fn() -> Option<String>,
) -> String {
    if let Some((strand, item)) = r.step() {
        if let Some(t) = line_of(strand, item).filter(|t| !t.is_empty()) {
            return t;
        }
    }
    if let Some(t) = prompt_h1() {
        return t;
    }
    let t = branch_title(&r.branch);
    if t.is_empty() {
        "Untitled run".into()
    } else {
        t
    }
}

/// The text after the first `**bold**` span (the line's id), or `None` with no id.
fn bold_id(s: &str) -> Option<&str> {
    let open = s.find("**")?;
    // An id leads the line: a bold span later on is emphasis, not the id.
    if !s[..open].trim().is_empty() {
        return None;
    }
    let close = s[open + 2..].find("**")? + open + 2;
    Some(&s[close + 2..])
}

/// `s` without any `(waits on: …)` parenthetical.
fn drop_waits_on(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        let tail = &s[i..];
        if let Some(inner_end) = tail.strip_prefix('(').and_then(|t| t.find(')')) {
            let inner = &tail[1..1 + inner_end];
            if inner.trim_start().to_lowercase().starts_with("waits on:") {
                i += inner_end + 2;
                continue;
            }
        }
        let ch = tail.chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Markdown to plain words: wiki links to their alias or last path segment, `[text](url)` to its
/// text, backticks and emphasis markers dropped, whitespace collapsed.
pub fn plain_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        let tail = &s[i..];
        if let Some(end) = tail.strip_prefix("[[").and_then(|t| t.find("]]")) {
            let inner = &tail[2..2 + end];
            out.push_str(&wiki_label(inner));
            i += 2 + end + 2;
            continue;
        }
        if let Some((text, len)) = md_link(tail) {
            out.push_str(text);
            i += len;
            continue;
        }
        if tail.starts_with("**") || tail.starts_with("__") {
            i += 2;
            continue;
        }
        let ch = tail.chars().next().unwrap_or('\0');
        if ch == '`' || ch == '*' {
            i += 1;
            continue;
        }
        if ch == '_' && is_emphasis_underscore(s, i) {
            i += 1;
            continue;
        }
        out.push(ch);
        i += ch.len_utf8();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A wiki link's words: its alias, else its target's last path segment, heading dropped.
fn wiki_label(inner: &str) -> String {
    if let Some((_, alias)) = inner.split_once('|') {
        return alias.trim().to_string();
    }
    let target = inner.split('#').next().unwrap_or(inner);
    target
        .rsplit('/')
        .next()
        .unwrap_or(target)
        .trim()
        .trim_end_matches(".md")
        .to_string()
}

/// `[text](url)` at the start of `s`: the text and the byte length of the whole link.
fn md_link(s: &str) -> Option<(&str, usize)> {
    let rest = s.strip_prefix('[')?;
    let close = rest.find(']')?;
    let text = &rest[..close];
    if text.contains('[') {
        return None;
    }
    let after = rest[close + 1..].strip_prefix('(')?;
    let end = after.find(')')?;
    Some((text, 1 + close + 1 + 1 + end + 1))
}

/// An `_` that opens or closes emphasis: next to a space or an end on one side, a word on the
/// other. One inside a word (`item_background`) is part of the word.
fn is_emphasis_underscore(s: &str, i: usize) -> bool {
    let before = s[..i].chars().next_back();
    let after = s[i + 1..].chars().next();
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    !(word(before) && word(after))
}

/// `s` cut at [`TITLE_MAX`] characters on a word boundary, with `…`, or unchanged when it fits.
/// A single trailing full stop is dropped either way: a title is not a sentence.
fn cut(s: &str) -> String {
    cut_at(s, TITLE_MAX)
}

/// `s` cut at `max` characters on a word boundary, with `…`, or unchanged when it fits. A single
/// trailing full stop is dropped either way.
fn cut_at(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.strip_suffix('.').unwrap_or(s).trim_end().to_string();
    }
    let prefix: String = s.chars().take(max).collect();
    let next = s.chars().nth(max);
    let at_boundary = next.is_some_and(char::is_whitespace);
    let head = if at_boundary {
        prefix.as_str()
    } else {
        match prefix.rfind(char::is_whitespace) {
            Some(sp) if sp > 0 => &prefix[..sp],
            _ => prefix.as_str(),
        }
    };
    let head = head.trim_end_matches(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';' | ':' | '.' | '(' | '-' | '\u{2013}' | '\u{2014}'
            )
    });
    format!("{head}\u{2026}")
}

/// A heading's leading id removed: `GO256 Goose ablation` is `Goose ablation`, as are
/// `[GO256] …`, `GO256: …` and `GO256 — …`.
fn strip_leading_id(h1: &str) -> &str {
    let Some((first, rest)) = h1.split_once(char::is_whitespace) else {
        return h1;
    };
    let token = first
        .trim_start_matches('[')
        .trim_end_matches([']', ':', ','])
        .trim_end_matches(']');
    if looks_like_id(token) {
        rest.trim_start()
            .trim_start_matches(['-', ':', '\u{2013}', '\u{2014}'])
            .trim_start()
    } else {
        h1
    }
}

/// `A1c`, `GO256`, `HO2`, `K04`, `T1b`: one to six letters, digits, then at most two lowercase
/// letters.
fn looks_like_id(t: &str) -> bool {
    let letters = t.chars().take_while(char::is_ascii_alphabetic).count();
    let digits = t[letters..]
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    let suffix = &t[letters + digits..];
    (1..=6).contains(&letters)
        && t[..letters].chars().all(|c| c.is_ascii_uppercase())
        && digits >= 1
        && suffix.len() <= 2
        && suffix.chars().all(|c| c.is_ascii_lowercase())
}

/// A note's body, its YAML front matter removed.
pub fn strip_front_matter(src: &str) -> &str {
    let Some(rest) = src.strip_prefix("---\n") else {
        return src;
    };
    match rest.find("\n---") {
        Some(end) => {
            let after = &rest[end + 4..];
            after.strip_prefix('\n').unwrap_or(after)
        }
        None => src,
    }
}

// ---- LINES FROM A RUN --------------------------------------------------------------------

/// The last non blank readable line of `lines`, at most [`LAST_LINE_MAX`] characters.
pub fn last_line_of(lines: &[String]) -> Option<String> {
    lines
        .iter()
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| clip(l, LAST_LINE_MAX))
}

/// A report's first line: the first non blank line of its body, at most
/// [`REPORT_LINE_MAX`] characters.
pub fn report_line_of(src: &str) -> Option<String> {
    strip_front_matter(src)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| clip(l, REPORT_LINE_MAX))
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('\u{2026}');
    out
}

/// How far back from the end of `stream.jsonl` the last line is looked for first.
const TAIL_WINDOW: u64 = 1 << 20;

/// The readable tail of a run's stream: the last [`super::LOG_DEFAULT`] lines the log route
/// serves, read from the end of the file rather than all of it when the stream is large and
/// still plain. Falls back to the whole stream when that window holds no readable line.
fn tail_lines(run_dir: &Path) -> Vec<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let plain = run_dir.join("stream.jsonl");
    if let Ok(mut f) = std::fs::File::open(&plain) {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len > TAIL_WINDOW && f.seek(SeekFrom::Start(len - TAIL_WINDOW)).is_ok() {
            let mut buf = Vec::new();
            if f.read_to_end(&mut buf).is_ok() {
                let text = String::from_utf8_lossy(&buf);
                // The first line is very likely cut: start after it.
                let whole = text.split_once('\n').map(|(_, r)| r).unwrap_or("");
                let lines = stream::tail_readable(whole, super::LOG_DEFAULT);
                if !lines.is_empty() {
                    return lines;
                }
            }
        }
    }
    stream::tail_readable(&stream::read_stream(run_dir), super::LOG_DEFAULT)
}

/// The stream file a run's last line is read from, with what says whether it changed.
fn stream_stamp(run_dir: &Path) -> Option<(PathBuf, u64, Option<SystemTime>)> {
    ["stream.jsonl", "stream.jsonl.gz"].iter().find_map(|n| {
        let p = run_dir.join(n);
        let m = std::fs::metadata(&p).ok()?;
        Some((p, m.len(), m.modified().ok()))
    })
}

/// One run's remembered `last_line`, and the stream it was read from.
struct SeenLine {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    line: Option<String>,
}

/// `last_line` for every run, remembered per run while its stream is unchanged, so a list
/// polled every 15 seconds reads only the streams that grew (and never unpacks a compressed
/// one twice).
#[derive(Default)]
pub struct LastLines {
    seen: Mutex<HashMap<String, SeenLine>>,
}

impl LastLines {
    pub fn get(&self, run_id: &str, run_dir: &Path) -> Option<String> {
        let (path, len, modified) = stream_stamp(run_dir)?;
        {
            let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = seen.get(run_id) {
                if s.path == path && s.len == len && s.modified == modified {
                    return s.line.clone();
                }
            }
        }
        let line = last_line_of(&tail_lines(run_dir));
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).insert(
            run_id.to_string(),
            SeenLine {
                path,
                len,
                modified,
                line: line.clone(),
            },
        );
        line
    }
}

// ---- STARTABLE ---------------------------------------------------------------------------

/// Whether the app may offer `Start` on a Drafts line, and why not. `runnable` is the line's
/// `studio_runnable` check (its error, when it failed), `draft_src` the linked draft's text
/// when it was read, `live` whether the step has a run the bridge counts live.
pub fn startable(
    item: &StrandItem,
    runnable: &Result<String, String>,
    draft_src: Option<&str>,
    live: bool,
) -> Result<(), String> {
    if let Err(e) = runnable {
        return Err(e.clone());
    }
    if item.section == StrandSection::Later {
        return Err("Under Later.".into());
    }
    if let Some(w) = &item.waits_on {
        return Err(format!("Waits on {w}."));
    }
    if !draft_src.is_some_and(draft_is_studio) {
        return Err("Its draft does not set devjob-host to this bridge's machine.".into());
    }
    if live {
        return Err("Already has a live run.".into());
    }
    Ok(())
}

// ---- LAST LOOK ---------------------------------------------------------------------------

/// The file the shared last look lives in, in the bridge's state directory.
pub const LAST_LOOK_FILE: &str = "work-last-look.json";

fn last_look_path(st: &AppState) -> Option<PathBuf> {
    st.cfg
        .state_dir
        .as_deref()
        .map(|d| Path::new(d).join(LAST_LOOK_FILE))
}

/// The stored last look, RFC 3339, or `None` before the first.
pub fn read_last_look(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("last_look")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Store `at` as the last look.
pub fn write_last_look(path: &Path, at: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json!({ "last_look": at }).to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// What both last look routes answer.
pub fn last_look_json(at: Option<&str>) -> Value {
    json!({ "last_look": at })
}

fn guard(st: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    check_auth(headers, &st.cfg.token)?;
    if !st.limiter.allow() {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded".to_string(),
        ));
    }
    Ok(())
}

/// `GET /jesse/work/last-look`: `{ "last_look": "<RFC 3339>" | null }`.
pub async fn jesse_work_last_look(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard(&st, &headers)?;
    let at = last_look_path(&st).and_then(|p| read_last_look(&p));
    Ok(Json(last_look_json(at.as_deref())).into_response())
}

/// `POST /jesse/work/last-look`: the last look becomes now; answers what `GET` would.
pub async fn jesse_work_last_look_set(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard(&st, &headers)?;
    let Some(path) = last_look_path(&st) else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "this bridge keeps no state directory, so it cannot remember a last look".into(),
        ));
    };
    let now = super::rfc(super::now_ms());
    write_last_look(&path, &now).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the last look could not be stored: {e}"),
        )
    })?;
    Ok(Json(last_look_json(Some(&now))).into_response())
}

#[cfg(test)]
mod tests;
