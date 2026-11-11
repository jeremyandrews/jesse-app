//! **THE OUTBOUND CONTENT CHECK** — canaries and vault fingerprints, read off every inspected
//! request before it leaves.
//!
//! The egress proxy's tier and send list decide WHERE a request may go. This decides whether
//! what it carries is the vault: a turn with the vault in its context and an open web in front
//! of it can paste a note into a query string as easily as into a POST. Two signals:
//!
//!   * **Canaries** ([`canary_new`]): notes the operator plants in the vault, with a plausible
//!     title ("Gate code and alarm PIN") and a random marker no real text contains. Nothing
//!     legitimate ever sends that marker anywhere, so ONE occurrence, in any encoding, is a
//!     refusal. Recorded in `<state_dir>/canaries.json` (`0600`), re-read when it changes.
//!   * **Fingerprints** ([`FingerprintIndex`]): a sampled set of hashes of every run of
//!     [`WINDOW_WORDS`] consecutive words in the vault's text files. A request is refused when
//!     at least `[egress] fingerprint_threshold` distinct sampled windows of its text are in
//!     the set: a paragraph of a note matches, a sentence that happens to share a phrase
//!     does not.
//!
//! ---- WHAT IS CHECKED, AND IN WHICH FORMS -------------------------------------------
//!
//! The URL's path and query, every header value except the credential headers and `host`,
//! and the body; each in every form [`candidate_forms`] produces (raw, percent/form-decoded,
//! and for each long token its base64, base64url, hex and percent decodings). The decoding is
//! one level deep by design: the threat is a model reaching for the obvious encoding, not an
//! adversary layering ciphers, and every level multiplies the work done on every request.
//!
//! ---- WHAT IT NEVER DOES --------------------------------------------------------------
//!
//! It never logs, returns or stores the text it matched. A [`ContentVerdict`] is a bare enum:
//! the refusal says THAT the request carried vault content, never WHICH.
//!
//! ---- WHY LINKS ARE STRIPPED FIRST --------------------------------------------------
//!
//! The vault is full of links, and a turn following one is the most ordinary thing a turn
//! does. If a URL's words were fingerprinted, fetching a link a note mentions would look like
//! quoting the note. So `http://`, `https://` and `www.` tokens are cut (to the next
//! whitespace) from both sides BEFORE words are split, and the request URL is checked without
//! its scheme and host.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use base64::Engine;
use serde::{Deserialize, Serialize};

/// Words per fingerprint window.
///
/// EIGHT, because it sits between the two failure modes. Shorter windows (4-5 words) are
/// stock phrases ("at the end of the", "let me know if you") that every English text shares
/// with a 20-million-word vault, so unrelated prose would match. Longer windows (12+) make a
/// lightly edited paraphrase, or a short leaked sentence, produce no whole window at all. An
/// 8-word run is rare enough by chance that a match means copying, and a 40-word paragraph
/// still yields ~33 windows, ~8 of them sampled: comfortably over a threshold of 3.
pub const WINDOW_WORDS: usize = 8;

/// One window in this many is kept (by its hash, so both sides keep the SAME windows).
/// Quarters the index's memory; a copied paragraph still carries enough sampled windows.
pub const SAMPLE_MOD: u64 = 4;

/// Tokens strictly longer than this are tried as base64, base64url, hex and percent-encoded.
/// Shorter ones are left alone: a 24-character token decodes to at most 18 bytes, under three
/// words, which can carry neither a fingerprint window nor (in hex or base64) a canary.
pub const DECODE_MIN_TOKEN: usize = 24;

/// Vault files larger than this are not indexed (an export or a log, not a note).
const MAX_INDEXED_FILE: u64 = 8 * 1024 * 1024;

/// The content check's answer. Carries no content, by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentVerdict {
    Clean,
    /// A canary marker, in some form.
    Canary,
    /// At least the threshold of vault fingerprint windows.
    ContentMatch,
}

// ---- CANARIES ------------------------------------------------------------------------

/// One planted canary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanaryRecord {
    /// The marker string the note carries.
    pub marker: String,
    /// The note, vault-relative.
    pub path: String,
    /// Unix milliseconds.
    pub created: u64,
}

const CANARY_FILE: &str = "canaries.json";

