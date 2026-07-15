#!/usr/bin/env node

"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const EXPECTED_PROBLEM = [
  "Change only top-level ollama-acceptance.txt from the exact bytes PENDING plus one trailing newline to the exact bytes SEAF packaged Ollama acceptance passed. plus one trailing newline.",
  "Do not create, delete, rename, or modify any other path.",
  "When a Spec Creation provider request has no revision_context, the specification must propose the exact final bytes SEAF packaged Ollama acceptance draft. followed by one newline.",
  "This proposed final text is intentionally wrong, so Spec Review must request changes.",
  "When a Spec Creation provider request has revision_context, the specification must correct the proposed final bytes to SEAF packaged Ollama acceptance passed. followed by one newline.",
  "The recovered specification must preserve the permitted path, policy scope, command, and acceptance criteria.",
].join(" ");
const EXPECTED_ACCEPTANCE_CRITERIA = [
  "Only top-level ollama-acceptance.txt is changed.",
  "Before the change its exact bytes are PENDING followed by one newline.",
  "After the change its exact bytes are SEAF packaged Ollama acceptance passed. followed by one newline.",
  "No other path is created, deleted, renamed, or modified.",
];
const EXPECTED_NATIVE_CHECK_SHA256 =
  "9532707570283ea4930a487b4bb3cb28c89fc1cf33e6a1e8d7cb925c1f7f5df3";

function sameIdentity(left, right) {
  return left.dev === right.dev && left.ino === right.ino;
}

function sameStableFile(left, right) {
  return (
    sameIdentity(left, right) &&
    left.size === right.size &&
    left.mtimeNs === right.mtimeNs &&
    left.ctimeNs === right.ctimeNs
  );
}

function readBoundedRegularFile(filePath, maximumBytes, afterOpen = () => {}) {
  if (!Number.isSafeInteger(maximumBytes) || maximumBytes < 0) {
    throw new Error("Ollama review fixture byte bound is invalid");
  }
  if (!Number.isInteger(fs.constants.O_NOFOLLOW)) {
    throw new Error("Ollama review fixture validation requires O_NOFOLLOW");
  }
  let descriptor;
  try {
    descriptor = fs.openSync(
      filePath,
      fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW,
    );
  } catch (error) {
    throw new Error(`unsafe Ollama review fixture file: ${filePath}`, {
      cause: error,
    });
  }
  try {
    const before = fs.fstatSync(descriptor, { bigint: true });
    if (!before.isFile()) {
      throw new Error(`unsafe Ollama review fixture file: ${filePath}`);
    }
    if (before.size > BigInt(maximumBytes)) {
      throw new Error(`Ollama review fixture file exceeds bound: ${filePath}`);
    }
    afterOpen(filePath);

    const chunks = [];
    let totalBytes = 0;
    while (totalBytes <= maximumBytes) {
      const available = maximumBytes + 1 - totalBytes;
      const chunk = Buffer.allocUnsafe(Math.min(64 * 1024, available));
      const bytesRead = fs.readSync(descriptor, chunk, 0, chunk.length, null);
      if (bytesRead === 0) break;
      chunks.push(chunk.subarray(0, bytesRead));
      totalBytes += bytesRead;
    }
    if (totalBytes > maximumBytes) {
      throw new Error(`Ollama review fixture file exceeds bound: ${filePath}`);
    }

    const after = fs.fstatSync(descriptor, { bigint: true });
    if (!sameStableFile(before, after) || BigInt(totalBytes) !== after.size) {
      throw new Error(
        `Ollama review fixture file changed while reading: ${filePath}`,
      );
    }
    const finalPath = fs.lstatSync(filePath, { bigint: true });
    if (!finalPath.isFile() || !sameStableFile(after, finalPath)) {
      throw new Error(
        `Ollama review fixture file identity was replaced: ${filePath}`,
      );
    }
    return Buffer.concat(chunks, totalBytes);
  } finally {
    fs.closeSync(descriptor);
  }
}

function exactTopLevelBlock(lines, name, headerValue) {
  const header = `${name}:${headerValue ? ` ${headerValue}` : ""}`;
  const matchingFields = lines
    .map((line, index) =>
      line.startsWith(`${name}:`) ? { line, index } : null,
    )
    .filter((entry) => entry !== null);
  if (matchingFields.length !== 1 || matchingFields[0].line !== header) {
    throw new Error(`Ollama ticket must contain one exact ${name} field`);
  }
  const start = matchingFields[0].index + 1;
  let end = start;
  while (
    end < lines.length &&
    (lines[end].startsWith(" ") || lines[end] === "")
  ) {
    end += 1;
  }
  return lines.slice(start, end);
}

