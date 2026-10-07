#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const npmRoot = execFileSync('npm', ['root', '-g'], { encoding: 'utf8' }).trim();
const requireFromNpm = createRequire(path.join(npmRoot, 'npm', 'package.json'));
const semver = requireFromNpm('semver');
const referenceVersion = requireFromNpm('semver/package.json').version;

const atoms = [
  '*', 'x', '1', '1.2', '1.2.3', '0', '0.0', '0.0.0', '0.0.3',
  '1.2.3-alpha.1', '1.2.3+build.5', 'v1.2.3', '=1.2.3', '1.x', '1.2.*',
  '2.X', '3.4.5-rc.2+build.9',
];
const ranges = new Set([
  '', '*', 'x', '1.2.3 - 2.3.4', '1.2 - 2.3', '1.0.0 || 2.0.0',
  '^1.0.0 || ~2.1.0', '>=1.2.3 <2.0.0', '1.2.3, <2',
  '1 || || 2', '||', '1.2.3 -', '>>1.0.0', '<<2.0.0', '^', '~',
  '1.2.3-beta.1', '>=1.2.3-alpha.1 <1.2.3',
]);
for (const atom of atoms) {
  for (const operator of ['', '^', '~', '~>', '=', '>', '>=', '<', '<=']) {
    ranges.add(`${operator}${atom}`);
  }
}
for (const left of ['1', '1.2', '1.2.3', '1.2.3-alpha.1', '0.0.3', '2.x']) {
  for (const right of ['2', '2.0', '2.0.0', '2.4.0', '3.x']) {
    ranges.add(`${left} - ${right}`);
  }
}
for (const left of ['1.0.0', '^1.2', '1.x', '>=2.0.0']) {
  for (const right of ['2.0.0', '~2.3', '3.x', '<4.0.0']) {
    ranges.add(`${left} || ${right}`);
    ranges.add(`${left} ${right}`);
  }
}

const versions = new Set();
for (let major = 0; major <= 4; major += 1) {
  for (let minor = 0; minor <= 4; minor += 1) {
    for (let patch = 0; patch <= 3; patch += 1) {
      const stable = `${major}.${minor}.${patch}`;
      versions.add(stable);
      versions.add(`${stable}-alpha.1`);
      versions.add(`${stable}-rc.2`);
      versions.add(`${stable}+build.5`);
    }
  }
}
const cases = [...ranges].flatMap((range) => [...versions].map((version) => ({ range, version })));
const input = `${cases.map((item) => JSON.stringify(item)).join('\n')}\n`;
const rust = spawnSync(
  'cargo',
  ['run', '--locked', '-q', '-p', 'jsm-core', '--example', 'semver_probe'],
  { cwd: root, input, encoding: 'utf8', maxBuffer: 128 * 1024 * 1024 },
);
if (rust.error || rust.status !== 0) {
  process.stderr.write(rust.stderr || String(rust.error));
  process.exit(rust.status || 1);
}
const actual = rust.stdout.trimEnd().split('\n').map((line) => JSON.parse(line));
if (actual.length !== cases.length) {
  throw new Error(`Rust probe returned ${actual.length} results for ${cases.length} cases`);
}

const mismatches = [];
const mismatchCountsByRange = new Map();
const mismatchExamplesByRange = new Map();
let validRanges = 0;
for (let index = 0; index < cases.length; index += 1) {
  const item = cases[index];
  let expectedValid = false;
  let expectedMatch = false;
  try {
    expectedValid = semver.validRange(item.range) !== null;
    if (expectedValid) expectedMatch = semver.satisfies(item.version, item.range);
  } catch {
    expectedValid = false;
    expectedMatch = false;
  }
  if (expectedValid) validRanges += 1;
  if (actual[index].valid !== expectedValid || actual[index].matches !== expectedMatch) {
    const mismatch = {
      ...item,
      expected: { valid: expectedValid, matches: expectedMatch },
      actual: actual[index],
    };
    mismatches.push(mismatch);
    mismatchCountsByRange.set(item.range, (mismatchCountsByRange.get(item.range) || 0) + 1);
    const examples = mismatchExamplesByRange.get(item.range) || [];
    if (examples.length < 3) examples.push(mismatch);
    mismatchExamplesByRange.set(item.range, examples);
  }
}
const corpusHash = createHash('sha256').update(input).digest('hex');
const report = {
  schema: 'jsm.semver.differential.v1',
  generated_at: new Date().toISOString(),
  reference: `npm semver ${referenceVersion}`,
  range_count: ranges.size,
  version_count: versions.size,
  case_count: cases.length,
  npm_valid_range_cases: validRanges,
  agreement_cases: cases.length - mismatches.length,
  mismatch_count: mismatches.length,
  agreement_percent: Number((((cases.length - mismatches.length) / cases.length) * 100).toFixed(6)),
  corpus_sha256: corpusHash,
  mismatch_ranges: [...mismatchCountsByRange.entries()].map(([range, count]) => ({
    range,
    count,
    examples: mismatchExamplesByRange.get(range),
  })),
  mismatches: mismatches.slice(0, 100),
};
const reportFlag = process.argv.indexOf('--report');
if (reportFlag >= 0) {
  const reportPath = path.resolve(root, process.argv[reportFlag + 1] || 'docs/semver-differential.json');
  fs.mkdirSync(path.dirname(reportPath), { recursive: true });
  fs.writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
  console.log(`Report: ${path.relative(root, reportPath)}`);
}
console.log(
  `semver differential: ${report.agreement_cases}/${report.case_count} cases agree `
    + `(${report.agreement_percent}%) with ${report.reference}; mismatches=${report.mismatch_count}`,
);
if (mismatches.length > 0) {
  console.log(JSON.stringify(mismatches.slice(0, 12), null, 2));
  process.exitCode = 1;
}
