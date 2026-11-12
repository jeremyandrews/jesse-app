//! **THE WORK BOARD ROW'S WORDS** (0.205.0): two short phrases per strand, derived on read from
//! what the note already says, so a board row can tell the owner what a project is about right
//! now and what, if anything, is his to do.
//!
//! - [`headline`]: the lead clause of the note's `**Now:**`, in plain words.
//! - [`your_action`]: the first action of a `you:` `**Waiting on:**`, in plain words.
//!
//! Both are pure: no model is called, nothing is written back, and a note that says nothing
//! usable gives `None`. Step ids, PR numbers and parentheticals are kept off the phrase, because a
//! row is read at a glance and an id is a label the reader would have to look up. A leading id that
//! names one of the strand's own lines is replaced by that line's title.

use super::{looks_like_id, plain_text};

/// The longest phrase, in characters, before it is cut on a word with `…`. The app fits it to the
/// row's width on a word; this only bounds what travels.
pub const ROW_PHRASE_MAX: usize = 120;

/// The most words an action word after a leading id may have to be moved in front of the line's
/// title (`GO123 merged` → `Merged: <title>`). A longer remainder is a sentence of its own.
const SHORT_REST_WORDS: usize = 4;

/// The lead clause of a `**Now:**`, or `None` when nothing usable is left.
///
/// Plain words (Markdown, links and backticks gone), parentheticals dropped (a bare build number
/// such as `(213)` stays), then one sentence: the first that names `today` (`12 Nov`,
/// `2026-11-12`) if any, else the first of more than one word. A dated sentence is cut only at
/// `; `; any other at `; `, ` — `, `, which `, `, so ` and at `: ` (unless what precedes the colon
/// is one word, such as `Next:`, in which case what follows it is kept). A clause that opens on
/// step ids is reworded around their titles from `lines` (`id`, `title`): `GO123 merged` →
/// `Merged: arm64 Chromium fallback chain` (dates, times and the title's own words leave the verb;
/// `running` reads `Running`, `run is done` reads `Finished`); with a longer remainder the ids
/// are dropped (`K04 round 4 stopped in pre-flight again` → `Round 4 stopped in pre-flight again`).
pub fn headline(now: &str, lines: &[(String, String)], today: &str) -> Option<String> {
    let plain = drop_parens(&lead_id_in_parens(&plain_text(now), lines));
    let all = sentences(&plain);
    let dated = all.iter().position(|x| mentions_day(x, today));
    // The sentence naming today first, then the rest in order. A one word sentence
    // (`trovato.rs.`) and a `No …`/`Nothing …` one say nothing; with nothing better, the first
    // sentence stands.
    let mut order: Vec<usize> = dated.into_iter().collect();
    order.extend((0..all.len()).filter(|i| Some(*i) != dated));
    let mut first = None;
    for i in &order {
        let x = all[*i];
        let lower = x.to_lowercase();
        if x.split_whitespace().count() <= 1
            || lower.starts_with("no ")
            || lower.starts_with("nothing ")
        {
            continue;
        }
        let phrase = phrase_of(x, Some(*i) == dated, lines);
        // A lone verb (`Merged`, from `PR 327 merged`) names nothing: try the next sentence.
        if phrase.split_whitespace().count() >= 2 {
            return finish(&phrase, true);
        }
        first.get_or_insert(phrase);
    }
    finish(
        &first.or_else(|| all.first().map(|x| phrase_of(x, false, lines)))?,
        true,
    )
}

/// One sentence's headline phrase (see [`headline`]).
fn phrase_of(s: &str, dated: bool, lines: &[(String, String)]) -> String {
    // `W32's run is done: "The Bargain", the article …`: the name after the colon is the news.
    let quoted = s.split_once(": ").and_then(|(_, tail)| quoted_name(tail));
    let undated;
    let s = if dated {
        // The day is today, so its name and date say nothing; the time stays.
        undated = without_date(s.split("; ").next().unwrap_or(s));
        undated.as_str()
    } else {
        first_clause(s)
    };
    let (ids, rest) = leading_ids(s, lines);
    if ids.is_empty() {
        return scrub(s, lines);
    }
    let rest = rest
        .trim_start_matches(['\'', '\u{2019}'])
        .trim_start_matches("s ")
        .trim();
    let names: Vec<String> = ids.iter().filter_map(|id| title_for(id, lines)).collect();
    if !names.is_empty() && rest.split_whitespace().count() <= SHORT_REST_WORDS {
        let name = match quoted {
            Some(q) => q,
            None => capitalize(&names.join(" and ")),
        };
        match verb_of(rest, &name) {
            Some(verb) => format!("{verb}: {name}"),
            None => name,
        }
    } else {
        capitalize(&scrub(rest, lines))
    }
}

