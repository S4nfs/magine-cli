#!/usr/bin/env node
'use strict'
// Runs the native Magine CLI for this computer. npm installs it as one optional
// dependency per platform (like swc and Biome); only the matching one is downloaded.
const { spawnSync } = require('child_process')
const path = require('path')

const PACKAGES = {
  'win32 x64': '@zeupiter/magine-cli-win32-x64',
  'win32 arm64': '@zeupiter/magine-cli-win32-arm64',
  'darwin x64': '@zeupiter/magine-cli-darwin-x64',
  'darwin arm64': '@zeupiter/magine-cli-darwin-arm64',
  'linux x64': '@zeupiter/magine-cli-linux-x64',
  'linux arm64': '@zeupiter/magine-cli-linux-arm64',
}

function fail(message) {
  process.stderr.write(`magine: ${message}\n`)
  process.exit(1)
}

function binaryPath() {
  if (process.env.MAGINE_BINARY) return process.env.MAGINE_BINARY
  const platform = `${process.platform} ${process.arch}`
  const pkg = PACKAGES[platform]
  if (!pkg) fail(`there is no Magine CLI build for ${platform} yet. See https://magine.cloud/docs#cli`)
  const exe = process.platform === 'win32' ? 'magine.exe' : 'magine'
  try {
    return path.join(path.dirname(require.resolve(`${pkg}/package.json`)), exe)
  } catch {
    fail(`the binary for ${platform} (${pkg}) is not installed. Reinstall with optional dependencies allowed: npm install -g magine`)
  }
}

const bin = binaryPath()
// Ctrl+C reaches the CLI too; it decides how to stop (a wait ends with a message, the run goes on).
process.on('SIGINT', () => {})
const result = spawnSync(bin, process.argv.slice(2), { stdio: 'inherit' })
if (result.error) fail(`could not start ${bin}: ${result.error.message}`)
process.exit(result.status === null ? 1 : result.status)