/// Titles a canary note may carry: the kind of note a curious turn would open and a careless
/// one would quote.
const CANARY_TITLES: &[&str] = &[
    "Access codes — storage unit",
    "Recovery phrases (old wallet)",
    "Gate code and alarm PIN",
    "Backup codes — personal email",
    "Safe combination",
    "Router admin login",
    "Old bank PINs",
];

fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    use ring::rand::SecureRandom;
    let mut b = [0u8; N];
    ring::rand::SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| "no system randomness".to_string())?;
    Ok(b)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read the canary records (an absent file is none).
pub fn canaries_load(state_dir: &Path) -> Result<Vec<CanaryRecord>, String> {
    match std::fs::read(state_dir.join(CANARY_FILE)) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{CANARY_FILE}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{CANARY_FILE}: {e}")),
    }
}

fn canaries_store(state_dir: &Path, list: &[CanaryRecord]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = state_dir.join(CANARY_FILE);
    let tmp = state_dir.join(format!(".{CANARY_FILE}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let text = serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("{CANARY_FILE}: {e}"))?;
    f.write_all(&text).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{CANARY_FILE}: {e}"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())
}

/// Plant a canary: write one Markdown note at `<vault_root>/<rel_path>` carrying a fresh
/// marker, and record it in `<state_dir>/canaries.json`. Refuses an absolute path, a path
/// with `..`, a path that is not `.md`, and a file that already exists.
///
/// The operator runs this; the bridge never plants canaries on its own.
pub fn canary_new(
    state_dir: &Path,
    vault_root: &Path,
    rel_path: &str,
) -> Result<CanaryRecord, String> {
    let rel = Path::new(rel_path);
    if rel_path.is_empty() || rel.is_absolute() {
        return Err(format!("{rel_path:?} must be a vault-relative path"));
    }
    if !rel
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!("{rel_path:?} may not leave the vault"));
    }
    if rel.extension().and_then(|e| e.to_str()) != Some("md") {
        return Err(format!("{rel_path:?} is not a Markdown note (.md)"));
    }
    let target = vault_root.join(rel);
    if target.exists() {
        return Err(format!(
            "{rel_path:?} already exists; a canary never overwrites"
        ));
    }
    let r = random_bytes::<17>()?;
    let marker = format!("jc-{}", hex(&r[..16]));
    let title = CANARY_TITLES[r[16] as usize % CANARY_TITLES.len()];
    let body = format!(
        "# {title}\n\nKept here so I stop losing it. Do not share.\n\n- code: {marker}\n- \
         noted: {}\n",
        chrono::Local::now().format("%Y-%m-%d")
    );
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| format!("{rel_path:?}: {e}"))?;
        f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    }
    let mut list = canaries_load(state_dir)?;
    let record = CanaryRecord {
        marker,
        path: rel_path.to_string(),
        created: crate::activity::now_ms(),
    };
    list.push(record.clone());
    canaries_store(state_dir, &list)?;
    Ok(record)
}

// ---- NORMALISATION AND HASHING ---------------------------------------------------------

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// splitmix64's finaliser: FNV's low bits are weak, and the sample is taken on them.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The part of a whitespace token before any link in it (`http://`, `https://`, or `www.` at
/// the start or after a non-alphanumeric, as in `[text](www.x)`).
fn strip_link(tok: &str) -> &str {
    let lower = tok.to_ascii_lowercase();
    let mut cut = tok.len();
    for pat in ["http://", "https://"] {
        if let Some(i) = lower.find(pat) {
            cut = cut.min(i);
        }
    }
    let b = lower.as_bytes();
    let mut from = 0;
    while let Some(i) = lower[from..].find("www.") {
        let at = from + i;
        if at == 0 || !b[at - 1].is_ascii_alphanumeric() {
            cut = cut.min(at);
            break;
        }
        from = at + 4;
    }
    &tok[..cut]
}

/// The normalised words of `text`, each as its FNV-1a hash: links cut, lowercased, every
/// non-alphanumeric a separator. Hashes rather than strings, so a vault's worth is 8 bytes a
/// word and transient.
fn word_hashes(text: &[u8]) -> Vec<u64> {
    let s = String::from_utf8_lossy(text);
    let mut out = Vec::new();
    for tok in s.split_whitespace() {
        let mut h = FNV_OFFSET;
        let mut len = 0usize;
        for c in strip_link(tok).chars().flat_map(char::to_lowercase) {
            if c.is_alphanumeric() {
                let mut buf = [0u8; 4];
                for &byte in c.encode_utf8(&mut buf).as_bytes() {
                    h = (h ^ byte as u64).wrapping_mul(FNV_PRIME);
                }
                len += 1;
            } else if len > 0 {
                out.push(h);
                h = FNV_OFFSET;
                len = 0;
            }
        }
        if len > 0 {
            out.push(h);
        }
    }
    out
}