/// The first action of a `you:` `**Waiting on:**`, or `None` when the gate is not the owner's.
///
/// `you:` (any case) is dropped, then the text is made plain, parentheticals are dropped (a bare
/// build number stays), the first gate is kept (cut at `; `) and its first sentence, a lead in
/// such as `for S23, Part 0:` goes, and so does a trailing `, which …`, `, and …` or `boarded …`.
/// Ids are scrubbed as in [`headline`]: a run of two or more becomes `N items`. A gate that opens
/// on a step's id (`OT2's five questions`) is that step, named from its whole text in `texts`
/// (`id`, `text`) as [`next_title`] names a line. A leading `-ing` verb reads as an
/// instruction (`installing` → `install`) and `whether …` as `decide whether …`. A gate that is a
/// remark about the note rather than an action (`every open line here is … of yours`, `… only you
/// can have`, `… is yours to run`) gives way to `next`, the first open Queue line's title, when
/// there is one. The case is kept as written, so the app can put `You:` in front of it.
pub fn your_action(
    waiting: &str,
    lines: &[(String, String)],
    next: Option<&str>,
    texts: &[(String, String)],
) -> Option<String> {
    let t = waiting.trim();
    let head = t.get(..4)?;
    if !head.eq_ignore_ascii_case("you:") {
        return None;
    }
    let s = drop_parens(&plain_text(&t[4..]));
    let s = s.split("; ").next().unwrap_or("");
    let s = first_sentence(s);
    let s = drop_lead_in(s);
    let s = cut_tail(s);
    // `OT2's five questions`: the step names the action better than what is left of the gate,
    // from its whole text (its title is cut at 57 characters), else its title.
    let (ids, _) = leading_ids(s, lines);
    if ids.len() == 1 {
        let whole = texts
            .iter()
            .find(|(id, _)| *id == ids[0])
            .and_then(|(_, text)| next_title(text, "", lines));
        if let Some(step) = whole.or_else(|| title_for(&ids[0], lines)) {
            return finish(&decapitalize(&step), false);
        }
    }
    let phrase = scrub_action(s, lines);
    let lower = phrase.to_lowercase();
    let remark = [
        "of yours",
        "is yours",
        "are yours",
        "only you can",
        "yours to ",
    ]
    .iter()
    .any(|m| lower.contains(m));
    if remark {
        if let Some(n) = next.and_then(short_title) {
            let n = decapitalize(&scrub(&n, lines));
            // `… is yours to run`: the remark's own verb tells what to do with the line.
            let verb = lower
                .split_once("yours to ")
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .map(|v| v.trim_matches(|c: char| !c.is_alphabetic()).to_string())
                .filter(|v| !v.is_empty());
            return match verb {
                Some(v) if !n.to_lowercase().starts_with(&v) => finish(&format!("{v} {n}"), false),
                _ => finish(&n, false),
            };
        }
    }
    finish(&imperative(&phrase), false)
}

/// The first queued line as the row's Next names it, from its whole `text` rather than its
/// title (which is cut at 57 characters): parentheticals dropped, the first sentence, cut at
/// `; `, then the words before `: ` unless they are two or fewer (`Blog: The Bargain` keeps its
/// tail), ids, PR numbers, branches and paths scrubbed. With no text left, the cleaned `title`.
pub fn next_title(text: &str, title: &str, lines: &[(String, String)]) -> Option<String> {
    let plain = drop_parens(&plain_text(text));
    let sentence = first_sentence(&plain);
    let clause = sentence.split("; ").next().unwrap_or(sentence);
    let clause = match clause.split_once(": ") {
        Some((head, _)) if head.split_whitespace().count() > 2 => head,
        // `Release 0.105.0: tag main after B30 and B33 with S1 to S6`: a tail made of step ids
        // scrubs to nonsense, and the label alone names the step.
        Some((head, tail))
            if tail
                .split(", ")
                .next()
                .unwrap_or(tail)
                .split_whitespace()
                .any(|w| is_id(core(w), lines)) =>
        {
            head
        }
        // Ids later in the tail are cut away with the rest of it.
        Some((_, tail)) => {
            let first = tail.split(", ").next().unwrap_or(tail);
            &clause[..clause.len() - tail.len() + first.len()]
        }
        None => clause,
    };
    if let Some(t) = finish(&scrub(clause, lines), false) {
        return Some(t);
    }
    finish(&scrub(&short_title(title)?, lines), false)
}

