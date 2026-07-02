#!/usr/bin/env node
import { chmodSync, copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const config = JSON.parse(readFileSync(join(root, 'packaging/platforms.json'), 'utf8'));

function usage() {
  console.error(`usage:
  node packaging/npm/build.mjs pack --archives <dir> --out <dir> --version <version>
  node packaging/npm/build.mjs publish --dir <dir> --dist-tag <tag> --version <version>`);
  process.exit(1);
}

function option(name) {
  const index = process.argv.indexOf(name);
  if (index === -1 || index + 1 >= process.argv.length) {
    return undefined;
  }
  return process.argv[index + 1];
}

function requiredOption(name) {
  const value = option(name);
  if (!value) {
    console.error(`missing required option: ${name}`);
    usage();
  }
  return value;
}

function run(command, args) {
  const result = spawnSync(command, args, { cwd: root, stdio: 'inherit' });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

function writeJson(path, value) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

// `npm pack` of a scoped package @dekit/dekit-linux-x64 produces
// dekit-dekit-linux-x64-<version>.tgz; the root package produces dekit-<version>.tgz.
function packageDirName(name) {
  return name.replace('@', '').replace('/', '-');
}

function npmPack(packageDir, outDir) {
  mkdirSync(outDir, { recursive: true });
  run('npm', ['pack', packageDir, '--pack-destination', outDir]);
}

function extractBinary(archivesDir, platform, workDir) {
  rmSync(workDir, { recursive: true, force: true });
  mkdirSync(workDir, { recursive: true });
  const archive = join(archivesDir, `${config.package}-${platform.target}.${platform.archive}`);
  if (platform.archive === 'zip') {
    run('unzip', ['-o', '-q', archive, '-d', workDir]);
  } else {
    run('tar', ['-xzf', archive, '-C', workDir]);
  }
  return join(workDir, platform.binary);
}

function platformPackage(platform, binary, outDir, version) {
  const workDir = join(outDir, '.work', packageDirName(platform.npmPackage));
  rmSync(workDir, { recursive: true, force: true });
  mkdirSync(join(workDir, 'bin'), { recursive: true });

  copyFileSync(binary, join(workDir, 'bin', platform.binary));
  chmodSync(join(workDir, 'bin', platform.binary), 0o755);
  copyFileSync(join(root, 'LICENSE'), join(workDir, 'LICENSE'));

  writeJson(join(workDir, 'package.json'), {
    name: platform.npmPackage,
    version,
    description: `Prebuilt dekit binary for ${platform.target}`,
    repository: {
      type: 'git',
      url: 'git+https://github.com/pvolok/dekit.git',
    },
    license: 'MIT',
    os: [platform.npmOs],
    cpu: [platform.npmCpu],
    files: ['bin'],
  });

  npmPack(workDir, outDir);
}

function rootPackage(outDir, version) {
  const workDir = join(outDir, '.work', config.package);
  rmSync(workDir, { recursive: true, force: true });
  mkdirSync(join(workDir, 'bin'), { recursive: true });

  copyFileSync(join(root, 'packaging/npm/bin-dekit.js'), join(workDir, 'bin/dekit.js'));
  chmodSync(join(workDir, 'bin/dekit.js'), 0o755);
  copyFileSync(join(root, 'packaging/npm/README.md'), join(workDir, 'README.md'));
  copyFileSync(join(root, 'LICENSE'), join(workDir, 'LICENSE'));

  const optionalDependencies = Object.fromEntries(
    config.platforms.map((platform) => [platform.npmPackage, version]),
  );

  writeJson(join(workDir, 'package.json'), {
    name: config.package,
    version,
    description: 'A scriptable process manager you drive from a CLI, TUI, API, or script',
    repository: {
      type: 'git',
      url: 'git+https://github.com/pvolok/dekit.git',
    },
    license: 'MIT',
    bin: {
      dekit: 'bin/dekit.js',
    },
    files: ['bin', 'README.md'],
    optionalDependencies,
    engines: {
      node: '>=16',
    },
  });

  npmPack(workDir, outDir);
}

function packAll(archivesDir, outDir, version) {
  mkdirSync(outDir, { recursive: true });
  for (const platform of config.platforms) {
    const binary = extractBinary(archivesDir, platform, join(outDir, '.work', 'bin', platform.target));
    platformPackage(platform, binary, outDir, version);
  }
  rootPackage(outDir, version);
}

function isPublished(name, version) {
  const result = spawnSync('npm', ['view', `${name}@${version}`, 'version'], {
    cwd: root,
    encoding: 'utf8',
  });
  return result.status === 0 && result.stdout.trim() === version;
}

// Platform packages must land before the root package, whose optionalDependencies
// pin their exact versions. Re-running after a partial failure skips what is already up.
function publishAll(dir, distTag, version) {
  const packages = [
    ...config.platforms.map((platform) => ({
      name: platform.npmPackage,
      file: `${packageDirName(platform.npmPackage)}-${version}.tgz`,
    })),
    { name: config.package, file: `${config.package}-${version}.tgz` },
  ];

  for (const { name, file } of packages) {
    if (isPublished(name, version)) {
      console.log(`skip ${name}@${version} (already published)`);
      continue;
    }
    run('npm', ['publish', '--provenance', '--access', 'public', '--tag', distTag, join(dir, file)]);
  }
}

const command = process.argv[2];

try {
  if (command === 'pack') {
    packAll(
      resolve(requiredOption('--archives')),
      resolve(requiredOption('--out')),
      requiredOption('--version'),
    );
  } else if (command === 'publish') {
    publishAll(
      resolve(requiredOption('--dir')),
      requiredOption('--dist-tag'),
      requiredOption('--version'),
    );
  } else {
    usage();
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