/// Every sampled window hash of `text`, in order (duplicates included).
fn sampled_windows(text: &[u8], mut each: impl FnMut(u64)) {
    let words = word_hashes(text);
    if words.len() < WINDOW_WORDS {
        return;
    }
    for w in words.windows(WINDOW_WORDS) {
        let mut h = FNV_OFFSET;
        for &x in w {
            h = (h ^ x).wrapping_mul(FNV_PRIME);
        }
        let h = mix(h);
        if h.is_multiple_of(SAMPLE_MOD) {
            each(h);
        }
    }
}

// ---- THE FINGERPRINT INDEX ---------------------------------------------------------------

/// The sampled window hashes of every text file in the vault.
///
/// A SORTED, DEDUPLICATED `Vec<u64>` rather than a `HashSet`: the real vault (~20M words)
/// gives ~5M sampled windows, which is 40 MB here against roughly twice that in a hash set's
/// buckets, and a binary search over 5M entries is ~23 comparisons, nothing beside a request.
#[derive(Clone, Default)]
pub struct FingerprintIndex {
    hashes: Vec<u64>,
    files: usize,
}

impl std::fmt::Debug for FingerprintIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FingerprintIndex")
            .field("windows", &self.hashes.len())
            .field("files", &self.files)
            .finish()
    }
}

impl FingerprintIndex {
    /// Index `.md` and `.txt` files under `vault_root`, skipping `.git` and every hidden
    /// directory. BLOCKING (it reads the whole vault): run it on `spawn_blocking`. An
    /// unreadable file or directory is skipped, not fatal.
    pub fn build(vault_root: &Path) -> FingerprintIndex {
        let mut hashes = Vec::new();
        let mut files = 0usize;
        let mut stack = vec![vault_root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') {
                    continue;
                }
                let Ok(ft) = e.file_type() else { continue };
                let path = e.path();
                if ft.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !ft.is_file() {
                    continue;
                }
                let lower = name.to_ascii_lowercase();
                if !(lower.ends_with(".md") || lower.ends_with(".txt")) {
                    continue;
                }
                if e.metadata()
                    .map(|m| m.len() > MAX_INDEXED_FILE)
                    .unwrap_or(true)
                {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                files += 1;
                sampled_windows(&bytes, |h| hashes.push(h));
            }
        }
        hashes.sort_unstable();
        hashes.dedup();
        hashes.shrink_to_fit();
        FingerprintIndex { hashes, files }
    }

    /// How many sampled windows the index holds.
    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }

    /// How many files were indexed.
    pub fn files(&self) -> usize {
        self.files
    }

    fn contains(&self, h: u64) -> bool {
        self.hashes.binary_search(&h).is_ok()
    }

    fn collect_matches(&self, text: &[u8], into: &mut HashSet<u64>) {
        sampled_windows(text, |h| {
            if self.contains(h) {
                into.insert(h);
            }
        });
    }

    /// The number of DISTINCT sampled windows of `text` (normalised) found in the index.
    pub fn count_matches(&self, text: &[u8]) -> usize {
        let mut found = HashSet::new();
        self.collect_matches(text, &mut found);
        found.len()
    }
}

/// How many of `texts` the index would refuse at `threshold`: `(hits, total)`. For measuring
/// the false-positive rate against a corpus of ordinary outbound text.
pub fn measure_false_positives(
    index: &FingerprintIndex,
    threshold: usize,
    texts: &[Vec<u8>],
) -> (usize, usize) {
    let hits = texts
        .iter()
        .filter(|t| index.count_matches(t) >= threshold)
        .count();
    (hits, texts.len())
}

// ---- DECODED FORMS ---------------------------------------------------------------------

