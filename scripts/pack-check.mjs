// What a user gets from npm, end to end: assembles the launcher and this platform's binary
// package, packs both like `npm publish` would, installs the tarballs into a temporary global
// prefix and runs the installed `magine`. Runs on Windows, macOS and Linux (CI does all three).
//
// node scripts/pack-check.mjs            builds with `cargo build --release` first
// MAGINE_BIN=<path> node scripts/...     uses a binary you built yourself
import { execFileSync, spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { buildNpm, cargoVersion, TARGETS } from './build-npm.mjs'

const root = fileURLToPath(new URL('..', import.meta.url))
const win = process.platform === 'win32'
const npm = win ? 'npm.cmd' : 'npm'
const run = (cmd, args, opts = {}) => execFileSync(cmd, args, { cwd: root, encoding: 'utf8', shell: win, ...opts })
const fail = (msg) => {
  console.error(`pack-check: ${msg}`)
  process.exit(1)
}

const HOST = {
  'win32 x64': 'x86_64-pc-windows-msvc',
  'win32 arm64': 'aarch64-pc-windows-msvc',
  'darwin x64': 'x86_64-apple-darwin',
  'darwin arm64': 'aarch64-apple-darwin',
  'linux x64': 'x86_64-unknown-linux-musl',
  'linux arm64': 'aarch64-unknown-linux-musl',
}[`${process.platform} ${process.arch}`]
if (!HOST) fail(`no npm package for ${process.platform} ${process.arch}`)

let bin = process.env.MAGINE_BIN
if (!bin) {
  run('cargo', ['build', '--release', '--locked'], { stdio: 'inherit' })
  bin = join(process.env.CARGO_TARGET_DIR || join(root, 'target'), 'release', win ? 'magine.exe' : 'magine')
}
if (!existsSync(bin)) fail(`no binary at ${bin}`)

const work = mkdtempSync(join(tmpdir(), 'magine-pack-'))
try {
  const version = cargoVersion()
  const built = buildNpm({ bins: { [HOST]: bin }, out: join(work, 'npm') })
  const pkgDir = built.platforms[TARGETS[HOST].pkg]
  const pack = (dir) => JSON.parse(run(npm, ['pack', '--json', '--pack-destination', work], { cwd: dir }))[0]
  const main = pack(built.main)
  const plat = pack(pkgDir)
  const files = (p) => p.files.map((f) => f.path).sort()
  console.log(`pack-check: ${main.filename} (${(main.size / 1024).toFixed(1)} KB): ${files(main).join(', ')}`)
  console.log(`pack-check: ${plat.filename} (${(plat.size / 1024 / 1024).toFixed(2)} MB): ${files(plat).join(', ')}`)
  for (const must of ['bin/magine.js', 'README.md', 'LICENSE', 'package.json']) if (!files(main).includes(must)) fail(`magine is missing ${must}`)
  if (files(main).some((f) => !/^(bin\/magine\.js|README\.md|LICENSE|package\.json)$/.test(f))) fail(`unexpected files in magine: ${files(main)}`)
  const exe = win ? 'magine.exe' : 'magine'
  if (!files(plat).includes(exe)) fail(`${plat.name} is missing ${exe}`)
  if (main.version !== version || plat.version !== version) fail(`versions ${main.version}/${plat.version} != ${version}`)

  const prefix = join(work, 'global')
  // Optional dependencies are not published yet; the local platform tarball stands in for the registry.
  run(npm, ['install', '--global', '--prefix', prefix, '--omit=optional', '--no-audit', '--no-fund', join(work, plat.filename), join(work, main.filename)])
  const cmd = win ? join(prefix, 'magine.cmd') : join(prefix, 'bin', 'magine')
  if (!existsSync(cmd)) fail(`the installed command is missing: ${cmd}`)
  const env = { ...process.env, MAGINE_CONFIG_DIR: join(work, 'config'), MAGINE_NO_BROWSER: '1', NO_COLOR: '1' }
  delete env.MAGINE_BINARY
  delete env.MAGINE_API_KEY
  const exec = (args) => spawnSync(cmd, args, { encoding: 'utf8', shell: win, env })
  const v = exec(['--version'])
  if (v.status !== 0 || v.stdout.trim() !== version) fail(`"magine --version" printed ${JSON.stringify(v.stdout)} (exit ${v.status}) ${v.stderr}`)
  const guide = exec(['guide', '--json'])
  if (guide.status !== 0 || JSON.parse(guide.stdout).commands.length < 20) fail(`"magine guide --json" failed: ${guide.stderr}`)
  const who = exec(['whoami', '--json'])
  if (who.status !== 4) fail(`"magine whoami" without a sign-in should exit 4, got ${who.status}: ${who.stderr}`)
  const bad = exec(['agents', 'strat'])
  if (bad.status !== 2) fail(`a wrong command should exit 2, got ${bad.status}`)
  console.log(`pack-check: npm install -g magine works (magine ${version}) on ${process.platform}/${process.arch}`)
} finally {
  rmSync(work, { recursive: true, force: true })
}
