# Lessons

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
