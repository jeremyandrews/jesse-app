## [App 1.0 (189)] - 2026-10-05

**A run recorded by a third party app reaches the agent with its elevation, its time to
the second and its laps.** An outdoor Runna run on Apple Watch on 2026-10-05 showed in
Apple Fitness an elevation profile from 284 to 354 m, workout time 0:29:26, elapsed
0:29:31 and 5 laps; the workout line sent `29m` and none of the rest. Three root causes:

1. **Elevation was read only from workout metadata a third party recorder never writes.**
   `HKMetadataKeyElevationAscended` and `HKMetadataKeyElevationDescended` come from Apple's
   Workout app. Runna saves the GPS route instead, and the app never read routes.
2. **Duration was rounded to the minute and elapsed time was never sent.** `HKWorkout.duration`
   is workout time, pauses excluded, and its doc comment wrongly called it elapsed.
3. **Run lap events were ignored.** `.lap` events were read for swims only.

- **Route elevation.** `HKSeriesType.workoutRoute()` joins the read set (the existing
  grown-set check prompts for it once). For a run, walk or hike with no ascent metadata the
  route's locations are read and reduced at once to four numbers by the new pure
  `RouteElevationReducer`: readings with negative vertical accuracy or worse than 10 m are
  dropped, a change of direction counts only after 5 m of hysteresis (so flat ground jitter
  adds nothing, and a confirmed climb is counted to its true peak), and fewer than 10 usable
  readings yield nothing. No coordinate is kept, logged or sent. Metadata still wins and
  renders unmarked; the route's values render `ascent 100 m (route)`, and the range
  `elevation 284 to 354 m` follows whenever the route was read. Elevation stays off cycles.
- **Bounded, cached route read.** A first cut of this release would never have rendered a
  long run's climb: the route read's 600 ms bound was an unmeasured guess, a read that missed
  it was thrown away, and nothing was cached, so a marathon's ~12,600 points were re-read
  from HealthKit on every turn and missed the bound every time. Now the reduced numbers are
  cached per workout UUID (`RouteElevationCache`, Application Support
  `JesseRouteElevation/route-elevation.json`, excluded from backup, the four numbers and
  two dates per workout, pruned to workouts that ended within the last 72 h). A hit reads
  nothing. A miss starts one read per workout, never cancelled; the gather waits up to
  900 ms (`BoundedRead`, which resumes at the bound even though a HealthKit query ignores
  cancellation) and a later result lands in the cache for the next turn. A workout with no
  route series, or a failed read, is not cached and is retried; routes with too few usable
  readings are cached as a negative. Each read logs its series count, point count,
  milliseconds and whether it beat the bound, never a location.
- **Time.** The base line renders workout time to the second (`29m26s`, `1h05m10s`), and
  the detail segment adds `elapsed 29m31s` when elapsed exceeds workout time by 5 s or
  more. `WorkoutSummary.end` now uses the elapsed time when known.
- **Laps.** Runs, walks and hikes render their `.lap` event count (`5 laps`); swims are
  unchanged.
- **Brand and weather.** `HKMetadataKeyWorkoutBrandName` joins the attribution when it
  differs from the source name, and `HKMetadataKeyWeatherCondition` renders as a word next
  to the temperature (`partly cloudy`), mapped in the pure layer and pinned to the SDK enum.
- **Inventory.** `docs/workout-feed-inventory.md` lists every workout relevant metadata
  key, quantity type and workout sub object the feed still does not read, with a
  recommendation for each.

## [App 1.0 (188)] - 2026-10-05

**Query expansion adds new words instead of rephrasing the query.** On 1.0 (187) `lost
keys` expanded to `missing keys`, `keys not found`, `search for keys`, `found keys`: every
term repeated `keys`, none brought a new word for it, and a thread that said `keychain` or
`fob` could never be found. Three root causes:

1. **Expansion asked for paraphrased queries.** The instructions and the `@Guide` asked
   for "alternative search terms for the same thing, synonyms, rephrasings", and a small
   model answers that by rewriting the query around its strongest noun, kept verbatim.
2. **Each alternative ANDed its own tokens.** Every term was compiled like a typed query,
   so `keys not found` needed `keys`, `not` and `found`. Every alternative was a subset of
   the threads containing `keys`: expansion could only narrow inside the original words.
3. **Function words were required.** `significantTokens` keeps any token of two or more
   characters, so `for` in a model term was a mandatory needle.

- **One group per word.** The model is now asked, for each significant word of the query
  (stop words removed with `LookupQuery.isStopWord`), for up to four words or short phrases
  that mean the same thing or name the same object, never containing the word, with a
  worked example (`lost: misplaced, missing, can't find, forgot`; `keys: key, keychain,
  fob, car key`). The fresh session per call, prewarm, the 2 s timeout and the cache stay.