function parseAuthoritativeTicketFields(ticket) {
  if (ticket.includes("\r")) {
    throw new Error("Ollama ticket must use canonical LF newlines");
  }
  const lines = ticket.endsWith("\n")
    ? ticket.slice(0, -1).split("\n")
    : ticket.split("\n");
  const problemLines = exactTopLevelBlock(lines, "problem", ">-");
  if (
    problemLines.length === 0 ||
    problemLines.some((line) => !/^  \S/.test(line))
  ) {
    throw new Error(
      "Ollama ticket problem field is not one canonical folded scalar",
    );
  }
  const problem = problemLines.map((line) => line.slice(2)).join(" ");

  const criterionLines = exactTopLevelBlock(lines, "acceptance_criteria", "");
  if (
    criterionLines.length === 0 ||
    criterionLines.some((line) => !/^  - \S/.test(line))
  ) {
    throw new Error(
      "Ollama ticket acceptance_criteria field is not one canonical list",
    );
  }
  const acceptanceCriteria = criterionLines.map((line) => line.slice(4));
  return { problem, acceptanceCriteria };
}

function canonicalFixtureRoot(fixtureRoot) {
  if (typeof fixtureRoot !== "string" || fixtureRoot.length === 0) {
    throw new Error("Ollama review fixture root is missing");
  }
  const resolved = path.resolve(fixtureRoot);
  const canonical = fs.realpathSync.native(resolved);
  const rootStat = fs.lstatSync(resolved, { bigint: true });
  if (canonical !== resolved || !rootStat.isDirectory()) {
    throw new Error(
      "Ollama review fixture root or its parent is symlinked or unsafe",
    );
  }
  return { canonical, rootStat };
}

function validateOllamaReviewFixture(fixtureRoot, hooks = {}) {
  const { canonical: root, rootStat: initialRootStat } =
    canonicalFixtureRoot(fixtureRoot);
  const rootDescriptor = fs.openSync(
    root,
    fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW,
  );
  try {
    const openedRootStat = fs.fstatSync(rootDescriptor, { bigint: true });
    if (
      !openedRootStat.isDirectory() ||
      !sameIdentity(initialRootStat, openedRootStat)
    ) {
      throw new Error(
        "Ollama review fixture root identity changed before validation",
      );
    }
    const inspect = hooks.afterOpen ?? (() => {});
    function fixturePath(name) {
      const candidate = path.resolve(root, name);
      if (path.dirname(candidate) !== root) {
        throw new Error(`Ollama review fixture path escaped its root: ${name}`);
      }
      return candidate;
    }
    const ticketBytes = readBoundedRegularFile(
      fixturePath("seaf.ticket.yaml"),
      16 * 1024,
      inspect,
    );
    const initialBytes = readBoundedRegularFile(
      fixturePath("ollama-acceptance.txt"),
      64,
      inspect,
    );
    const checkBytes = readBoundedRegularFile(
      fixturePath("ollama-native-check.sh"),
      16 * 1024,
      inspect,
    );

    const ticket = ticketBytes.toString("utf8");
    if (!Buffer.from(ticket, "utf8").equals(ticketBytes)) {
      throw new Error("Ollama ticket is not valid UTF-8");
    }
    const fields = parseAuthoritativeTicketFields(ticket);
    if (fields.problem !== EXPECTED_PROBLEM) {
      throw new Error("Ollama ticket authoritative problem protocol changed");
    }
    if (
      JSON.stringify(fields.acceptanceCriteria) !==
      JSON.stringify(EXPECTED_ACCEPTANCE_CRITERIA)
    ) {
      throw new Error(
        "Ollama ticket authoritative acceptance criteria changed",
      );
    }
    if (!initialBytes.equals(Buffer.from("PENDING\n", "utf8"))) {
      throw new Error("Ollama fixture initial bytes changed");
    }
    const nativeCheckDigest = crypto
      .createHash("sha256")
      .update(checkBytes)
      .digest("hex");
    if (nativeCheckDigest !== EXPECTED_NATIVE_CHECK_SHA256) {
      throw new Error("Ollama fixture native check digest changed");
    }

    const finalRootStat = fs.lstatSync(root, { bigint: true });
    const finalOpenedRootStat = fs.fstatSync(rootDescriptor, { bigint: true });
    if (
      !finalRootStat.isDirectory() ||
      !sameIdentity(openedRootStat, finalRootStat) ||
      !sameIdentity(openedRootStat, finalOpenedRootStat) ||
      fs.realpathSync.native(root) !== root
    ) {
      throw new Error("Ollama review fixture root identity was replaced");
    }
  } finally {
    fs.closeSync(rootDescriptor);
  }
}

if (require.main === module) {
  validateOllamaReviewFixture(process.argv[2]);
}

module.exports = {
  parseAuthoritativeTicketFields,
  readBoundedRegularFile,
  validateOllamaReviewFixture,
};
