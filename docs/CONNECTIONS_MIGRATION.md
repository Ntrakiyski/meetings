# Connections desktop migration

Tracks [Connections #11](https://github.com/Ntrakiyski/connections/issues/11), paired with [server PR #14](https://github.com/Ntrakiyski/connections/pull/14). This branch implements the `meetings-v1` contract. It is not a production release.

## Ownership and recovery

Discovery pins the configured issuer and endpoints, S256 PKCE, exact native redirect, state and callback issuer. An opaque access token is independently verified through `/api/meetings-identity`. Tokens use real OS keychain backends in a new Connections namespace; changing issuers requires sign-in.

Capture, import, retained finalization, saved recordings and queues carry the original issuer, company, workspace and stable user. SQLite keys include issuer/company/workspace; queue selection also includes creator. Switching to A2, B1 or another issuer cannot drain A1's queue. Stop and save controls remain mounted when the session disappears during capture.

The new checked SQLite migration retains Clerk-era meetings as `legacy_unverified` and preserves the original queue separately. Legacy user and organization snapshots are retained. The native **Verify and recover** action calls `/api/meetings-identity/legacy` under the current grant. Only a verified server import ledger matching the original workspace, company and user permits local backfill. Queue conflicts preserve the original bytes rather than replacing newer work. Recovery repeats safely; verified ownership is immutable.

Before the historical destructive ownership migration runs, startup archives its five content tables in `connections_legacy_*` tables in the same local database. SQLx applies the unchanged checked migration files and checksums. Original recording folders remain untouched. These pre-ownership archives have no original owner proof and are never assigned by the active sign-in. The visible recovery count includes preserved recordings and legacy queue entries.

## Verification so far

- 18 production protocol, queue and migration tests passed through a portable Rust harness on Linux. Includes persistent SQLite close/reopen, repeated checked SQLx migrations, A1/A2/B1/issuer/user isolation, failed offline retries, immutable ownership and proof-based legacy recovery with conflicts.
- Five frontend session/retry recovery tests passed with Node's test runner.
- Cargo dependency metadata validates with `--locked`; OS keyring features and their required lockfile dependencies are included.
- The frontend production build/typecheck and five recovery tests passed on macOS in [CI run 42422462860](https://github.com/Ntrakiyski/meetings/actions/runs/42422462860). The first frozen install exposed a pre-existing lockfile/overrides mismatch; the lockfile now passes frozen validation. Native sidecar compilation exposed an unused OpenSSL auto-detection incompatibility, addressed in the pilot workflow. [CI run 42422692330](https://github.com/Ntrakiyski/meetings/actions/runs/42422692330) is still verifying the full native build. OS keychain, actual capture and the paired pilot remain unverified. The workflow creates an isolated unsigned pilot application as a CI artifact without publishing a release or update feed.

The portable harness is diagnostic evidence, not an application build or paired desktop pilot. CI results must be recorded before the PR is marked ready.

## Paired pilot

Use the exact companion server commit deployed to an isolated HTTPS staging origin. Launch the unsigned pilot application with `MEETINGLY_CONNECTIONS_URL` set to that origin. Its application identifier is `com.meetily.connections-pilot`, so it uses a separate app data directory from the installed release. Use synthetic recordings/accounts. Do not copy a live database; use a consistent backup if testing legacy recovery.

1. Sign in, consent to A1 and verify identity. Wait more than five minutes and verify refresh.
2. Start a short recording under A1, disconnect the network, stop and save. Preserve its external ID and local ownership.
3. Restart, reconnect and authorize A2, then B1. The A1 recording must remain queued and be absent from both receiving workspaces.
4. Reauthorize its original user in A1. Verify upload, transcript bytes, revision, receiving workspace and creator. Repeat retry to verify idempotency.
5. Remove/revoke A1 access and verify refresh/upload denial, with B1 still usable. Changing the issuer must require sign-in and retain the original queued ownership.
6. Test legacy recovery using a reviewed, verified synthetic import mapping. Sibling/client/other-user records remain unverified. Ambiguous archives remain local with a visible count.

Record the desktop commit, CI run/artifact, server commit, staging URL, runtime keychain/native callback results, recording ID and receiving identity in Connections #10/#11. The oldest compatible client is pending a passing paired pilot; this branch is only the candidate.

[Connections #12](https://github.com/Ntrakiyski/connections/issues/12) additionally requires production-shaped staging, provider/mail sentinel checks, backup restoration and rollback/forward reconciliation. Production cutover remains operator-owned. Keep the tracker and dependent issues open until their acceptance evidence is reviewed after merge.