- **A deterministic filter** (`SearchQueryRules.filterConcepts`, replacing
  `filterExpansionTerms`): trims, folds case and diacritics, drops blanks, single stop
  words, and any alternative containing its own word or another word of the query; keeps
  an alternative the word contains (`key` for `keys`); dedupes; caps four per word and
  twelve overall, round robin. The observed `lost keys` output now filters to nothing.
- **AND across words, OR within a word.** `CompiledConceptQuery` in `ThreadSearchIndex`:
  an expansion hit contains, for every query word, the word or one of its alternatives
  (matched whole, as a phrase, either apostrophe), at least one of them an alternative. A
  thread saying only `misplaced my keychain` is now found for `lost keys`. The typed query
  and its semantics are unchanged; ranking stays title, body, expansion.
- **The vault search gets the same idea** through its unchanged `[String]` contract:
  alternate queries with one word substituted at a time (`misplaced keys`, `lost key`,
  ...), best first, at most four.
- **"Also searching:" groups by word** (`misplaced, missing · key, keychain`) on the
  iPhone and the Mac, and the `search` log line records the groups.
- **A quality check against the real model** (`ExpansionQualityTests`) runs the production
  expander over six fixed queries and prints the groups; it skips where the model is
  unavailable.

## [App 1.0 (187)] - 2026-10-05

**Conversation search answers instantly, and the on-device expansion runs on ordinary
queries and says so.** Three root causes, each measured on a 500 conversation fixture in
the simulator before it was changed:

1. **Unbounded main thread rescans per keystroke.** The list filtered from computed view
   properties: the expansion gate's base count, the union match set and the layout each
   walked every turn of every thread through `localizedStandardContains` on the main
   actor, with no debounce and nothing cached, faulting every turn body on the way. An
   ordinary query cost 140 to 170 ms of main thread work per keystroke.
2. **The base count gate suppressed expansion.** `shouldExpand` only asked the model when
   the typed query found fewer than five threads. Almost every real query finds five, so
   the model was almost never asked and "Also searching" almost never appeared.
3. **The expander's session grew forever.** One `LanguageModelSession` served every query,
   so each call carried every earlier prompt and answer; each expansion was slower than the
   last, and once the context window filled every call threw and was swallowed to `[]`.

- **One search pass per settled query, off the main actor.** `ThreadSearchModel` debounces
  the field (120 ms), cancels stale passes, and runs the pass in the new
  `ThreadSearchIndex` actor; the list keeps the previous result until the new one lands.
  The index holds each thread's title and every turn's searchable text, case, diacritic
  and width folded once into bytes, and rebuilds only threads whose title or `updatedAt`
  changed, reading them through its own `ModelContext`. It is built in the background when
  the search field gains focus. An in memory index meets the budget (a settled pass of 1
  to 10 ms), so SQLite FTS5 was not needed.
- **Short queries stay cheap.** One character matches word starts in titles only; two
  characters match word starts in titles and bodies; three or more keep the old substring
  semantics, and a test asserts the index finds exactly the threads `threadMatches` found.
- **Ranked results.** While searching, the list (iPhone and Mac) is flat: title hits, then
  body hits, then expansion only hits, each newest first. Snippets are cut lazily for
  visible rows from the one source text the pass recorded; the Mac sidebar now shows them
  too.
- **Every real query expands.** `shouldExpand` is now: three or more characters, the tier
  enabled, the model available. The vault search shares the gate, its alternate hits still
  follow every direct hit, and it now obeys the Settings toggle as well.
- **A clean session per expansion,** a 2 s timeout that yields no terms, and one `search`
  log line per expansion with the query, terms, latency and outcome.
- **Expansion is visible.** "Expanding search…" shows while the model works and "Also
  searching: …" when its terms land, on the iPhone and the Mac. Settings says when the tier
  is off or why the model is unavailable.

| Query | Main thread per keystroke, before | After | Settled pass after the debounce |
|---|---|---|---|
| `j` | 28 ms (111 ms cold) | 1.7 ms | 0.6 ms |
| `je` | 141 ms | 1.9 ms | 6.1 ms |
| `jes` | 142 ms | 1.8 ms | 6.1 ms |
| `bridge` | 150 ms | 1.9 ms | 7.4 ms |
| `run bridge` | 167 ms | 1.6 ms | 10.1 ms |

Debug build, iPhone simulator on the Mac Studio. The index's first build takes about
750 ms, off the main actor, once per launch.



**"When is my flight today?" was answered "Not found in the vault on this device", with
the answer sitting in three live notes.** Both questions asked on the phone that Sunday
with the bridge unreachable — the owner's own phrasing, and "When is the KLM flight to
Amsterdam today?" — reached the itinerary's `## Flight Details`, the trip note's booking
section and the day's list, and retrieved none of them. Three causes, each proved against
the real vault before anything was changed:

