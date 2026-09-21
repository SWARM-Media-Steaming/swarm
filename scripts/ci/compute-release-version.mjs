#!/usr/bin/env node
// Compute the SWARM Server release version from the VERSION file and git
// history.
//
// `VERSION` (repository root) holds the product version as of the commit that
// last changed it: one `MAJOR.MINOR.PATCH` line (blank lines and `#` comments
// are ignored). Every later commit on the branch's first-parent line adds one
// to the patch, so each push to `main` — a direct commit, or the merge commit
// a promotion PR lands as, however many issue commits it carries — is exactly
// one patch. Minor and major change only by editing VERSION. See
// .claude/rules/versioning.md.
//
//   node scripts/ci/compute-release-version.mjs --channel stable
//   node scripts/ci/compute-release-version.mjs --channel beta --run-number 42
//
// Needs full history (`fetch-depth: 0` in CI); a shallow clone cannot be
// counted. Note the count is of *all* first-parent commits on the branch, so a
// push that does not touch the server (and so publishes no release) still uses
// up a patch number; published versions therefore skip, but never repeat or
// go backwards.

import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

export const VERSION_FILE = "VERSION";
const VERSION_RE = /^(\d+)\.(\d+)\.(\d+)$/;

function git(repo, ...args) {
  try {
    return execFileSync("git", ["-C", repo, ...args], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
  } catch (error) {
    const detail = String(error.stderr || error.message).trim();
    throw new Error(`git ${args.join(" ")} failed: ${detail}`);
  }
}

export function parseVersionFile(text) {
  const entries = text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"));
  if (entries.length !== 1) {
    throw new Error(`${VERSION_FILE} must contain exactly one MAJOR.MINOR.PATCH line`);
  }
  const match = VERSION_RE.exec(entries[0]);
  if (!match) {
    throw new Error(`${VERSION_FILE} has an invalid version: ${JSON.stringify(entries[0])}`);
  }
  return [Number(match[1]), Number(match[2]), Number(match[3])];
}

export function computeVersion({ repo = ".", channel = "stable", runNumber, ref = "HEAD" } = {}) {
  if (git(repo, "rev-parse", "--is-shallow-repository") === "true") {
    throw new Error("the patch number counts commits; check out full history (fetch-depth: 0)");
  }
  let text;
  try {
    text = git(repo, "show", `${ref}:${VERSION_FILE}`);
  } catch {
    throw new Error(`${VERSION_FILE} does not exist at ${ref}`);
  }
  const [major, minor, patch] = parseVersionFile(text);
  const changedAt = git(repo, "log", "--first-parent", "-1", "--format=%H", ref, "--", VERSION_FILE);
  if (!changedAt) {
    throw new Error(`${VERSION_FILE} has no history on ${ref}`);
  }
  const since = Number(git(repo, "rev-list", "--first-parent", "--count", `${changedAt}..${ref}`));
  const version = `${major}.${minor}.${patch + since}`;
  if (channel === "beta") {
    if (runNumber === undefined || runNumber === null || Number.isNaN(runNumber)) {
      throw new Error("--run-number is required for the beta channel");
    }
    return `${version}-beta.${runNumber}`;
  }
  if (channel !== "stable") {
    throw new Error(`unknown channel ${JSON.stringify(channel)} (expected stable or beta)`);
  }
  return version;
}

function parseArgs(argv) {
  const options = {};
  for (let i = 0; i < argv.length; i += 2) {
    const flag = argv[i];
    const value = argv[i + 1];
    if (value === undefined || !flag.startsWith("--")) {
      throw new Error(`usage: compute-release-version.mjs [--channel stable|beta] [--run-number N] [--repo DIR] [--ref REF]`);
    }
    if (flag === "--channel") options.channel = value;
    else if (flag === "--run-number") options.runNumber = Number(value);
    else if (flag === "--repo") options.repo = value;
    else if (flag === "--ref") options.ref = value;
    else throw new Error(`unknown option ${flag}`);
  }
  return options;
}

function isDirectRun() {
  const entry = process.argv[1];
  return Boolean(entry) && import.meta.url === pathToFileURL(entry).href;
}

if (isDirectRun()) {
  try {
    console.log(computeVersion(parseArgs(process.argv.slice(2))));
  } catch (error) {
    console.error(`compute-release-version: ${error.message}`);
    process.exit(1);
  }
}