/// The verb of a short remainder after leading ids, capitalized, without the dates, times and
/// words the title already says; `None` when nothing is left.
fn verb_of(rest: &str, name: &str) -> Option<String> {
    let lower = rest.to_lowercase();
    if lower.starts_with("running") {
        return Some("Running".into());
    }
    if lower.starts_with("run ") && (lower.contains("done") || lower.contains("finished")) {
        return Some("Finished".into());
    }
    let named: Vec<String> = name
        .split_whitespace()
        .map(|w| {
            w.to_lowercase()
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
        .collect();
    let words: Vec<&str> = rest
        .split_whitespace()
        .filter(|w| *w != "is")
        .filter(|w| !is_date_word(w))
        .filter(|w| {
            let c = w.to_lowercase();
            let c = c.trim_matches(|ch: char| !ch.is_alphanumeric());
            !named.iter().any(|n| n == c)
        })
        .collect();
    let verb = words.join(" ");
    let verb = verb.trim_matches(|c: char| matches!(c, ',' | ':' | ';') || c.is_whitespace());
    (!verb.is_empty()).then(|| capitalize(verb))
}

/// A word that is part of a date or a time: a number, `13:39`, `2026-11-01`, a month or a day.
fn is_date_word(w: &str) -> bool {
    let c = w
        .trim_matches(|ch: char| matches!(ch, ',' | '.' | ';'))
        .to_lowercase();
    if c.chars().any(|ch| ch.is_ascii_digit())
        && c.chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, ':' | '-' | '/'))
    {
        return true;
    }
    MONTHS.iter().any(|(short, long)| c == *short || c == *long)
        || ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
            .iter()
            .any(|d| c == *d)
}

/// `s` without its weekdays, months and the day numbers beside a month: the words of a date,
/// not a time and not a count (`Shipped 12 Nov: 3 fixes` keeps its `3`).
fn without_date(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().collect();
    let clean = |w: &str| {
        w.trim_matches(|ch: char| matches!(ch, ',' | '.' | ';' | ':'))
            .to_lowercase()
    };
    let is_month = |w: &str| {
        let c = clean(w);
        MONTHS.iter().any(|(short, long)| c == *short || c == *long)
    };
    let is_day_number = |w: &str| {
        let c = clean(w);
        !c.is_empty() && c.len() <= 2 && c.chars().all(|ch| ch.is_ascii_digit())
    };
    let mut out: Vec<String> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let c = clean(w);
        let weekday = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"].contains(&c.as_str());
        // `12 Nov` or `Nov 12`, but in `12 Nov: 3 fixes` the `3` is a count.
        let beside_month = is_day_number(w)
            && (words.get(i + 1).is_some_and(|n| is_month(n))
                || (i > 0
                    && is_month(words[i - 1])
                    && !words[i - 1].ends_with(':')
                    && !(i > 1 && is_day_number(words[i - 2]))));
        if weekday || is_month(w) || beside_month {
            // A colon the date carried stays with the words before it.
            if w.ends_with(':') {
                if let Some(prev) = out.last_mut() {
                    if !prev.ends_with(':') {
                        prev.push(':');
                    }
                }
            }
            continue;
        }
        out.push((*w).to_string());
    }
    out.join(" ")
}

/// The name a clause opens on in quotes (`"The Bargain", the article …` → `The Bargain`), or
/// `None`: no quote first, an empty one, or one longer than eight words.
fn quoted_name(s: &str) -> Option<String> {
    let s = s.trim_start();
    let (open, close) = match s.chars().next()? {
        '"' => ('"', '"'),
        '\u{201C}' => ('\u{201C}', '\u{201D}'),
        _ => return None,
    };
    let inner = &s[open.len_utf8()..];
    let end = inner.find(close)?;
    let name = inner[..end].trim();
    let words = name.split_whitespace().count();
    ((1..=8).contains(&words)).then(|| name.to_string())
}

/// `kc1 fixed for good (CL16c): …`: a short lead clause whose parenthetical is one of the
/// strand's step ids reads as if the id led it (`CL16c kc1 fixed for good: …`), so the step's
/// title can name it.
fn lead_id_in_parens(s: &str, lines: &[(String, String)]) -> String {
    let Some(open) = s.find('(') else {
        return s.to_string();
    };
    let Some(close) = s[open..].find(')').map(|c| open + c) else {
        return s.to_string();
    };
    let inner = s[open + 1..close].trim();
    let before = s[..open].trim();
    let Some((_, title)) = lines.iter().find(|(id, _)| id == inner) else {
        return s.to_string();
    };
    if before.is_empty() || before.split_whitespace().count() > 4 || before.contains('.') {
        return s.to_string();
    }
    // `kc1 fixed for good (CL16c)`, CL16c titled `Stop the kc1 crash recurring`: the clause
    // already names the thing, and the title would only add an instruction. Leave it be.
    let title_words: Vec<String> = title.split_whitespace().map(str::to_lowercase).collect();
    if before
        .split_whitespace()
        .any(|w| w.len() > 2 && title_words.contains(&w.to_lowercase()))
    {
        return s.to_string();
    }
    format!("{inner} {before}{}", &s[close + 1..])
}