1. **Every word of the question was a requirement on the note.** The chat path dropped
   stop words and then handed the survivors to `VaultSearchQuery.matchExpression`, which
   joins tokens with `AND`. "today" is not a stop word and no note writes it for a date,
   so the precise pass matched nothing; the single-keyword fallback then fused a top
   twenty PER WORD, and the fused four need not hold the right chunk.
2. **The owner's name was one more requirement**, appended to a first-person question by
   App 1.0 (170). A booking, a calendar line or a Today item rarely names whose it is —
   and the one that did spelled it `JEREMIAH KIRSTEN ANDREWS`, which the prefix term
   `"Jeremy"*` does not match.
3. **The model was never told the date.** With `Sun 27 Sep` in front of it and no idea
   what day it was, abstaining was the correct behaviour under its own instructions.

A question is now PLANNED rather than tokenized. `LookupPlan` reads it as concept groups:
one per content word, every spelling of the owner's name as one, and the absolute forms of
whatever day a relative word named as one more. ONE FTS5 query ORs every term of every
group, and a chunk is ranked by how many GROUPS it matched — the question's own words
first, the owner and the date as modifiers on top of them, bm25 breaking the ties. Nothing
is required, so no single word can empty the result, and four spellings of one name are
four chances to match one concept rather than four concepts.

**The typed search box is untouched.** Its rule is every token and it stays every token:
the person typing into a field chose those words. Only the chat retrieval path changed.



### Corrected

- **An ungranted MCP tool on codex-cli 0.153.4 produces no refusal at all**, because there is
  nothing to refuse: the tool is absent from the model's tool list and no call is emitted. The
  `TypeError: tools.… is not a function` shape documented against 0.146.0 did not reproduce. A
  Codex send probe can therefore only ever record absence, never an attempt that was stopped —
  a stronger boundary and a weaker observation, which is precisely why the record now has to
  show that the tool is absent from the **child**.
- **SECURITY.md said the battery "cannot observe what a server REGISTERED".** That is true of
  Codex and false of Claude Code, whose init event lists registered tools before the allowlist
  — which is how the record establishes that WhatsApp registers three send tools and Slack
  registers none. The section now distinguishes the two harnesses.

### Operator note

**The Codex containment record is invalidated by this change and must be re-recorded before
this bridge can serve a Codex turn**, because every Codex row's `toolset_args` is now short by
its `enabled_tools` lines. Run, where the credentials are:

```
cargo run --features containment-probe --bin containment-probe -- --harness codex --write
```

`bridge/containment.toml` is untouched and no Claude Code argv moved.

**`JESSE_VAULTQA_MCP_CONFIG` in the LaunchAgent plist must change from a path to `qmd`, or be
removed, before this bridge is started.** The old value now refuses to boot.

## [App 1.0 (141)] - 2026-09-18

**The unread badge stopped re-rendering the whole app on every save.** The markers that
arrived in App 1.0 (135) were right; where their NUMBER came from was not.


On the real vault, pinned to the morning of the incident: "When is my flight today?" went
from four unrelated chunks to the trip note's booking section third and the itinerary's
flight table fourth — both carrying `KL1654`, `12:40` and `Sun 27 Sep`. "When is the KLM
flight to Amsterdam today?" now ranks the right NOTE first but still the wrong section of
it: the booking row says `AMS`, not `Amsterdam`, so the fare table that was researched
before the booking matches one more of the question's words. The vocabulary gap between
`AMS` and `Amsterdam` is the expansion tier's job and that tier is unchanged here.

### Added

- `LookupPlan` (and `VaultClock`) in JesseVault: the pure planner. Content words with the
  relative day words removed (today, tonight, tomorrow, yesterday, this morning, this
  afternoon, this evening, this week, next week), the owner's forms, and the day's
  absolute forms derived from one list of nine formats (`2026-09-27`, `27 Sep`, `Sep 27`,
  `27 September`, `September 27`, each of the four again behind the weekday abbreviation
  an itinerary table uses). `this week` and `next week` name a span, so they leave the
  query and bring no date with them. Every form is spelled in the device's locale AND in
  English, because the notes are English on a device that may not be.
- `VaultSearchQuery.anyMatchExpression(_:)`: the OR expression, each term a quoted FTS5
  phrase with a prefix on its last word, so `27 Sep` also matches `27 September`.
- `VaultSearcher.matchingAny(_:limit:)`: the one query, uncollapsed, so the concept rank
  sees a note's flight table even when bm25 preferred another of its chunks.