/// Percent-decode `b` (`+` as a space when `plus` is set, as in a form body). `None` when
/// nothing would change.
fn percent_decode(b: &[u8], plus: bool) -> Option<Vec<u8>> {
    if !b.contains(&b'%') && !(plus && b.contains(&b'+')) {
        return None;
    }
    let val = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match (val(b[i + 1]), val(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' if plus => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    (out != b).then_some(out)
}

fn hex_decode(t: &[u8]) -> Option<Vec<u8>> {
    if !t.len().is_multiple_of(2) || !t.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let val = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
    Some(t.chunks(2).map(|p| val(p[0]) << 4 | val(p[1])).collect())
}

fn b64_engines() -> [base64::engine::GeneralPurpose; 2] {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    let cfg = GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    [
        GeneralPurpose::new(&base64::alphabet::STANDARD, cfg),
        GeneralPurpose::new(&base64::alphabet::URL_SAFE, cfg),
    ]
}

/// The decodings of one long token: base64, base64url, hex, percent.
fn decode_token(t: &[u8], out: &mut Vec<Vec<u8>>) {
    for engine in b64_engines() {
        if let Ok(d) = engine.decode(t) {
            if !d.is_empty() {
                out.push(d);
            }
        }
    }
    if let Some(d) = hex_decode(t) {
        out.push(d);
    }
}

fn is_token_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"+/=_%.-".contains(&c)
}

/// Every form `raw` is checked in: the raw bytes; the percent/form-decoded whole; and for
/// every token longer than [`DECODE_MIN_TOKEN`] (split on bytes outside
/// `[A-Za-z0-9+/=_%.-]`) its base64, base64url, hex and percent decodings, including a
/// base64 or hex token that was itself percent-encoded (`%2B`, `%3D`), and the value after a
/// `key=` prefix (a query parameter is one token, `=` being a base64 byte).
pub fn candidate_forms(raw: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![raw.to_vec()];
    if let Some(d) = percent_decode(raw, true) {
        out.push(d);
    }
    for tok in raw.split(|c| !is_token_byte(*c)) {
        if tok.len() <= DECODE_MIN_TOKEN {
            continue;
        }
        let mut variants: Vec<&[u8]> = vec![tok];
        if let Some(eq) = tok.iter().position(|&c| c == b'=') {
            let value = &tok[eq + 1..];
            if value.len() > DECODE_MIN_TOKEN {
                variants.push(value);
            }
        }
        for v in variants {
            if let Some(p) = percent_decode(v, false) {
                decode_token(&p, &mut out);
                out.push(p);
            }
            decode_token(v, &mut out);
        }
    }
    out
}

// ---- THE CHECK -------------------------------------------------------------------------

#[derive(Default)]
struct CanaryCache {
    mtime: Option<SystemTime>,
    markers: Vec<Vec<u8>>,
}

/// The content check the egress proxy holds: canaries (re-read when `canaries.json`
/// changes) and the fingerprint index (swapped in whole by [`spawn_index_task`]).
pub struct ContentCheck {
    state_dir: PathBuf,
    canaries: Mutex<CanaryCache>,
    index: Arc<RwLock<Option<FingerprintIndex>>>,
    threshold: usize,
}

impl std::fmt::Debug for ContentCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentCheck")
            .field("threshold", &self.threshold)
            .finish_non_exhaustive()
    }
}

impl ContentCheck {
    /// A check reading canaries under `state_dir`, refusing at `threshold` fingerprint
    /// windows (at least 1). No index until one is built or set.
    pub fn new(state_dir: &Path, threshold: usize) -> ContentCheck {
        ContentCheck {
            state_dir: state_dir.to_path_buf(),
            canaries: Mutex::new(CanaryCache::default()),
            index: Arc::new(RwLock::new(None)),
            threshold: threshold.max(1),
        }
    }

    /// The shared index slot.
    pub fn index(&self) -> Arc<RwLock<Option<FingerprintIndex>>> {
        self.index.clone()
    }

    /// Swap in a freshly built index.
    pub fn set_index(&self, index: FingerprintIndex) {
        *self.index.write().unwrap_or_else(|p| p.into_inner()) = Some(index);
    }

