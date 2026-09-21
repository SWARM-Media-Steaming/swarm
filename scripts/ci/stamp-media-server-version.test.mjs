import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { applyReleaseVersion, wixVersion } from "./stamp-media-server-version.mjs";

test("plain stable versions stay three numeric fields", () => {
  assert.equal(wixVersion("0.1.0"), "0.1.0");
  assert.equal(wixVersion("1.2.3"), "1.2.3");
});

test("beta and main suffixes become the MSI build field", () => {
  assert.equal(wixVersion("0.1.0-beta.25"), "0.1.0.25");
  assert.equal(wixVersion("0.1.0+main.26"), "0.1.0.26");
  assert.equal(wixVersion("0.2.3-beta.1"), "0.2.3.1");
  assert.equal(wixVersion("0.1.0+main.65535"), "0.1.0.65535");
});

test("build counters above 65535 fold into the WiX range", () => {
  assert.equal(wixVersion("0.1.0-beta.65536"), "0.1.0.1");
  assert.equal(wixVersion("0.1.0+main.65537"), "0.1.0.2");
  assert.equal(wixVersion("0.1.0-beta.131070"), "0.1.0.65535");
});

test("rejects versions WiX cannot represent", () => {
  assert.throws(() => wixVersion("beta.1"), /cannot derive/);
  assert.throws(() => wixVersion("0.1.0-beta"), /no numeric/);
  assert.throws(() => wixVersion("256.0.0"), /exceeds WiX field limits/);
  assert.throws(() => wixVersion("0.256.0"), /exceeds WiX field limits/);
  assert.throws(() => wixVersion("0.1.65536"), /exceeds WiX field limits/);
});

test("stamping keeps the semver and only sets the MSI version", () => {
  const conf = {
    productName: "SWARM Server",
    version: "0.1.0",
    bundle: { active: true, macOS: { minimumSystemVersion: "10.15" } },
    plugins: { updater: { pubkey: "keep-me" } },
  };
  applyReleaseVersion(conf, "0.1.0-beta.25");
  assert.equal(conf.version, "0.1.0-beta.25");
  assert.equal(conf.bundle.windows.wix.version, "0.1.0.25");
  assert.equal(conf.bundle.active, true);
  assert.equal(conf.bundle.macOS.minimumSystemVersion, "10.15");
  assert.equal(conf.plugins.updater.pubkey, "keep-me");
});

test("stamping preserves an existing WiX upgrade code", () => {
  const conf = {
    version: "0.1.0",
    bundle: { windows: { wix: { upgradeCode: "6ba7b811-9dad-11d1-80b4-00c04fd430c8" } } },
  };
  applyReleaseVersion(conf, "0.1.0+main.26");
  assert.equal(conf.bundle.windows.wix.version, "0.1.0.26");
  assert.equal(conf.bundle.windows.wix.upgradeCode, "6ba7b811-9dad-11d1-80b4-00c04fd430c8");
});

test("the CLI stamps a tauri.conf.json copy", () => {
  const dir = mkdtempSync(join(tmpdir(), "stamp-media-server-"));
  try {
    const source = JSON.parse(
      readFileSync(fileURLToPath(new URL("../../apps/server/tauri.conf.json", import.meta.url)), "utf8"),
    );
    const configPath = join(dir, "tauri.conf.json");
    writeFileSync(configPath, `${JSON.stringify(source, null, 2)}\n`);
    const result = spawnSync(
      process.execPath,
      [
        fileURLToPath(new URL("./stamp-media-server-version.mjs", import.meta.url)),
        "0.1.0+main.26",
        configPath,
      ],
      { encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    const stamped = JSON.parse(readFileSync(configPath, "utf8"));
    assert.equal(stamped.version, "0.1.0+main.26");
    assert.equal(stamped.bundle.windows.wix.version, "0.1.0.26");
    assert.equal(stamped.productName, source.productName);
    assert.equal(stamped.plugins.updater.pubkey, source.plugins.updater.pubkey);
    assert.equal(stamped.identifier, source.identifier);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
