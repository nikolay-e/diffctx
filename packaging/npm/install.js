#!/usr/bin/env node
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const https = require('https');
const { execFileSync } = require('child_process');

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

function extract(archivePath, archiveKind, destinationDir) {
  if (archiveKind === 'zip') {
    execFileSync(
      'powershell',
      ['-NoProfile', '-Command', `Expand-Archive -LiteralPath '${archivePath}' -DestinationPath '${destinationDir}' -Force`],
      { stdio: 'inherit' }
    );
    return;
  }
  execFileSync('tar', ['-xzf', archivePath, '-C', destinationDir], { stdio: 'inherit' });
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
  fs.copyFileSync(path.join(workDir, binaryName), path.join(binDir, binaryName));
  if (process.platform !== 'win32') {
    fs.chmodSync(path.join(binDir, binaryName), 0o755);
  }
  fs.rmSync(workDir, { recursive: true, force: true });
}

main().catch((error) => {
  console.error(`diffctx: ${error.message}`);
  process.exit(1);
});
