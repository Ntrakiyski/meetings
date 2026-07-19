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

---

# Task Plan

## Goal

Create the remote Meetily meeting row when the first transcript segment reaches the UI, then update that one row with every subsequent segment.

## Constraints

- Keep one remote row per meeting and reuse the final local meeting ID.
- Serialize uploads so an older response cannot overwrite newer transcript JSON.
- Preserve local recording and final-save success when the network is unavailable.
- Use the existing authenticated ingest function; Realtime subscriptions are not required for writes.
- Keep the final full-meeting upsert as the authoritative snapshot.

## Steps

- [x] Reuse the frontend recording-session ID for final SQLite persistence.
- [x] Add a serialized live-publish queue driven by transcript UI events.
- [x] Flush live publishing before the final meeting save.
- [x] Verify first-segment creation and ordered JSON updates against production.
- [x] Build and verify the macOS application.

## Verification

- [x] The first segment creates one remote row.
- [x] Later segments update the same row in order without duplicates.
- [x] Final save keeps the same external ID and complete segment list.
- [x] Missing credentials or network failures do not block local persistence.
- [x] No InsForge Realtime dependency was added.

## Review

The frontend now starts a live publishing session with the same ID later used by SQLite. Each accepted UI transcript event appends to an in-memory snapshot and queues one authenticated upsert; the promise chain prevents older snapshots from racing newer ones. Stop processing waits until the queue is stable, then the existing final publisher writes the authoritative complete meeting. Missing credentials and request failures are swallowed by the live queue and cannot block local saving.

No InsForge Realtime dependency was added because it does not improve writes. It remains an optional subscriber mechanism if another screen later needs to watch the row change.

Verification passed: the app-only TypeScript check, Rust `cargo check`, focused publisher test, and full signed Next/Tauri release build. A production two-stage synthetic upload created one row on the first segment and updated that same row to two ordered segments; the row count remained one and the synthetic row was removed. The final `.app` passes strict code-signature verification.

---

# Task Plan

## Goal

Automatically enhance the completed transcript when a meeting stops, preserve the exact original, show the enhanced version by default with a raw-transcript control, and establish truthful speaker behavior for laptop meetings and headphones.

## Constraints

- Never overwrite or discard the locally captured transcript.
- Allow the LLM to change text only; segment identity, order, timestamps, and speaker source must remain deterministic.
- Reuse the user's configured summary LLM provider and credentials.
- A missing model/key or enhancement failure must not block saving or opening the meeting.
- Continue to distinguish microphone from system audio without inventing remote speaker identities.
- Preserve unrelated `pnpm-lock.yaml`, `.codex`, and `script` changes.

## Steps

- [x] Trace the active LLM, persistence, meeting-details, and final audio paths.
- [x] Add durable raw/enhanced transcript state and a validated automatic enhancement command.
- [x] Trigger enhancement after stop and republish raw plus enhanced transcript data to Connections.
- [x] Add a corrected/raw transcript control with processing and failure states.
- [x] Assess and implement the smallest honest speaker improvement supported by the captured audio.
- [x] Apply and verify the compatible InsForge schema/function changes.
- [x] Run focused tests, type checks, release build, signature checks, and an end-to-end data round trip.

## Verification

- [x] Original transcript remains byte-for-byte available after successful enhancement.
- [x] Enhanced transcript is default and only segment text differs.
- [x] Failed or unavailable enhancement falls back to raw without losing the meeting.
- [x] Connections stores both raw and enhanced JSON under one stable meeting row.
- [x] Microphone/headphone/system-audio behavior is verified and described accurately.
- [x] Final signed application artifact is rebuilt and checked.

## Review

Meetily now starts a best-effort enhancement job immediately after the authoritative local save at meeting stop. It reuses the configured summary provider/model, processes bounded batches, accepts only the same ordered segment IDs, and persists corrected text in a separate column. Raw text, timestamps, speakers, and segment identity are never overwritten. Meeting details poll while correction runs, show corrected text by default when complete, and provide an Original/Corrected control; the Original view bypasses display cleanup and renders the stored ASR text exactly. Automatic summaries wait for enhancement to finish so they use corrected text when available.

Connections production now stores `raw_transcript` and `raw_transcript_segments` beside the corrected/default fields under the same external meeting ID. The schema and edge function were tested on an InsForge branch, merged with zero conflicts, deployed to production, and verified with an authenticated two-version upsert. The test proved later transcript-only updates do not erase the raw copy; all synthetic rows were deleted. A missing PostgREST cache refresh found during production verification was corrected and confirmed in logs.

The active capture pipeline can reliably distinguish the selected microphone from macOS system audio, including when meeting playback uses headphones. The UI now renders these as `You` and `Meeting audio`. It still does not claim Speaker 1/2/3 within remote meeting audio: true remote-person separation needs an active diarization model and clustering pass, which this Community pipeline does not currently provide.

Verification passed: app-only TypeScript checking through the production Next build, Rust `cargo check`, focused enhancement and Connections publisher tests, all 437 Connections tests, strict app code-signature verification, valid DMG checksum, and production InsForge round trips. The `.app` and DMG were rebuilt successfully. The optional Tauri updater archive could not be cryptographically signed because `TAURI_SIGNING_PRIVATE_KEY` is not present; this does not invalidate the verified ad-hoc-signed app bundle or DMG.
