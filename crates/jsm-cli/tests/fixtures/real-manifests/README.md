# Real npm manifest regression corpus

These frozen `package.json` snapshots were copied from installed public npm packages; tests do not require network access. They exercise both scoped and unscoped names, dependency and peer-dependency sections, scripts, bins, engines, exports, package type, and common metadata fields.

| Fixture | Package version | Public package |
|---|---:|---|
| `inquirer-core.package.json` | `@inquirer/core` 9.2.1 | [npm](https://www.npmjs.com/package/@inquirer/core/v/9.2.1) |
| `ansi-regex.package.json` | `ansi-regex` 5.0.1 | [npm](https://www.npmjs.com/package/ansi-regex/v/5.0.1) |
| `commander.package.json` | `commander` 13.1.0 | [npm](https://www.npmjs.com/package/commander/v/13.1.0) |
| `debug.package.json` | `debug` 4.4.3 | [npm](https://www.npmjs.com/package/debug/v/4.4.3) |
| `ms.package.json` | `ms` 2.1.3 | [npm](https://www.npmjs.com/package/ms/v/2.1.3) |
| `semver.package.json` | `semver` 7.8.5 | [npm](https://www.npmjs.com/package/semver/v/7.8.5) |

The regression test parses each snapshot, adds one dependency through the minimal-diff editor, reparses the result, and verifies that all pre-existing manifest data remains unchanged.