    /// The current canary markers, lowercased, re-read when the file's mtime moves. A file
    /// that will not parse keeps the last good list (and says so, without its contents).
    fn markers(&self) -> Vec<Vec<u8>> {
        let mut c = self.canaries.lock().unwrap_or_else(|p| p.into_inner());
        let mtime = std::fs::metadata(self.state_dir.join(CANARY_FILE))
            .and_then(|m| m.modified())
            .ok();
        if mtime != c.mtime {
            match canaries_load(&self.state_dir) {
                Ok(list) => {
                    c.markers = list
                        .into_iter()
                        .filter(|r| !r.marker.is_empty())
                        .map(|r| r.marker.to_ascii_lowercase().into_bytes())
                        .collect();
                    c.mtime = mtime;
                }
                Err(_) => eprintln!("egress content check: {CANARY_FILE} does not parse"),
            }
        }
        c.markers.clone()
    }

    /// Check one outbound request. `url` is the full request URL (its scheme and host are
    /// not checked: fetching a link is not quoting it); `headers` are every request header,
    /// of which the credential headers and `host` are skipped; `body` is the whole body.
    pub fn check(&self, url: &str, headers: &[(String, Vec<u8>)], body: &[u8]) -> ContentVerdict {
        let mut parts: Vec<&[u8]> = vec![url_tail(url).as_bytes(), body];
        for (name, value) in headers {
            let n = name.to_ascii_lowercase();
            if n == "host" || super::CREDENTIAL_HEADERS.contains(&n.as_str()) {
                continue;
            }
            parts.push(value);
        }
        let forms: Vec<Vec<u8>> = parts
            .iter()
            .filter(|p| !p.is_empty())
            .flat_map(|p| candidate_forms(p))
            .collect();
        let markers = self.markers();
        if !markers.is_empty()
            && forms.iter().any(|f| {
                let lower = f.to_ascii_lowercase();
                markers.iter().any(|m| contains(&lower, m))
            })
        {
            return ContentVerdict::Canary;
        }
        let guard = self.index.read().unwrap_or_else(|p| p.into_inner());
        let Some(index) = guard.as_ref() else {
            return ContentVerdict::Clean;
        };
        let mut found = HashSet::new();
        for f in &forms {
            index.collect_matches(f, &mut found);
            if found.len() >= self.threshold {
                return ContentVerdict::ContentMatch;
            }
        }
        ContentVerdict::Clean
    }
}

