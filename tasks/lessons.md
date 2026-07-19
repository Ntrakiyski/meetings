# Lessons

## 2026-07-19 - Separate macOS bundle success from updater-signing success

Mistake: Treated the Tauri build command as one undifferentiated result even though it can successfully create and sign the `.app`/DMG, then fail later because updater signing lacks `TAURI_SIGNING_PRIVATE_KEY`.
Why it happened: The updater public key is configured in the repository, while its private counterpart is intentionally absent from the environment.
Rule for next time: Inspect the emitted artifacts and verify macOS code signatures/DMG integrity separately; report updater signing as a distinct credential requirement.
Example check: `codesign --verify --deep --strict` and `hdiutil verify` pass even if the optional updater archive signing step exits nonzero.

## 2026-07-19 - Reload PostgREST schema after merging an InsForge migration

Mistake: Assumed an InsForge branch merge would refresh the parent PostgREST schema cache in the same way as a direct migration apply.
Why it happened: The merged columns existed in PostgreSQL and migration history, but the parent PostgREST process had not received a reload notification.
Rule for next time: After merging schema changes, verify one API write using every new column; if PostgREST reports `PGRST204`, issue a schema-cache reload and repeat the exact write before deployment completion.
Example check: Production function logs contain no `PGRST204`, and a raw/corrected payload round-trips through the function.

## 2026-07-19 - Verify available test command syntax before combining checks

Mistake: Passed two positional Rust test filters in one `cargo test` invocation and assumed the Connections package exposed an `npm run check` script.
Why it happened: Combined previously useful checks without confirming each tool's accepted arguments and the current package scripts.
Rule for next time: Inspect `cargo test --help` or run one filter per invocation, and read `package.json` before selecting repository scripts.
Example check: Two separate `cargo test <filter> --lib` commands and an existing `npm run <script>` both start successfully.

## 2026-07-19 - Search every caller before extending an internal API

Mistake: Added the raw-transcript argument to the Connections publisher but initially updated only the live and saved-meeting callers.
Why it happened: The final-save publisher lives in the general API module rather than the Connections module.
Rule for next time: Run `rg` for every call site before compiling after changing a shared function signature.
Example check: `rg "publish_meeting\\(" frontend/src-tauri/src` lists the live, final-save, saved-meeting, and test paths.

## 2026-07-19 - Parenthesize jq pipelines inside boolean assertions

Mistake: Wrote a combined `jq -e` assertion whose pipe precedence applied `length` to the intermediate boolean instead of the segment array.
Why it happened: Combined row-count and segment-count checks without grouping the nested pipeline.
Rule for next time: Parenthesize each piped subexpression before joining it with `and` or `or`.
Example check: `.meetings | (length == 1 and (.[0].transcript_segments | length == 2))` evaluates both intended arrays.

## 2026-07-19 - Pass the source file when deploying an InsForge function

Mistake: Invoked `functions deploy meetily` without the CLI's required `--file` option.
Why it happened: Recalled the function slug but not the installed CLI version's deploy syntax.
Rule for next time: Check function deploy help and pass the reviewed source path explicitly.
Example check: `functions deploy meetily --file functions/meetily.ts` returns a successful deployment URL.

## 2026-07-19 - Use an app-only TypeScript check in Meetily

Mistake: Ran the repository TypeScript compiler directly, which failed on the pre-existing `bun:test` import in the test tree before checking the application sources.
Why it happened: The main `tsconfig.json` includes every TypeScript test even though Bun's test types are not installed for the pnpm toolchain.
Rule for next time: Run an app-only TypeScript config that excludes `tests`, and run the test suite with its own runtime.
Example check: `tsc` over `frontend/src` passes independently of the Bun test-type setup.

## 2026-07-19 - Do not pass provider source paths to Vitest

Mistake: Used `src/providers/meetily` as a Vitest filter even though the provider has no standalone test file, causing a false “No test files found” failure.
Why it happened: Treated Vitest's positional filter as a source-code selection rather than a test-file selection.
Rule for next time: Run the full fast Connections suite when a provider has no focused test file.
Example check: `npm test -- --run` discovers all provider/runtime regressions and exits successfully.

## 2026-07-19 - Publish data that is created after meeting completion

Mistake: Published the meeting only when the transcript was first saved, so summaries generated later remained local and never reached Connections.
Why it happened: The integration was attached to the initial persistence event without tracing the later summary-completion and summary-edit paths.
Rule for next time: For derived meeting data, publish after every authoritative local save and upsert by the stable meeting ID.
Example check: Save a transcript, then generate a summary, and verify the same remote row gains the summary without creating a duplicate.
