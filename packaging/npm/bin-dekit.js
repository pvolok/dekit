#!/usr/bin/env node
'use strict';

const path = require('path');
const { spawnSync } = require('child_process');

// Platform package names follow @dekit/dekit-<platform>-<arch>, using the exact
// values Node reports (darwin/linux/win32 and x64/arm64). npm only installs the
// optional dependency matching this host, so require.resolve finds it here and
// fails cleanly on platforms we do not ship.
const pkg = `@dekit/dekit-${process.platform}-${process.arch}`;
const binName = process.platform === 'win32' ? 'dekit.exe' : 'dekit';

let binary;
try {
  const packageJson = require.resolve(`${pkg}/package.json`);
  binary = path.join(path.dirname(packageJson), 'bin', binName);
} catch (error) {
  console.error(`dekit: no prebuilt binary for ${process.platform} ${process.arch} (missing ${pkg})`);
  console.error('dekit: reinstall with npm install -g dekit, or see https://github.com/pvolok/dekit');
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' });

if (result.error) {
  console.error(`dekit: failed to run ${binary}: ${result.error.message}`);
  process.exit(1);
}

if (result.signal) {
  process.kill(process.pid, result.signal);
} else {
  process.exit(result.status == null ? 1 : result.status);
}
