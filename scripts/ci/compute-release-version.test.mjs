import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { computeVersion, parseVersionFile } from "./compute-release-version.mjs";

const SCRIPT = fileURLToPath(new URL("./compute-release-version.mjs", import.meta.url));

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "compute-release-version-"));
  const repo = join(root, "repo");
  const git = (...args) =>
    execFileSync("git", ["-C", repo, ...args], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
  execFileSync("git", ["init", "-q", "-b", "main", repo]);
  git("config", "user.name", "test");
  git("config", "user.email", "test@example.invalid");
  let counter = 0;
  return {
    root,
    repo,
    git,
    commit(message = "work") {
      counter += 1;
      writeFileSync(join(repo, `file${counter}.txt`), message);
      git("add", "--all");
      git("commit", "-q", "-m", message);
    },
    setVersion(version) {
      writeFileSync(join(repo, "VERSION"), `# comment\n${version}\n`);
      git("add", "VERSION");
      git("commit", "-q", "-m", `Set version ${version}`);
    },
    version(options = {}) {
      return computeVersion({ repo, ...options });
    },
    cleanup() {
      rmSync(root, { recursive: true, force: true });
    },
  };
}

function withFixture(body) {
  const f = fixture();
  try {
    return body(f);
  } finally {
    f.cleanup();
  }
}

test("the commit that sets the version is that version", () =>
  withFixture((f) => {
    f.commit("before versioning existed");
    f.setVersion("0.1.1");
    assert.equal(f.version(), "0.1.1");
  }));

test("every later commit adds one to the patch", () =>
  withFixture((f) => {
    f.setVersion("0.1.1");
    f.commit();
    assert.equal(f.version(), "0.1.2");
    f.commit();
    f.commit();
    assert.equal(f.version(), "0.1.4");
  }));

test("changing the file restarts the patch at the new value", () =>
  withFixture((f) => {
    f.setVersion("0.1.1");
    f.commit();
    f.setVersion("0.2.0");
    assert.equal(f.version(), "0.2.0");
    f.commit();
    assert.equal(f.version(), "0.2.1");
    f.setVersion("1.0.0");
    assert.equal(f.version(), "1.0.0");
  }));

test("a promotion merge counts once however many commits it carries", () =>
  withFixture((f) => {
    f.setVersion("0.1.1");
    f.git("switch", "-q", "-c", "ai-main");
    for (let i = 0; i < 4; i += 1) f.commit();
    f.git("switch", "-q", "main");
    f.git("merge", "-q", "--no-ff", "-m", "Merge ai-main", "ai-main");
    assert.equal(f.version(), "0.1.2");
  }));

test("a promotion merge that carries a minor bump is the new minor", () =>
  withFixture((f) => {
    f.setVersion("0.1.1");
    f.commit();
    f.git("switch", "-q", "-c", "ai-main");
    f.commit();
    f.setVersion("0.2.0");
    f.commit();
    f.git("switch", "-q", "main");
    f.commit("hotfix on main");
    f.git("merge", "-q", "--no-ff", "-m", "Merge ai-main", "ai-main");
    assert.equal(f.version(), "0.2.0");
    f.commit();
    assert.equal(f.version(), "0.2.1");
  }));

test("the version is strictly increasing across a minor bump", () =>
  withFixture((f) => {
    f.setVersion("0.1.1");
    const seen = [f.version()];
    for (let i = 0; i < 3; i += 1) {
      f.commit();
      seen.push(f.version());
    }
    f.setVersion("0.2.0");
    seen.push(f.version());
    f.commit();
    seen.push(f.version());
    const key = (v) => v.split(".").map(Number);
    for (let i = 1; i < seen.length; i += 1) {
      const [a, b] = [key(seen[i - 1]), key(seen[i])];
      assert.ok(
        b[0] > a[0] || (b[0] === a[0] && (b[1] > a[1] || (b[1] === a[1] && b[2] > a[2]))),
        `${seen[i]} should be greater than ${seen[i - 1]}`,
      );
    }
  }));

test("the beta channel appends the run number", () =>
  withFixture((f) => {
    f.setVersion("0.1.1");
    f.commit();
    assert.equal(f.version({ channel: "beta", runNumber: 42 }), "0.1.2-beta.42");
    assert.throws(() => f.version({ channel: "beta" }), /--run-number is required/);
    assert.throws(() => f.version({ channel: "nightly" }), /unknown channel/);
  }));

test("a named ref is computed independently of the checkout", () =>
  withFixture((f) => {
    f.setVersion("0.1.1");
    f.git("switch", "-q", "-c", "other");
    f.commit();
    f.commit();
    f.git("switch", "-q", "main");
    assert.equal(f.version(), "0.1.1");
    assert.equal(f.version({ ref: "other" }), "0.1.3");
  }));

test("a missing VERSION file is an error", () =>
  withFixture((f) => {
    f.commit();
    assert.throws(() => f.version(), /does not exist/);
  }));

test("malformed VERSION files are errors", () => {
  for (const bad of ["0.1", "v0.1.1", "0.1.1-beta", "one.two.three", ""]) {
    assert.throws(() => parseVersionFile(`${bad}\n`), Error, JSON.stringify(bad));
  }
  assert.throws(() => parseVersionFile("0.1.1\n0.1.2\n"), /exactly one/);
  assert.deepEqual(parseVersionFile("# c\n\n0.1.9\n"), [0, 1, 9]);
});

test("a shallow clone is refused because it cannot be counted", () =>
  withFixture((f) => {
    f.setVersion("0.1.1");
    f.commit();
    f.commit();
    const shallow = join(f.root, "shallow");
    execFileSync("git", ["clone", "-q", "--depth", "1", `file://${f.repo}`, shallow], { stdio: "ignore" });
    assert.throws(() => computeVersion({ repo: shallow }), /full history/);
  }));

test("the command line prints only the version and fails loudly on error", () =>
  withFixture((f) => {
    f.commit();
    const failed = spawnSync("node", [SCRIPT, "--repo", f.repo], { encoding: "utf8" });
    assert.equal(failed.status, 1);
    assert.equal(failed.stdout, "");
    f.setVersion("0.1.1");
    f.commit();
    const ok = spawnSync("node", [SCRIPT, "--repo", f.repo, "--channel", "beta", "--run-number", "7"], {
      encoding: "utf8",
    });
    assert.equal(ok.status, 0);
    assert.equal(ok.stdout, "0.1.2-beta.7\n");
  }));