/// The months, short and long.
const MONTHS: [(&str, &str); 12] = [
    ("jan", "january"),
    ("feb", "february"),
    ("mar", "march"),
    ("apr", "april"),
    ("may", "may"),
    ("jun", "june"),
    ("jul", "july"),
    ("aug", "august"),
    ("sep", "september"),
    ("oct", "october"),
    ("nov", "november"),
    ("dec", "december"),
];

/// Whether a sentence names the day `today` (`YYYY-MM-DD`): as written, or as `12 Nov`,
/// `12 November`, `Nov 12` or `November 12`.
fn mentions_day(s: &str, today: &str) -> bool {
    if !today.is_empty() && s.contains(today) {
        return true;
    }
    let mut parts = today.split('-');
    let (Some(_), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let (Ok(m), Ok(d)) = (m.parse::<usize>(), d.parse::<u32>()) else {
        return false;
    };
    let Some((short, long)) = m.checked_sub(1).and_then(|i| MONTHS.get(i)) else {
        return false;
    };
    let words: Vec<String> = s
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .collect();
    let is_day = |w: &str| w.parse::<u32>().ok() == Some(d);
    let is_month = |w: &str| w == *short || w == *long;
    words
        .windows(2)
        .any(|p| (is_day(&p[0]) && is_month(&p[1])) || (is_month(&p[0]) && is_day(&p[1])))
}

/// Every sentence of `s`, in order (see [`first_sentence`]).
fn sentences(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let first = first_sentence(rest);
        if !first.is_empty() {
            out.push(first);
        }
        let used = first.as_ptr() as usize - rest.as_ptr() as usize + first.len();
        let tail = rest[used.min(rest.len())..]
            .trim_start_matches(['.', '!', '?'])
            .trim_start();
        if tail.len() >= rest.len() {
            break;
        }
        rest = tail;
    }
    out
}

/// A gate as an instruction: an `-ing` verb that opens it or follows `and`, `then` or `or`
/// (`installing X and signing Y` → `install X and sign Y`), and a leading `whether` → `decide
/// whether`.
fn imperative(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len() + 1);
    for (i, w) in words.iter().enumerate() {
        let lower = w.to_lowercase();
        let after_joint = i == 0
            || matches!(
                words[i - 1].trim_end_matches(',').to_lowercase().as_str(),
                "and" | "then" | "or"
            );
        if i == 0 && lower == "whether" {
            out.push("decide".into());
            out.push((*w).into());
            continue;
        }
        match GERUNDS
            .iter()
            .find(|(g, _)| *g == lower)
            .filter(|_| after_joint)
        {
            Some((_, v)) => out.push((*v).into()),
            None => out.push((*w).into()),
        }
    }
    out.join(" ")
}

/// The `-ing` verbs a gate opens on, and the instruction each reads as.
const GERUNDS: &[(&str, &str)] = &[
    ("running", "run"),
    ("installing", "install"),
    ("reviewing", "review"),
    ("deploying", "deploy"),
    ("signing", "sign"),
    ("checking", "check"),
    ("merging", "merge"),
    ("reading", "read"),
    ("writing", "write"),
    ("sending", "send"),
    ("setting", "set"),
    ("creating", "create"),
    ("answering", "answer"),
    ("deciding", "decide"),
    ("filing", "file"),
    ("paying", "pay"),
    ("calling", "call"),
    ("approving", "approve"),
    ("testing", "test"),
    ("trying", "try"),
];

/// `s` with a capitalized first word lowercased (`Review the draft` → `review the draft`), so a
/// title reads after `You:`. A word in capitals or with capitals inside (`SOC`, `GitHub`) stays.
fn decapitalize(s: &str) -> String {
    let first = s.split_whitespace().next().unwrap_or("");
    let mut cs = first.chars();
    let plain_word = cs.next().is_some_and(char::is_uppercase)
        && cs.clone().count() > 0
        && cs.all(|c| c.is_lowercase());
    if !plain_word {
        return s.to_string();
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

// ---- THE PIECES --------------------------------------------------------------------------

/// `s` without its parentheticals, nested ones included, and the space before each. An
/// unclosed `(` drops the rest: it is a parenthetical cut short.
fn drop_parens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < s.len() {
        let tail = &s[i..];
        let ch = tail.chars().next().unwrap_or('\0');
        // `App 1.0 (213)`: a bare build number says which one, so it stays.
        if depth == 0 && ch == '(' {
            if let Some(close) = tail.find(')') {
                let inner = &tail[1..close];
                if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                    out.push_str(&tail[..=close]);
                    i += close + 1;
                    continue;
                }
            }
        }
        match ch {
            '(' => {
                if depth == 0 {
                    let kept = out.trim_end().len();
                    out.truncate(kept);
                }
                depth += 1;
            }
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
        i += ch.len_utf8();
    }
    tidy(&out)
}

/// Whitespace collapsed, and no space left before a comma, a full stop, a colon or a semicolon.
fn tidy(s: &str) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(joined.len());
    for ch in joined.chars() {
        if matches!(ch, ',' | '.' | ':' | ';') && out.ends_with(' ') {
            out.pop();
        }
        out.push(ch);
    }
    out
}

