#!/usr/bin/env node
// Stamp the release version into apps/server/tauri.conf.json.
//
// The app version stays the workflow's semver (`0.1.0-beta.<n>` on ai-main,
// `0.1.0+main.<n>` when a stable publish reuses the Cargo version). WiX
// ProductVersion cannot carry those suffixes: it must be numeric
// major.minor.patch[.build], and the last two fields must be <= 65535.
// bundle.windows.wix.version is only the MSI product version. NSIS, the
// updater, and artifact filenames keep the real semver.
//
// Windows Installer ignores a fourth version field when comparing upgrades.
// Tauri's WiX template allows same-version upgrades, so a counter that no
// longer fits is folded into 1..65535 instead of failing the bundle.

import { readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const VERSION_RE =
  /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?$/;
const WIX_BUILD_MAX = 65535;

export function wixVersion(version) {
  const match = VERSION_RE.exec(version);
  if (!match) {
    throw new Error(`cannot derive a WiX version from ${JSON.stringify(version)}`);
  }
  const major = Number(match[1]);
  const minor = Number(match[2]);
  const patch = Number(match[3]);
  if (major > 255 || minor > 255 || patch > WIX_BUILD_MAX) {
    throw new Error(
      `version ${version} exceeds WiX field limits (major/minor <= 255, patch <= ${WIX_BUILD_MAX})`,
    );
  }
  const suffix = match[5] || match[4];
  if (!suffix) {
    return `${major}.${minor}.${patch}`;
  }
  const numbers = suffix.match(/\d+/g);
  if (!numbers) {
    throw new Error(
      `version ${version} has no numeric pre-release or build counter for the MSI version`,
    );
  }
  let build = Number(numbers[numbers.length - 1]);
  if (!Number.isSafeInteger(build)) {
    throw new Error(`version ${version} has a build counter that is not a safe integer`);
  }
  if (build > WIX_BUILD_MAX) {
    build = ((build - 1) % WIX_BUILD_MAX) + 1;
  }
  return `${major}.${minor}.${patch}.${build}`;
}

export function applyReleaseVersion(conf, version) {
  conf.version = version;
  conf.bundle ??= {};
  conf.bundle.windows ??= {};
  conf.bundle.windows.wix ??= {};
  conf.bundle.windows.wix.version = wixVersion(version);
  return conf;
}

function isDirectRun() {
  const entry = process.argv[1];
  return Boolean(entry) && import.meta.url === pathToFileURL(entry).href;
}

if (isDirectRun()) {
  const version = process.argv[2];
  const configPath = process.argv[3] || "apps/server/tauri.conf.json";
  if (!version) {
    console.error("usage: stamp-media-server-version.mjs <version> [tauri.conf.json]");
    process.exit(1);
  }
  const conf = JSON.parse(readFileSync(configPath, "utf8"));
  applyReleaseVersion(conf, version);
  writeFileSync(configPath, `${JSON.stringify(conf, null, 2)}\n`);
  console.log(`Stamped ${version} (MSI ${conf.bundle.windows.wix.version}) into ${configPath}`);
}
