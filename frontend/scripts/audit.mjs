import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";

const LEVELS = ["info", "low", "moderate", "high", "critical"];
const THRESHOLD = LEVELS.indexOf("moderate");

const allowlist = JSON.parse(
  readFileSync(new URL("./audit-allowlist.json", import.meta.url), "utf8"),
);

const result = spawnSync("npm", ["audit", "--omit=dev", "--json"], {
  encoding: "utf8",
  maxBuffer: 64 * 1024 * 1024,
});
let report;
try {
  report = JSON.parse(result.stdout);
} catch {
  console.error("npm audit did not return JSON:\n", result.stdout, result.stderr);
  process.exit(2);
}
if (report.error) {
  console.error("npm audit failed:", report.error);
  process.exit(2);
}

// Collect concrete advisories (objects in `via`), keyed by GHSA id.
const findings = new Map();
for (const vuln of Object.values(report.vulnerabilities ?? {})) {
  for (const via of vuln.via) {
    if (typeof via !== "object") continue;
    if (LEVELS.indexOf(via.severity) < THRESHOLD) continue;
    const id = via.url?.split("/").pop() ?? String(via.source);
    const entry = findings.get(id) ?? { ...via, id, packages: new Set() };
    entry.packages.add(via.name);
    findings.set(id, entry);
  }
}

const today = new Date().toISOString().slice(0, 10);
const errors = [];

for (const [id, f] of findings) {
  const allowed = allowlist[id];
  const label = `${id} (${f.severity}) ${[...f.packages].join(", ")} ${f.range}: ${f.title}`;
  if (!allowed) {
    errors.push(`NOT ALLOWLISTED  ${label}\n    ${f.url}`);
  } else if (allowed.expires < today) {
    errors.push(`ALLOWLIST EXPIRED ${allowed.expires}  ${label}\n    re-review: ${f.url}`);
  } else {
    console.log(`allowlisted until ${allowed.expires}: ${label}\n    reason: ${allowed.reason}`);
  }
}

for (const id of Object.keys(allowlist)) {
  if (!findings.has(id)) {
    errors.push(`STALE ALLOWLIST ENTRY ${id}: no longer reported; remove it from audit-allowlist.json`);
  }
}

if (errors.length) {
  console.error(`\n${errors.join("\n")}`);
  process.exit(1);
}
console.log(`\nnpm audit: ${findings.size} finding(s), all allowlisted.`);