/// Words a full stop follows without ending a sentence.
const ABBREVIATIONS: &[&str] = &[
    "dr", "mr", "mrs", "ms", "st", "vs", "no", "e.g", "i.e", "etc",
];

/// The first sentence of `s`: up to a `.`, `!` or `?` that a space and a letter or a digit
/// follow (`running. tag1.com merged` is two sentences), unless the word before it is an
/// abbreviation. A closing full stop is dropped.
fn first_sentence(s: &str) -> &str {
    let bytes = s.as_bytes();
    for (i, ch) in s.char_indices() {
        if !matches!(ch, '.' | '!' | '?') || bytes.get(i + 1) != Some(&b' ') {
            continue;
        }
        let next = s[i + 2..].chars().next();
        if !next.is_some_and(char::is_alphanumeric) {
            continue;
        }
        let word = s[..i].rsplit(' ').next().unwrap_or("").to_lowercase();
        if ch == '.' && ABBREVIATIONS.contains(&word.as_str()) {
            continue;
        }
        return s[..i].trim();
    }
    s.trim().trim_end_matches(['.', '!']).trim_end()
}

/// The first clause of a sentence, for a headline.
fn first_clause(s: &str) -> &str {
    let mut s = s;
    for stop in ["; ", " \u{2014} ", " - ", ", which ", ", so "] {
        if let Some((head, _)) = s.split_once(stop) {
            s = head;
        }
    }
    match s.split_once(": ") {
        // `Next: the library card …`: a one word label, so the words after it are the clause.
        Some((head, tail)) if head.split_whitespace().count() <= 1 => first_clause(tail),
        Some((head, _)) => head,
        None => s,
    }
}

/// An action's lead in: `for S23, Part 0: the user` is `the user`.
fn drop_lead_in(s: &str) -> &str {
    if let Some((head, tail)) = s.split_once(": ") {
        let lower = head.to_lowercase();
        if lower.starts_with("for ") || head.split_whitespace().count() <= 1 {
            return tail;
        }
    }
    s
}

/// An action without the clause that only says where it is tracked or what it carries.
fn cut_tail(s: &str) -> &str {
    let mut s = s;
    for stop in [
        ", which ",
        ", and ",
        ", then ",
        ", boarded",
        " boarded ",
        ", both boarded",
        ", not boarded",
    ] {
        if let Some(at) = s.find(stop) {
            s = &s[..at];
        }
    }
    s
}

/// Whether `piece` (punctuation and a possessive already trimmed) is a step id: shaped like one,
/// and either one of the strand's own lines or two or more letters long (so `Ablation A2` keeps
/// its `A2` unless `A2` is a line of this strand).
fn is_id(piece: &str, lines: &[(String, String)]) -> bool {
    if !looks_like_id(piece) {
        return false;
    }
    let letters = piece.chars().take_while(char::is_ascii_alphabetic).count();
    letters >= 2 || lines.iter().any(|(id, _)| id == piece)
}

/// A piece's core: trailing punctuation and a possessive `'s` trimmed.
fn core(piece: &str) -> &str {
    let t = piece.trim_end_matches([',', '.', ':', ';']);
    t.strip_suffix("'s")
        .or_else(|| t.strip_suffix("\u{2019}s"))
        .unwrap_or(t)
}

/// The ids a clause opens on (`H2 and H7 merged` → `[H2, H7]`) and the words after them.
fn leading_ids<'a>(s: &'a str, lines: &[(String, String)]) -> (Vec<String>, &'a str) {
    let mut ids = Vec::new();
    let mut rest = s;
    loop {
        let trimmed = rest.trim_start();
        let (piece, after) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        if piece.is_empty() {
            break;
        }
        if piece == "and" && !ids.is_empty() {
            rest = after;
            continue;
        }
        let c = core(piece);
        if !is_id(c, lines) {
            break;
        }
        ids.push(c.to_string());
        let possessive = piece.trim_end_matches([',', '.', ':', ';']).len() > c.len();
        if possessive {
            // `W32's run is done`: the id ends the list, and its `'s` goes with it.
            return (ids, after);
        }
        rest = after;
    }
    if ids.is_empty() {
        (ids, s)
    } else {
        (ids, rest)
    }
}

