# Versioning Rules

The published SWARM Server version is computed, not hand-edited. Follow these
rules so it stays a strictly increasing plain semver (`MAJOR.MINOR.PATCH`).

### How the number is made

- `VERSION` (repository root) holds the SWARM Server version **as of the commit
  that last changed it**. It is one `MAJOR.MINOR.PATCH` line; blank lines and
  `#` comments are ignored.
- Every later commit on a branch's first-parent line adds one to the patch.
  On `main` that means every push: a direct commit, or the merge commit an
  `ai-main` → `main` promotion lands as (however many issue commits it
  carries). `ai-main` publishes betas as `<version>-beta.<run>`.
- `scripts/ci/compute-release-version.mjs` does the arithmetic, and
  `.github/workflows/release-media-server.yml` calls it. It needs full git
  history (`fetch-depth: 0`). The workspace `Cargo.toml` and
  `apps/server/tauri.conf.json` versions are placeholders the workflow
  overwrites — do not keep them in sync by hand and do not touch `Cargo.lock`
  for a version.
- This is a monorepo (STUN server, TV clients, docs). The patch counts every
  commit on the branch, but a release publishes only when the server or what it
  depends on changed, so published versions can skip a number. They never
  repeat or go backwards. Releases are tagged `server-v<version>`.

### Who changes `VERSION`

- **Never edit `VERSION` while implementing an issue.** The issue worker owns
  it, and discards any edit an AI makes (committed or not).
- **Minor:** a trusted author labels the issue `minor`. When the worker
  delivers that issue it writes the next minor (`0.1.9` → `0.2.0`) into the
  issue's commit. Use the label for issues that are a meaningful new *server*
  capability, not for TV-client or STUN-only work. Only one bump per release:
  if `ai-main` already carries a bump `main` has not shipped, further `minor`
  issues add none. A `minor` label applied by anyone who is not a trusted
  author is ignored.
- **Major:** deliberate and rare (`0.x` → `1.0.0` when the product is declared
  stable; afterwards, breaking changes only). A human commits it directly; no
  label triggers it.
- A change to `VERSION` restarts the patch at the value written in the file.
  Versions must never go backwards — the updater compares them as semver, and
  installs still on the older `0.1.0+main.<n>` builds rely on the comparator in
  `apps/server/src/gui.rs`, which must stay.

### What counts as a minor

A meaningful new capability or anything users must notice: a new library or
streaming feature, a setting that changes behavior, a config/state change
needing migration. Fixes and internal refactors are patches.
