#!/usr/bin/env node

"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const {
  validateOllamaReviewFixture,
} = require("../fixtures/packaged-external-golden-path/ollama/validate-fixture.cjs");

const sourceRoot = path.resolve(
  __dirname,
  "../fixtures/packaged-external-golden-path/ollama",
);
const fixtureFiles = [
  "seaf.ticket.yaml",
  "ollama-acceptance.txt",
  "ollama-native-check.sh",
];

function temporaryDirectory(t) {
  const canonicalTemp = fs.realpathSync.native(os.tmpdir());
  const directory = fs.mkdtempSync(
    path.join(canonicalTemp, "seaf-ollama-preflight."),
  );
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return directory;
}

function copyFixture(t) {
  const root = path.join(temporaryDirectory(t), "fixture");
  fs.mkdirSync(root);
  for (const name of fixtureFiles) {
    fs.copyFileSync(path.join(sourceRoot, name), path.join(root, name));
  }
  return root;
}

test("preflight rejects a fixture root reached through a symlinked parent", (t) => {
  const root = copyFixture(t);
  const temporaryRoot = path.dirname(root);
  const realParent = path.join(temporaryRoot, "real-parent");
  const aliasParent = path.join(temporaryRoot, "parent-alias");
  fs.mkdirSync(realParent);
  fs.renameSync(root, path.join(realParent, "fixture"));
  fs.symlinkSync(realParent, aliasParent, "dir");
  assert.throws(
    () => validateOllamaReviewFixture(path.join(aliasParent, "fixture")),
    /symlink|canonical|unsafe/,
  );
});

test("preflight rejects a symlinked fixture root", (t) => {
  const root = copyFixture(t);
  const alias = path.join(path.dirname(root), "fixture-alias");
  fs.symlinkSync(root, alias, "dir");
  assert.throws(
    () => validateOllamaReviewFixture(alias),
    /symlink|canonical|unsafe/,
  );
});

test("preflight accepts the exact trusted fixture", (t) => {
  const root = copyFixture(t);
  assert.doesNotThrow(() => validateOllamaReviewFixture(root));
});

test("preflight rejects a symlinked reviewed file", (t) => {
  const root = copyFixture(t);
  const ticketPath = path.join(root, "seaf.ticket.yaml");
  const external = path.join(path.dirname(root), "external-ticket.yaml");
  fs.renameSync(ticketPath, external);
  fs.symlinkSync(external, ticketPath);
  assert.throws(() => validateOllamaReviewFixture(root), /unsafe|symlink/);
});

test("preflight rejects an oversized reviewed file before reading it", (t) => {
  const root = copyFixture(t);
  fs.writeFileSync(
    path.join(root, "seaf.ticket.yaml"),
    Buffer.alloc(16 * 1024 + 1),
  );
  assert.throws(() => validateOllamaReviewFixture(root), /exceeds bound/);
});

test("preflight rejects a reviewed path replaced after inspection", (t) => {
  const root = copyFixture(t);
  const ticketPath = path.join(root, "seaf.ticket.yaml");
  const ticketBytes = fs.readFileSync(ticketPath);
  let replaced = false;
  assert.throws(
    () =>
      validateOllamaReviewFixture(root, {
        afterOpen(filePath) {
          if (filePath !== ticketPath || replaced) return;
          replaced = true;
          fs.renameSync(ticketPath, `${ticketPath}.old`);
          fs.writeFileSync(ticketPath, ticketBytes);
        },
      }),
    /replaced|identity|changed/,
  );
});

test("preflight reads the review protocol only from the authoritative problem field", (t) => {
  const root = copyFixture(t);
  const ticketPath = path.join(root, "seaf.ticket.yaml");
  const ticket = fs.readFileSync(ticketPath, "utf8");
  const protocolStart = ticket.indexOf(
    " When a Spec Creation provider request has no",
  );
  const protocolEnd = ticket.indexOf("\nresearch_questions:");
  assert.notEqual(protocolStart, -1);
  assert.notEqual(protocolEnd, -1);
  const protocol = ticket
    .slice(protocolStart + 1, protocolEnd)
    .replace(/\n  /g, " ");
  const misplaced = `${ticket.slice(0, protocolStart)}\nprotocol_notes: >-\n  ${protocol}\n${ticket.slice(protocolEnd + 1)}`;
  fs.writeFileSync(ticketPath, misplaced);
  assert.throws(() => validateOllamaReviewFixture(root), /problem|protocol/);
});

test("preflight binds the complete approved native check rather than matching snippets", (t) => {
  const root = copyFixture(t);
  fs.appendFileSync(
    path.join(root, "ollama-native-check.sh"),
    "\n# weakening placeholder\n",
  );
  assert.throws(() => validateOllamaReviewFixture(root), /native check|digest/);
});