/// A line's title as a name inside a phrase: its own ids and parentheticals dropped, an
/// ellipsis and trailing punctuation trimmed.
fn title_for(id: &str, lines: &[(String, String)]) -> Option<String> {
    let (_, title) = lines.iter().find(|(i, _)| i == id)?;
    let short = short_title(title)?;
    let t = scrub(&short, lines);
    // `W9 rewrite prompt` without its id is `the rewrite prompt`.
    let led_by_id = short
        .split_whitespace()
        .next()
        .is_some_and(|w| is_id(core(w), lines));
    let t = if led_by_id && !t.is_empty() {
        format!("the {t}")
    } else {
        t
    };
    (!t.is_empty()).then_some(t)
}

/// A served title as a name: parentheticals dropped; one cut at its limit loses its `…`, the
/// clause after its first comma and any small words it ended on (`… so the…` names nothing).
fn short_title(title: &str) -> Option<String> {
    let t = drop_parens(title);
    let cut_short = t.ends_with('\u{2026}');
    let mut t = t.trim_end_matches('\u{2026}').to_string();
    if cut_short {
        if let Some((head, _)) = t.split_once(", ") {
            t = head.to_string();
        }
    }
    let mut words: Vec<&str> = t.split_whitespace().collect();
    while cut_short && words.len() > 1 && words.last().is_some_and(|w| is_filler(w)) {
        words.pop();
    }
    // `… draft and answer` cut short: the half clause after the last `and` goes.
    if cut_short && words.len() > 3 && matches!(words[words.len() - 2], "and" | "or") {
        words.truncate(words.len() - 2);
    }
    let t = words.join(" ");
    let t = t.trim_end_matches([',', ';', ':', '.', ' ']).trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// A preposition an id can hang from (`for GD5`, `on K04`).
fn is_preposition(w: &str) -> bool {
    matches!(
        w.to_lowercase().as_str(),
        "for" | "on" | "in" | "of" | "to" | "at" | "by" | "from" | "with" | "into" | "behind"
    )
}

/// A word a cut title should not end on.
fn is_filler(w: &str) -> bool {
    const FILLER: &[&str] = &[
        "a", "an", "and", "as", "at", "by", "for", "from", "in", "into", "its", "of", "on", "or",
        "the", "their", "to", "with",
    ];
    FILLER.contains(&w.trim_end_matches(',').to_lowercase().as_str())
}

/// Ids and PR numbers out of a phrase. A run of two or more ids (each perhaps with a version,
/// `W9 v2`) becomes `N items`; a lone id goes with its possessive; `PR 828`, `PRs 9 and 10`,
/// `pull request 133` and `PR #317` go entirely.
fn scrub(s: &str, lines: &[(String, String)]) -> String {
    scrub_with(s, lines, true)
}

/// [`scrub`] for an action: a branch name stays, because there it is what the action is on
/// (`branch protection on release-0.19`).
fn scrub_action(s: &str, lines: &[(String, String)]) -> String {
    scrub_with(s, lines, false)
}

fn scrub_with(s: &str, lines: &[(String, String)], branches: bool) -> String {
    let pieces: Vec<&str> = s.split_whitespace().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < pieces.len() {
        let pc = pieces[i];
        // `on release-0.19`: in a headline a branch says only where, so it goes; in an action it
        // is what the action is on, so it reads as the kind of branch it is.
        if pc.eq_ignore_ascii_case("on") && pieces.get(i + 1).is_some_and(|n| is_branch(core(n))) {
            if !branches {
                let kind = if core(pieces[i + 1]).starts_with("release") {
                    "on the release branch"
                } else {
                    "on the branch"
                };
                out.push(kind.to_string());
            }
            carry_punctuation(&mut out, pieces[i + 1]);
            i += 2;
            continue;
        }
        // `on ks1`, `from kc1`: a machine's short name says where, not what.
        if matches!(pc, "on" | "from") && pieces.get(i + 1).is_some_and(|n| is_host(core(n))) {
            carry_punctuation(&mut out, pieces[i + 1]);
            i += 2;
            continue;
        }
        // The board is his: `Jeremy` on it is `you`.
        // Only the name itself (`Jeremy`, `Jeremy's`, `Jeremy,`), never a word that starts with it.
        if let Some(rest) = pc.strip_prefix("Jeremy").filter(|r| {
            r.starts_with('\'')
                || r.starts_with('\u{2019}')
                || r.chars().all(|c| !c.is_alphanumeric())
        }) {
            let you = if out.is_empty() { "You" } else { "you" };
            let rest = match rest {
                "'s" | "\u{2019}s" => "r".to_string(),
                r if r.starts_with("'s") || r.starts_with("\u{2019}s") => {
                    format!(
                        "r{}",
                        &r[r.char_indices().nth(2).map_or(r.len(), |(i, _)| i)..]
                    )
                }
                r => r.to_string(),
            };
            let possessive = rest.starts_with('r');
            out.push(format!("{you}{rest}"));
            i += 1;
            // `Jeremy calls` → `you call`.
            if !possessive {
                if let Some(verb) = pieces.get(i).and_then(|v| agree_with_you(v)) {
                    out.push(verb);
                    i += 1;
                }
            }
            continue;
        }
        // `to ~/jesse/Code/x`: where a clone lands is not the action.
        if matches!(pc, "to" | "into") && pieces.get(i + 1).is_some_and(|n| is_path(core(n))) {
            carry_punctuation(&mut out, pieces[i + 1]);
            i += 2;
            continue;
        }
        // A path reads as its file name, a URL as its host.
        if is_path(core(pc)) || core(pc).contains("://") {
            let c = core(pc);
            let short = if let Some((_, rest)) = c.split_once("://") {
                rest.split('/').next().unwrap_or(rest)
            } else {
                c.trim_end_matches('/').rsplit('/').next().unwrap_or(c)
            };
            let tail = &pc[c.len()..];
            out.push(format!("{short}{tail}"));
            i += 1;
            continue;
        }
        // A commit hash, with its possessive.
        if is_hash(core(pc)) {
            carry_punctuation(&mut out, pc);
            i += 1;
            continue;
        }
        // A PR reference.
        let lower = pieces[i].to_lowercase();
        let pr_words = if lower == "pr" || lower == "prs" {
            Some(1)
        } else if lower == "pull"
            && pieces.get(i + 1).is_some_and(|t| {
                let t = t.to_lowercase();
                t == "request" || t == "requests"
            })
        {
            Some(2)
        } else {
            None
        };
        if let Some(n) = pr_words {
            let mut j = i + n;
            let mut numbers = 0;
            while let Some(t) = pieces.get(j) {
                let c = core(t).trim_start_matches('#');
                if !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit()) {
                    numbers += 1;
                    j += 1;
                    if t.ends_with(',') && pieces.get(j) != Some(&"and") {
                        continue;
                    }
                } else if *t == "and"
                    && numbers > 0
                    && pieces.get(j + 1).is_some_and(|n| {
                        let c = core(n).trim_start_matches('#');
                        !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit())
                    })
                {
                    // `PRs 9 and 10`; but in `PR 5 and deploy` the `and` is the sentence's.
                    j += 1;
                } else {
                    break;
                }
            }
            if numbers > 0 {
                let last = pieces[j - 1];
                if core(last).len() < last.trim_end_matches([',', '.', ':', ';']).len() {
                    // `now that PR 1566's prompt is live` → `now that the prompt is live`.
                    out.push("the".into());
                } else {
                    carry_punctuation(&mut out, last);
                }
                i = j;
                continue;
            }
        }
        // A run of ids.
        let mut j = i;
        let mut count = 0;
        let mut last = i;
        let mut run_ids: Vec<&str> = Vec::new();
        while let Some(t) = pieces.get(j) {
            if is_id(core(t), lines) {
                count += 1;
                run_ids.push(core(t));
                last = j;
                j += 1;
                // A version after an id (`W9 v2`) belongs to it.
                if let Some(v) = pieces.get(j) {
                    let vc = core(v);
                    if vc.len() > 1
                        && vc.starts_with('v')
                        && vc[1..].chars().all(|c| c.is_ascii_digit())
                    {
                        last = j;
                        j += 1;
                    }
                }
                if pieces[last].ends_with(',') {
                    continue;
                }
                if pieces.get(j) == Some(&"and")
                    && pieces.get(j + 1).is_some_and(|n| is_id(core(n), lines))
                {
                    j += 1;
                    continue;
                }
                break;
            } else {
                break;
            }
        }
        if count >= 2 {
            // `review W33, W23 and W9 v2` → `review 3 drafts`, or `3 Blog drafts` when every one
            // of them is titled `Blog`: a line of a note's `## Drafts` is a draft.
            let titles: Vec<Option<String>> = run_ids
                .iter()
                .map(|id| {
                    lines
                        .iter()
                        .find(|(i, _)| i == id)
                        .and_then(|(_, t)| short_title(t))
                })
                .collect();
            let shared = match titles.first() {
                Some(Some(t))
                    if t.split_whitespace().count() <= 2
                        && titles.iter().all(|x| x.as_deref() == Some(t.as_str())) =>
                {
                    Some(t.clone())
                }
                _ => None,
            };
            match shared {
                Some(t) => out.push(format!("{count} {t} drafts")),
                None => out.push(format!("{count} drafts")),
            }
            carry_punctuation(&mut out, pieces[last]);
            i = last + 1;
            continue;
        }
        if count == 1 {
            // `a branch for GD5` at the end of a clause: the preposition goes with the id.
            let clause_ends = last + 1 == pieces.len()
                || pieces[last].ends_with([',', ';', ':', '.'])
                || is_filler(pieces[last + 1]);
            if clause_ends && out.last().is_some_and(|w| is_preposition(w)) {
                out.pop();
            }
            carry_punctuation(&mut out, pieces[last]);
            i = last + 1;
            continue;
        }
        out.push(pieces[i].to_string());
        i += 1;
    }
    tidy(&out.join(" "))
        .trim_start_matches([',', ';', ':', ' '])
        .to_string()
}