- `VaultRetriever.byConcept(_:plan:)`: the rank, and `lexicalScanLimit` (120 rows) it
  ranks over.
- `VaultRelativeDateRetrievalTests`: nineteen tests over the floor corpus — both
  questions, the same shape for "tomorrow", every relative phrase and the day it means, a
  day no note mentions, the owner's four spellings counted once, a first-person question
  still finding a note that names nobody, the rank itself, and the prompt's date line.

### Changed

- `VaultRetriever.retrieve`: three widening passes became one query plus the existing
  expansion tier, which is now additive rather than replacing. The archive demotion, the
  `Inbox/` exclusion, the budget and the embedding fusion are unchanged; the fusion now
  reorders the concept rank rather than bm25's raw order.
- `VaultAnswerer.prompt`: states the date first — `Today is Sunday, 27 September 2026.` —
  in the device's locale and zone, from an injected clock. Unconditional: thirty
  characters, and "is that this week?" is a question about the date that does not contain
  the word.
- `VaultAnswerer.instructions`: one more sentence, "Today, tomorrow and yesterday mean the
  date in the first line." Thirty-six words, still under the sixty-word ceiling.
- `VaultAnswerGenerating`: a second, defaulted `generate(question:chunks:clock:)`. Only
  one conformance builds a prompt; the stubs do not have to mention a date they have no
  use for.
- `OfflineLookupSettings.ownerName`: may hold several spellings separated by commas
  (`Jeremy, Jeremiah, Jeremia, Andrews`). `PromptStore.ownerName`, which builds sentences,
  answers with the first form alone.
- `OfflineAnswerService`: one `VaultClock` for the turn — the day the retriever resolves
  "today" against, the day the prompt states, and the stopwatch the diagnostics row is
  timed with. It replaces the `now` closure.
- `VaultRetrievalFixture`: two itineraries of the same shape on different days, a Today
  list that names the flight without the word, four live notes that say "flight" and
  answer nothing, six that name the owner and answer nothing, a pinned clock and an owner
  name. The floor suite grew from fourteen questions to eighteen; the four new ones each
  ask with a word no note contains.

## [App 1.0 (176)] - 2026-09-27

**A saved note edit showed on the reader only after a relaunch.** Edit a note, Save, and
back in the reader the text was as it had been before the save, under "The Obsidian copy on
this device is behind"; force quit, relaunch, and the edit was there. It started with bridge
0.154.0, whose `GET /jesse/vault/note` made the reader ask the Studio on every open. Before
that route existed the reader always showed the device's copy, so nothing could race.

**The root cause: the reload after a save raced the unawaited outbox flush, and the open
decision ignored this device's own queued writes.** `VaultNoteWriter.replace` queues the
write, writes the local file, and starts the flush without waiting for it. The editor's
Save then reloads the reader at once, which reads the new local bytes and asks the Studio
with their hash; the flush has usually not reached the bridge yet, so the Studio answers
with its old copy, and `VaultNoteOpening.decide`, working from the two hashes alone, showed
that old copy as if the device were behind. When the flush then landed, nothing reloaded:
the outbox's change notice only refreshed the pending count. Two smaller holes led to the
same screen. A save to the Studio's copy (`VaultBridgeCopyWriter`) whose flush came back
`503` or failed left the write queued and reloaded onto the Studio's unchanged text, with no
sign that the save was only waiting. And `VaultNoteWriter` never cleared the opener's ten
second reuse cache, so a Studio copy fetched just before a local save could be served to the
reload after it. Failing tests confirmed all three on `main` before anything changed.

**The fix, all in JesseKit, so the iPhone and the Mac both have it.**

- The open decision takes this device's unanswered writes to the note. When the newest one
  left the local copy exactly as it is now, the local copy is the newer one by construction
  and opens as the device's own, editable, with no "behind" line. A conflicted write, and
  every offline answer, decide exactly as before.
- On the Studio's copy, an unanswered save is shown as what was saved, with the pending
  notice and a header line saying the change has not reached the Studio. An edit or a tick
  made on it takes it as its base, so a second save queues against what is on screen. The
  Obsidian folder is still never written from the Studio's copy.
- The reader reloads, without a spinner, when a write of its note is answered (applied or
  refused), and never while the editor is open. A write answered while the reader was

### Fence shapes the changelog does not carry

A fenced block with a language tag:

```swift
let blocks = parseMarkdownBlocks(reply)
print(blocks.count)
```

A tilde fence, whose body holds a line of three backticks that must stay code:

~~~
cargo test --workspace
```
~~~

A four backtick fence that quotes a three backtick fence inside it:

````markdown
```swift
let quoted = true
```
````

The unclosed fence below runs to the end of the reply:

```
tail -f ~/Library/Logs/jesse-bridge.log