/// A URL without its scheme and authority: the path and query.
fn url_tail(url: &str) -> &str {
    match url.find("://") {
        Some(i) => {
            let rest = &url[i + 3..];
            rest.find(['/', '?']).map(|j| &rest[j..]).unwrap_or("")
        }
        None => url,
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && hay.len() >= needle.len()
        && hay.windows(needle.len()).any(|w| w == needle)
}

/// Keep `check`'s index current: build it now, then again every `interval` and whenever the
/// returned `Notify` is notified (the bridge notifies after each commit). Builds run on
/// `spawn_blocking`; the old index serves until the new one is swapped in. A notification
/// that arrives mid-build is kept (a `Notify` permit) and starts the next build at once.
pub fn spawn_index_task(
    check: Arc<ContentCheck>,
    vault_root: PathBuf,
    interval: Duration,
) -> (tokio::task::JoinHandle<()>, Arc<tokio::sync::Notify>) {
    let notify = Arc::new(tokio::sync::Notify::new());
    let n = notify.clone();
    let handle = tokio::spawn(async move {
        loop {
            let root = vault_root.clone();
            match tokio::task::spawn_blocking(move || FingerprintIndex::build(&root)).await {
                Ok(index) => check.set_index(index),
                Err(e) => eprintln!("egress content check: the index build failed: {e}"),
            }
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = n.notified() => {}
            }
        }
    });
    (handle, notify)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::proxy::ca::tests::tmp;
    use std::os::unix::fs::PermissionsExt;

    /// A paragraph only the fixture vault has.
    pub(crate) const PARAGRAPH: &str = "On the third Tuesday the harbour master finally agreed \
        that the blue rowing boat could stay moored beside the old fish market, provided \
        that Marta repainted the hull before the autumn storms and kept the oars locked in \
        the shed behind the bakery on Via Garibaldi.";

    pub(crate) const LINK: &str =
        "https://example.com/some/long/path/with/many/words/in/it/for/a/test?x=1";

    pub(crate) fn fixture_vault() -> PathBuf {
        let v = tmp("vault");
        std::fs::create_dir_all(v.join("Journal")).unwrap();
        std::fs::create_dir_all(v.join(".git")).unwrap();
        std::fs::write(
            v.join("Journal/2024-10-01.md"),
            format!("# Harbour\n\n{PARAGRAPH}\n\nSee {LINK} for the permit.\n"),
        )
        .unwrap();
        std::fs::write(
            v.join(".git/secret.md"),
            "this hidden directory text must never be indexed at all ever again ok",
        )
        .unwrap();
        std::fs::write(v.join("image.png"), [0u8, 1, 2, 3]).unwrap();
        v
    }

    fn b64(s: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    fn b64url(s: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
    }

    fn pct(s: &[u8]) -> String {
        s.iter().map(|b| format!("%{b:02X}")).collect()
    }

    #[test]
    fn canary_new_refuses_escapes_and_overwrites_and_records_privately() {
        let state = tmp("canary-state");
        let vault = tmp("canary-vault");
        assert!(canary_new(&state, &vault, "../x.md").is_err());
        assert!(canary_new(&state, &vault, "a/../../x.md").is_err());
        assert!(canary_new(&state, &vault, "/etc/x.md").is_err());
        assert!(canary_new(&state, &vault, "notes.txt").is_err());
        std::fs::write(vault.join("exists.md"), "mine").unwrap();
        assert!(canary_new(&state, &vault, "exists.md").is_err());
        assert_eq!(
            std::fs::read_to_string(vault.join("exists.md")).unwrap(),
            "mine"
        );

        let r = canary_new(&state, &vault, "Private/codes.md").unwrap();
        assert!(r.marker.starts_with("jc-") && r.marker.len() == 35);
        assert!(r.marker[3..].bytes().all(|b| b.is_ascii_hexdigit()));
        let note = std::fs::read_to_string(vault.join("Private/codes.md")).unwrap();
        assert!(note.contains(&r.marker) && note.starts_with("# "));
        let file = state.join(CANARY_FILE);
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let r2 = canary_new(&state, &vault, "Private/more.md").unwrap();
        assert_eq!(canaries_load(&state).unwrap(), vec![r, r2]);
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_canary_is_found_in_every_form_of_a_query_and_a_body() {
        let state = tmp("canary-forms");
        let vault = tmp("canary-forms-vault");
        let r = canary_new(&state, &vault, "codes.md").unwrap();
        let check = ContentCheck::new(&state, 3);
        let m = r.marker.as_bytes();
        let hexed = hex(m);
        let forms = [
            r.marker.clone(),
            b64(m),
            b64url(m),
            hexed.clone(),
            pct(m),
            // A percent-encoded base64 token, padding and all.
            b64(format!("note {}", r.marker).as_bytes()).replace('=', "%3D"),
        ];
        for f in &forms {
            let url = format!("https://evil.example/collect?d={f}&t=1");
            assert_eq!(
                check.check(&url, &[], b""),
                ContentVerdict::Canary,
                "query {f}"
            );
            let body = format!("{{\"payload\":\"{f}\"}}");
            assert_eq!(
                check.check("https://evil.example/", &[], body.as_bytes()),
                ContentVerdict::Canary,
                "body {f}"
            );
        }
        let up = r.marker.to_ascii_uppercase();
        assert_eq!(
            check.check("https://e.example/", &[], up.as_bytes()),
            ContentVerdict::Canary
        );
        // A header carries it too, but a credential header is not read.
        let h = vec![("x-note".to_string(), m.to_vec())];
        assert_eq!(
            check.check("https://e.example/", &h, b""),
            ContentVerdict::Canary
        );
        let cred = vec![("authorization".to_string(), m.to_vec())];
        assert_eq!(
            check.check("https://e.example/", &cred, b""),
            ContentVerdict::Clean
        );
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn fingerprints_match_a_copied_paragraph_in_any_encoding_and_nothing_else() {
        let vault = fixture_vault();
        let index = FingerprintIndex::build(&vault);
        assert_eq!(
            index.files(),
            1,
            "hidden dirs and non-text files are skipped"
        );
        assert!(index.count_matches(PARAGRAPH.as_bytes()) >= 3);
        let hidden = "this hidden directory text must never be indexed at all ever again ok";
        assert_eq!(index.count_matches(hidden.as_bytes()), 0);

        let state = tmp("fp-state");
        let check = ContentCheck::new(&state, 3);
        assert_eq!(
            check.check("https://x.example/", &[], PARAGRAPH.as_bytes()),
            ContentVerdict::Clean,
            "no index yet"
        );
        check.set_index(index.clone());
        let body = format!("{{\"text\": \"{PARAGRAPH}\"}}");
        assert_eq!(
            check.check("https://x.example/post", &[], body.as_bytes()),
            ContentVerdict::ContentMatch
        );
        // Reformatted: different case and punctuation, same words.
        let shouty = PARAGRAPH.to_uppercase().replace(',', " ;; ");
        assert_eq!(
            check.check("https://x.example/", &[], shouty.as_bytes()),
            ContentVerdict::ContentMatch
        );
        let url = format!("https://x.example/q?d={}", b64(PARAGRAPH.as_bytes()));
        assert_eq!(check.check(&url, &[], b""), ContentVerdict::ContentMatch);
        let url = format!("https://x.example/q?d={}", b64url(PARAGRAPH.as_bytes()));
        assert_eq!(check.check(&url, &[], b""), ContentVerdict::ContentMatch);
        let form = format!("https://x.example/search?q={}", PARAGRAPH.replace(' ', "+"));
        assert_eq!(check.check(&form, &[], b""), ContentVerdict::ContentMatch);

        let english = "The committee will meet next week to review the budget proposal and \
            discuss whether the new library opening hours should be extended during the \
            summer months, when more students are expected to use the reading rooms.";
        assert_eq!(index.count_matches(english.as_bytes()), 0);
        assert_eq!(
            check.check("https://x.example/", &[], english.as_bytes()),
            ContentVerdict::Clean
        );
        // A link the vault mentions is fetched, not quoted.
        assert_eq!(check.check(LINK, &[], b""), ContentVerdict::Clean);
        assert_eq!(index.count_matches(LINK.as_bytes()), 0);

        let texts = vec![english.as_bytes().to_vec(), PARAGRAPH.as_bytes().to_vec()];
        assert_eq!(measure_false_positives(&index, 3, &texts), (1, 2));
        let _ = std::fs::remove_dir_all(&vault);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn short_tokens_are_not_decoded_and_long_ones_are() {
        // 24 characters exactly: left alone.
        let short = b"aGVsbG8gd29ybGQgaGVsbG8h";
        assert_eq!(short.len(), 24);
        assert_eq!(candidate_forms(short), vec![short.to_vec()]);
        let long = b64(b"hello world, hello again!");
        assert!(long.len() > 24);
        let forms = candidate_forms(long.as_bytes());
        assert!(forms.iter().any(|f| f == b"hello world, hello again!"));
        let forms = candidate_forms(hex(b"a secret of some length").as_bytes());
        assert!(forms.iter().any(|f| f == b"a secret of some length"));
        let forms = candidate_forms(b"a%20b+c");
        assert!(forms.iter().any(|f| f == b"a b c"));
    }

    #[test]
    fn links_are_cut_before_words_are_split() {
        assert_eq!(strip_link("see:https://x.y/z"), "see:");
        assert_eq!(strip_link("[text](www.example.com/a)"), "[text](");
        assert_eq!(strip_link("awww.nice"), "awww.nice");
        assert_eq!(url_tail("https://h.example:8443/p?q=1"), "/p?q=1");
        assert_eq!(url_tail("https://h.example"), "");
    }

    #[tokio::test]
    async fn the_index_task_builds_and_rebuilds_on_notify() {
        let vault = tmp("idx-vault");
        let state = tmp("idx-state");
        let check = Arc::new(ContentCheck::new(&state, 3));
        let (handle, notify) =
            spawn_index_task(check.clone(), vault.clone(), Duration::from_secs(3600));
        let matches = |c: &ContentCheck| {
            c.check("https://x.example/", &[], PARAGRAPH.as_bytes()) == ContentVerdict::ContentMatch
        };
        for _ in 0..200 {
            if check.index().read().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!matches(&check));
        std::fs::write(vault.join("new.md"), PARAGRAPH).unwrap();
        notify.notify_one();
        let mut ok = false;
        for _ in 0..200 {
            if matches(&check) {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ok, "the rebuilt index has the new note");
        handle.abort();
        let _ = std::fs::remove_dir_all(&vault);
        let _ = std::fs::remove_dir_all(&state);
    }
}