/// `release-0.19`, `hotfix-2`: letters, a hyphen, then a version.
fn is_branch(t: &str) -> bool {
    let Some((name, ver)) = t.split_once('-') else {
        return false;
    };
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_lowercase())
        && ver.chars().next().is_some_and(|c| c.is_ascii_digit())
        && ver.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// A third person verb after `you` in its plain form (`calls` → `call`, `has` → `have`), or
/// `None` when the word is not one.
fn agree_with_you(w: &str) -> Option<String> {
    let c = core(w);
    if c.is_empty() || !c.chars().all(|ch| ch.is_ascii_lowercase()) || !c.ends_with('s') {
        return None;
    }
    let tail = &w[c.len()..];
    let plain = match c {
        "is" => "are".to_string(),
        "has" => "have".to_string(),
        "was" => "were".to_string(),
        "does" => "do".to_string(),
        "goes" => "go".to_string(),
        _ if c.ends_with("ss") => return None,
        _ if ["shes", "ches", "xes", "sses"]
            .iter()
            .any(|e| c.ends_with(e)) =>
        {
            c[..c.len() - 2].to_string()
        }
        _ => c[..c.len() - 1].to_string(),
    };
    Some(format!("{plain}{tail}"))
}

/// `ks1`, `kc1`: a machine's short name, two or three lowercase letters then a digit or two.
fn is_host(t: &str) -> bool {
    let letters = t.chars().take_while(char::is_ascii_lowercase).count();
    let digits = t.len() - letters;
    (2..=3).contains(&letters)
        && (1..=2).contains(&digits)
        && t[letters..].chars().all(|c| c.is_ascii_digit())
}

