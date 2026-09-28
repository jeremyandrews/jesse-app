//! Recorded audio, transcribed on the Studio by engines that run inside this process, or by
//! a hosted speech engine the owner explicitly selected.
//!
//! # THE INVARIANT, first, because everything below is downstream of it
//!
//! **Recorded audio leaves the Studio only to a hosted speech engine the owner selected, and
//! only as that engine's transcription request.** It arrives over the tailnet from the phone,
//! or over loopback from the Mac app, and lands in custody. From there:
//!
//! * with no hosted engine selected (the default, `JESSE_SPEECH_ENGINE=local`), it never
//!   leaves the machine: it is read by open models loaded from this disk into this process,
//!   and the wire traffic is byte for byte what it was before hosted engines existed;
//! * with one selected, as the Studio default (`JESSE_SPEECH_ENGINE=hosted:<id>`, or
//!   `local,hosted:<id>` for local first and that engine only when the local reading fails)
//!   or for one run (`?engine=` on the upload), it goes to that engine's transcription
//!   endpoint and nowhere else;
//! * it NEVER reaches a turn, the assistant child, a vision helper, or any registered model
//!   through the turn path. Nothing selects a hosted engine implicitly, and a local failure
//!   never escalates to the cloud unless the owner configured that fallback.
//!
//! **Once it is text, nothing further is restricted**: the transcript is an ordinary message
//! and flows exactly as typed text does.
//!
//! # Why the rule changed (Bridge 0.159.0), on purpose
//!
//! The rule until then was that audio never left the Studio, and a guard and a wire test held
//! it. On 2026-09-28 the local engines looped on every hour-long recording and returned a
//! minute of text each, and the owner had no other engine to ask: the rule protected the
//! audio by leaving him with no transcript at all. The replacement keeps what mattered (no
//! implicit egress, and nothing reaching a turn surface) and adds the one exit he chooses.
//! It replaced, in turn, App 1.0 (124)'s "audio never goes on the network", which drew the
//! line at the network interface rather than at the destination.
//!
//! # How the code holds it
//!
//! 1. **ONE DOOR.** Audio is accepted on exactly one route, `POST /jesse/transcriptions`
//!    ([`http`]), and lands in an [`intake::AudioCustody`]. The turn ATTACHMENT gate
//!    (`crate::sniff_attachment`) still refuses every audio container, and must: a turn
//!    attachment can be routed to a hosted vision helper or read by a hosted child. A test
//!    pins that the two sniffers never accept the same bytes.
//! 2. **TWO KINDS OF ENGINE, BOTH NAMED.** A local engine exists only as a model file on this
//!    disk loaded into this process ([`engine::EngineLoader`], the known-good list
//!    [`models::known_good`]). A hosted engine exists only as a registry entry that DECLARES a
//!    transcription capability and is armed (its token is set), resolved by the HTTP boundary
//!    into a plain [`hosted::HostedTarget`] when the configuration or the run names it.
//! 3. **NO HANDLE ON ANYTHING ELSE.** The pipeline ([`service`]) is handed its own parts and
//!    nothing more: no application state, no model registry, no vision layer. Even the hosted
//!    engine is handed a target, not the registry. `scripts/ci-guards.sh` fails the build if
//!    any file here other than the HTTP boundary names one of those surfaces.
//! 4. **ONE FILE MAY SEND.** [`hosted`] is the only file here that makes an outbound request,
//!    and it posts only to its target's transcription URL; the guard fails the build on a
//!    request anywhere else in `speech/`, or on any other request in `hosted.rs`. Model
//!    weights are still fetched with a body-less GET for a URL fixed at compile time
//!    ([`models::HttpFetcher`]); no audio has a path into that request.
//! 5. **THE WIRE TESTS.** In [`http`]:
//!    `recorded_audio_never_reaches_a_hosted_backend` (nothing selected: the active model, its
//!    vision helper and an ARMED speech engine all see zero connections);
//!    `a_selected_hosted_engine_is_the_only_place_the_audio_goes` (selected: the canary
//!    reaches that engine's transcription endpoint and nothing else, and the same audio as a
//!    turn attachment is refused); and
//!    `local_then_hosted_sends_audio_only_when_the_local_reading_fails`.
//! 6. **PROVENANCE.** The status names every engine that read the recording and, for a hosted
//!    one, the host the audio went to, so a transcript read in the cloud is never taken for
//!    one read on the Studio.
//!
//! # Custody
//!
//! The upload and every working copy derived from it (the decoded 16 kHz file, a hosted
//! engine's chunks) live in one 0700 directory per run, removed by `Drop` when the run ends:
//! on success, on every failure, on cancel, and on a panic's unwind. A bridge killed mid-run
//! leaves the directory behind, and boot deletes everything under the intake root: jobs are in
//! memory, so anything there is by definition abandoned. The conditioned signal is never
//! written at all. The bridge keeps the TRANSCRIPT (in memory, for a day, so the app can
//! collect it); it never keeps the audio.
//!
//! # After the transcript
//!
//! Nothing here touches a turn. The app puts the transcript in the composer and the user
//! sends it like any message, to whatever model the bridge is configured to use. The one
//! thing this adds downstream is the DISAGREEMENT LIST ([`reconcile`]): where two engines
//! (two local ones, or a local and a hosted one) read the same stretch differently, both
//! readings are returned, so the model that later reads the transcript can resolve a date or a
//! name the way a person would, and say when it cannot.

pub mod condition;
pub mod decode;
pub mod engine;
pub mod hosted;
pub mod http;
pub mod intake;
pub mod models;
pub mod reconcile;
pub mod service;
pub mod wav;

pub use http::{jesse_speech, jesse_transcribe, jesse_transcription, jesse_transcription_cancel};
pub use models::{CheckReport, SpeechTier};
pub use service::{SpeechConfig, SpeechService};
