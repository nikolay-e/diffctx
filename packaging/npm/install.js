#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const https = require('node:https');
const { execFileSync } = require('node:child_process');

const { version } = require('./package.json');
const checksums = require('./checksums.json');

const TARGETS = {
  'linux-x64': { target: 'x86_64-unknown-linux-gnu', archive: 'tar.gz' },
  'linux-arm64': { target: 'aarch64-unknown-linux-gnu', archive: 'tar.gz' },
  'darwin-arm64': { target: 'aarch64-apple-darwin', archive: 'tar.gz' },
  'win32-x64': { target: 'x86_64-pc-windows-msvc', archive: 'zip' },
};

function selectTarget() {
  const key = `${process.platform}-${process.arch}`;
  const selected = TARGETS[key];
  if (!selected) {
    throw new Error(
      `diffctx has no prebuilt binary for ${key}. ` +
        'Install via pip ("pip install diffctx") or cargo ("cargo install diffctx") instead.'
    );
  }
  return selected;
}

function download(url, destination, redirectsLeft = 5) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { 'user-agent': `diffctx-npm/${version}` } }, (response) => {
        const { statusCode, headers } = response;
        if (statusCode >= 300 && statusCode < 400 && headers.location) {
          response.resume();
          if (redirectsLeft === 0) {
            reject(new Error(`too many redirects for ${url}`));
            return;
          }
          resolve(download(headers.location, destination, redirectsLeft - 1));
          return;
        }
        if (statusCode !== 200) {
          response.resume();
          reject(new Error(`download failed for ${url}: HTTP ${statusCode}`));
          return;
        }
        const file = fs.createWriteStream(destination);
        response.pipe(file);
        file.on('finish', () => file.close(resolve));
        file.on('error', reject);
      })
      .on('error', reject);
  });
}

function verify(archivePath, expectedSha256) {
  const actual = crypto.createHash('sha256').update(fs.readFileSync(archivePath)).digest('hex');
  if (actual !== expectedSha256) {
    throw new Error(`checksum mismatch for ${path.basename(archivePath)}: ${actual} != ${expectedSha256}`);
  }
}

function systemBinary(candidates, name) {
  const found = candidates.find((candidate) => fs.existsSync(candidate));
  if (!found) {
    throw new Error(`cannot locate ${name}; expected one of ${candidates.join(', ')}`);
  }
  return found;
}

function extract(archivePath, archiveKind, destinationDir) {
  if (archiveKind === 'zip') {
    const systemRoot = process.env.SystemRoot || 'C:\\Windows';
    const powershell = systemBinary(
      [
        path.join(systemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe'),
        path.join(systemRoot, 'SysWOW64', 'WindowsPowerShell', 'v1.0', 'powershell.exe'),
      ],
      'powershell.exe'
    );
    execFileSync(
      powershell,
      ['-NoProfile', '-Command', `Expand-Archive -LiteralPath '${archivePath}' -DestinationPath '${destinationDir}' -Force`],
      { stdio: 'inherit' }
    );
    return;
  }
  const tar = systemBinary(['/usr/bin/tar', '/bin/tar'], 'tar');
  execFileSync(tar, ['-xzf', archivePath, '-C', destinationDir], { stdio: 'inherit' });
}

async function main() {
  const { target, archive } = selectTarget();
  const assetName = `diffctx-${version}-${target}.${archive}`;
  const expectedSha256 = checksums[assetName];
  if (!expectedSha256) {
    throw new Error(`no published checksum for ${assetName}`);
  }

  const binDir = path.join(__dirname, 'bin');
  fs.mkdirSync(binDir, { recursive: true });
  const workDir = fs.mkdtempSync(path.join(os.tmpdir(), 'diffctx-'));
  const archivePath = path.join(workDir, assetName);

  const url = `https://github.com/nikolay-e/diffctx/releases/download/v${version}/${assetName}`;
  await download(url, archivePath);
  verify(archivePath, expectedSha256);
  extract(archivePath, archive, workDir);

  const binaryName = process.platform === 'win32' ? 'diffctx.exe' : 'diffctx';
  const extracted = path.join(workDir, binaryName);
  const installed = path.join(binDir, binaryName);
  fs.copyFileSync(extracted, installed);
  // Carry over the archive's mode instead of a hardcoded literal, so the
  // binary stays executable without widening permissions beyond the release.
  fs.chmodSync(installed, fs.statSync(extracted).mode & 0o777);
  fs.rmSync(workDir, { recursive: true, force: true });
}

main().catch((error) => {
  console.error(`diffctx: ${error.message}`);
  process.exit(1);
});