/// `~/x/y` or `/x/y`: a file system path.
fn is_path(t: &str) -> bool {
    (t.starts_with("~/") || (t.starts_with('/') && t.len() > 1 && t[1..].contains('/')))
        && !t.contains(' ')
}

/// `2b908fb`: seven to forty hex digits, at least one of them a letter and one a digit.
fn is_hash(t: &str) -> bool {
    (7..=40).contains(&t.len())
        && t.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars().any(|c| c.is_ascii_alphabetic())
}

/// A dropped piece's trailing comma, colon or semicolon kept on the word before it, so the
/// sentence around it still reads.
fn carry_punctuation(out: &mut [String], dropped: &str) {
    if let Some(p) = dropped
        .chars()
        .last()
        .filter(|c| matches!(c, ',' | ';' | ':'))
    {
        if let Some(prev) = out.last_mut() {
            if !prev.ends_with([',', ';', ':']) {
                prev.push(p);
            }
        }
    }
}

/// `s` with its first letter capitalized, when its first word is plain lowercase letters
/// (`merged` → `Merged`; `kc1` and `tag1.com` stay as written).
fn capitalize(s: &str) -> String {
    let first = s.split_whitespace().next().unwrap_or("");
    if first.is_empty() || !first.chars().all(|c| c.is_alphabetic() && c.is_lowercase()) {
        return s.to_string();
    }
    let mut cs = s.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().chain(cs).collect(),
        None => String::new(),
    }
}

/// The finished phrase: ends trimmed of punctuation, capitalized when asked, cut at
/// [`ROW_PHRASE_MAX`] on a word, `None` when empty.
fn finish(s: &str, cap: bool) -> Option<String> {
    let s = s
        .trim()
        .trim_matches(|c: char| {
            matches!(c, ',' | ';' | ':' | '-' | '\u{2014}') || c.is_whitespace()
        })
        .trim_end_matches(['.', '!'])
        .trim();
    if s.is_empty() {
        return None;
    }
    let s = if cap { capitalize(s) } else { s.to_string() };
    Some(super::cut_at(&s, ROW_PHRASE_MAX))
}

#[cfg(test)]
mod tests;
