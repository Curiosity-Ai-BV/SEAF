#!/usr/bin/env node

"use strict";

const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const fixtureFiles = [
  "seaf.ticket.yaml",
  "ollama-acceptance.txt",
  "ollama-native-check.sh",
];

function temporaryDirectory(t) {
  const canonicalTemp = fs.realpathSync.native(os.tmpdir());
  const directory = fs.mkdtempSync(
    path.join(canonicalTemp, "seaf-ollama-boundary."),
  );
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return directory;
}

test(
  "harness rejects a substituted fixture root without executing its validator",
  { skip: process.env.SEAF_NESTED_PREFLIGHT_BOUNDARY === "1" },
  (t) => {
    const root = temporaryDirectory(t);
    const scripts = path.join(root, "scripts");
    const fixtureRoot = path.join(
      root,
      "fixtures/packaged-external-golden-path",
    );
    const maliciousRoot = path.join(root, "malicious-ollama");
    fs.mkdirSync(scripts);
    fs.mkdirSync(fixtureRoot, { recursive: true });
    fs.mkdirSync(maliciousRoot);

    for (const name of [
      "test-packaged-external-golden-path.sh",
      "build-release-artifact.sh",
      "test-packaged-ollama-fixture-preflight.cjs",
      "test-packaged-ollama-fixture-trust-boundary.cjs",
      "validate-packaged-ollama-fixture.cjs",
    ]) {
      fs.copyFileSync(path.join(__dirname, name), path.join(scripts, name));
    }
    fs.chmodSync(
      path.join(scripts, "test-packaged-external-golden-path.sh"),
      0o755,
    );
    fs.chmodSync(path.join(scripts, "build-release-artifact.sh"), 0o755);

    const repositoryFixtureRoot = path.resolve(
      __dirname,
      "../fixtures/packaged-external-golden-path",
    );
    for (const name of [
      "project.txt",
      "golden-path-check.sh",
      "seaf.ticket.yaml",
      "seaf.evals.yaml.in",
    ]) {
      fs.copyFileSync(
        path.join(repositoryFixtureRoot, name),
        path.join(fixtureRoot, name),
      );
    }
    fs.chmodSync(path.join(fixtureRoot, "golden-path-check.sh"), 0o755);
    const sourceOllamaRoot = path.join(repositoryFixtureRoot, "ollama");
    for (const name of fixtureFiles) {
      fs.copyFileSync(
        path.join(sourceOllamaRoot, name),
        path.join(maliciousRoot, name),
      );
    }

    const marker = path.join(root, "malicious-validator-executed");
    fs.writeFileSync(
      path.join(maliciousRoot, "validate-fixture.cjs"),
      [
        'const fs = require("node:fs");',
        'fs.writeFileSync(process.env.SEAF_MALICIOUS_VALIDATOR_MARKER, "executed\\n");',
      ].join("\n"),
    );
    fs.symlinkSync(maliciousRoot, path.join(fixtureRoot, "ollama"), "dir");

    const gitEnvironment = {
      ...process.env,
      GIT_CONFIG_NOSYSTEM: "1",
      GIT_CONFIG_GLOBAL: "/dev/null",
    };
    for (const args of [
      ["init", "-q"],
      ["config", "user.name", "SEAF Fixture Boundary"],
      ["config", "user.email", "fixture-boundary@seaf.invalid"],
      ["add", "-A"],
      ["commit", "-q", "-m", "Create substituted fixture boundary"],
    ]) {
      const git = spawnSync("git", args, {
        cwd: root,
        encoding: "utf8",
        env: gitEnvironment,
      });
      assert.equal(git.status, 0, `git fixture setup failed: ${git.stderr}`);
    }

    const result = spawnSync(
      "bash",
      [path.join(scripts, "test-packaged-external-golden-path.sh")],
      {
        cwd: root,
        encoding: "utf8",
        env: {
          ...process.env,
          SEAF_MALICIOUS_VALIDATOR_MARKER: marker,
          SEAF_NESTED_PREFLIGHT_BOUNDARY: "1",
        },
        timeout: 10_000,
      },
    );
    assert.notEqual(
      result.status,
      0,
      "substituted fixture root must fail closed",
    );
    assert.equal(
      result.signal,
      null,
      "substituted fixture rejection must be bounded",
    );
    assert.equal(
      fs.existsSync(marker),
      false,
      `malicious validator executed: ${result.stdout}${result.stderr}`,
    );
    assert.match(
      `${result.stdout}${result.stderr}`,
      /fixture root or its parent is symlinked or unsafe/,
    );
    assert.doesNotMatch(
      `${result.stdout}${result.stderr}`,
      /==> Build and install verified packaged CLI/,
    );
  },
);
