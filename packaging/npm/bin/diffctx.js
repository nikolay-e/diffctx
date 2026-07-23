#!/usr/bin/env node
'use strict';

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const binaryName = process.platform === 'win32' ? 'diffctx.exe' : 'diffctx';
const binaryPath = path.join(__dirname, binaryName);

if (!fs.existsSync(binaryPath)) {
  console.error(
    'diffctx: the native binary is missing — the postinstall download did not run or failed.\n' +
      'Reinstall with "npm install diffctx", or use "pip install diffctx" / "cargo install diffctx".'
  );
  process.exit(1);
}

const result = spawnSync(binaryPath, process.argv.slice(2), { stdio: 'inherit' });
if (result.error) {
  console.error(`diffctx: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status === null ? 1 : result.status);
