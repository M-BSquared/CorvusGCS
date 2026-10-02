"use strict";

/**
 * Runs every frontend test: tests/*.js, then plugins/<id>/tests/*.js for each
 * plugin folder that is there. A plugin that is not installed has no tests to
 * run, so a missing folder is never a failure.
 *
 * Each file runs in its own node process, like `node <file>` by hand, with
 * CORVUS_ROOT set to the repository so a plugin's tests find src/ wherever
 * the plugin is kept.
 *
 * Fails when any file fails, and when there is nothing at all to run (a
 * pattern that matches nothing must not pass by running nothing).
 *
 *   node tools/frontend_tests.js            run them all
 *   node tools/frontend_tests.js --list     only print what would run
 */

const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const ROOT = path.resolve(__dirname, "..");

function jsFiles(dir) {
  try {
    return fs.readdirSync(dir, { withFileTypes: true })
      .filter((e) => e.isFile() && e.name.endsWith(".js"))
      .map((e) => path.join(dir, e.name))
      .sort();
  } catch (_) {
    return [];
  }
}

function testFiles(root) {
  const files = jsFiles(path.join(root, "tests"));
  let plugins = [];
  try {
    plugins = fs.readdirSync(path.join(root, "plugins"), { withFileTypes: true })
      .filter((e) => e.isDirectory())
      .map((e) => e.name)
      .sort();
  } catch (_) {
    plugins = [];
  }
  plugins.forEach((id) => files.push(...jsFiles(path.join(root, "plugins", id, "tests"))));
  return files;
}

function main() {
  const files = testFiles(ROOT);
  if (process.argv.includes("--list")) {
    files.forEach((f) => console.log(path.relative(ROOT, f)));
    return 0;
  }
  if (!files.length) {
    console.error("no frontend test files found under tests/ or plugins/*/tests/");
    return 1;
  }
  const failed = [];
  files.forEach((file) => {
    const name = path.relative(ROOT, file);
    console.log(`--- ${name}`);
    const run = spawnSync(process.execPath, [file], {
      stdio: "inherit",
      env: { ...process.env, CORVUS_ROOT: ROOT },
    });
    if (run.status !== 0) failed.push(name);
  });
  if (failed.length) {
    console.error(`\n${failed.length} of ${files.length} frontend test files failed:`);
    failed.forEach((f) => console.error(`  ${f}`));
    return 1;
  }
  console.log(`\n${files.length} frontend test files passed`);
  return 0;
}

if (require.main === module) process.exitCode = main();

module.exports = { testFiles };
