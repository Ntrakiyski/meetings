# Task Plan

## Goal

Persist Meetily summaries and honest per-segment speaker-source metadata in Connections, while defining the safe path for true diarization and LLM transcript correction.

## Constraints

- Preserve existing local recording, transcript, and summary behavior.
- Do not invent Speaker 1/2/3 labels without a diarization model.
- Keep publishing best-effort so network failure cannot lose a local meeting or summary.
- Preserve timestamps and the original transcript.
- Do not overwrite unrelated uncommitted Meetily work.

## Steps

- [x] Trace the active transcription, summary persistence, and Connections publishing paths.
- [x] Propagate microphone/system-audio source labels into persisted transcript segments.
- [x] Republish the full meeting after generated or edited summaries are saved.
- [x] Expose transcript segments through the Connections Meetily read actions.
- [x] Verify local Rust/TypeScript checks and a production round trip.

## Verification

- [x] Initial meeting publishing still succeeds without a summary.
- [x] A later summary updates the same remote meeting row.
- [x] Transcript segments retain timestamps and source labels.
- [x] Publishing remains non-blocking on failure.
- [x] The implementation does not claim multi-person diarization.

## Review

Meetily now propagates the active audio channel as `Microphone` or `System Audio`, saves it locally in the existing speaker field, and publishes it inside each timestamped `transcript_segments` JSON object. Generated summaries and later summary edits republish the full meeting by stable ID; network failures remain detached from local persistence. The production ingest function preserves an existing summary on transcript-only retries, and the Connections detailed meeting actions expose the segment JSON. The unused remote `action_items` column was removed.

Verification passed: Rust `cargo check`, the focused payload test, an app-only TypeScript check, the full Next/Tauri release build, all 437 Connections tests, and the Connections fix/type checks. A production synthetic upsert proved summary retention plus speaker/timestamp storage, then the synthetic row was deleted. Connections deployed commit `3e9b2e0` healthy. The built macOS app passes strict code-signature verification.

True Speaker 1/2/3 diarization remains intentionally out of scope: the active Community transcription pipeline does not produce stable speaker embeddings. LLM transcript correction should be a separate manual operation that stores an enhanced copy and preserves the original transcript and timestamps.
