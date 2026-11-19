// The one-line installers (install/install.sh and install/install.ps1) against a fake npm registry.
// install.sh runs everywhere: on Windows through Git's sh, with a fake `uname` that says Linux and a
// shell script standing in for the Linux binary. install.ps1 runs on Windows with the real binary.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawn, spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:http'
import { join } from 'node:path'
import { gzipSync } from 'node:zlib'
import { fileURLToPath } from 'node:url'
import { CLI, VERSION, tempDir } from './helpers/cli.mjs'

const WINDOWS = process.platform === 'win32'
const INSTALL_SH = readFileSync(new URL('../install/install.sh', import.meta.url))
const INSTALL_PS1 = fileURLToPath(new URL('../install/install.ps1', import.meta.url))

/** A gzipped tar of { name, data, mode } entries, like `npm pack` makes. */
function tarGz(files) {
  const blocks = []
  for (const { name, data, mode = 0o644 } of files) {
    const header = Buffer.alloc(512)
    const octal = (n, width) => n.toString(8).padStart(width - 1, '0') + '\0'
    header.write(name, 0, 100, 'utf8')
    header.write(octal(mode, 8), 100, 'ascii')
    header.write(octal(0, 8), 108, 'ascii')
    header.write(octal(0, 8), 116, 'ascii')
    header.write(octal(data.length, 12), 124, 'ascii')
    header.write(octal(Math.floor(Date.now() / 1000), 12), 136, 'ascii')
    header.fill(' ', 148, 156)
    header.write('0', 156, 'ascii')
    header.write('ustar\x0000', 257, 'ascii')
    let sum = 0
    for (const byte of header) sum += byte
    header.write(sum.toString(8).padStart(6, '0') + '\0 ', 148, 'ascii')
    blocks.push(header, data, Buffer.alloc((512 - (data.length % 512)) % 512))
  }
  blocks.push(Buffer.alloc(1024))
  return gzipSync(Buffer.concat(blocks))
}

const sri = (buf) => `sha512-${createHash('sha512').update(buf).digest('base64')}`

function platformPackage(platform, binName, binData) {
  return tarGz([
    { name: 'package/package.json', data: Buffer.from(JSON.stringify({ name: `@magine/cli-${platform}`, version: VERSION })) },
    { name: `package/${binName}`, data: binData, mode: 0o755 },
  ])
}

/** Serves version manifests and tarballs the way registry.npmjs.org does. */
async function fakeRegistry(tgz, { integrity } = {}) {
  const seen = []
  let base = ''
  const server = createServer((req, res) => {
    seen.push(req.url)
    const manifest = /^\/@magine%2[fF]cli-([a-z0-9]+-[a-z0-9]+)\/([^/]+)$/.exec(req.url)
    if (manifest) {
      const name = `@magine/cli-${manifest[1]}`
      res.setHeader('content-type', 'application/json')
      // Compact JSON with lookalike fields, as npm sends it.
      res.end(
        JSON.stringify({
          name,
          description: 'The magine binary, for one platform',
          version: VERSION,
          _npmVersion: '10.8.2',
          dist: { shasum: 'f'.repeat(40), tarball: `${base}/${name}/-/cli-${manifest[1]}-${VERSION}.tgz`, fileCount: 2, integrity: integrity ?? sri(tgz) },
        })
      )
    } else if (/^\/@magine\/cli-[a-z0-9]+-[a-z0-9]+\/-\/[^/]+\.tgz$/.test(req.url)) {
      res.setHeader('content-type', 'application/octet-stream')
      res.end(tgz)
    } else {
      res.statusCode = 404
      res.end('{"error":"Not found"}')
    }
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  base = `http://127.0.0.1:${server.address().port}`
  return { base, seen, close: () => new Promise((resolve) => server.close(resolve)) }
}

/** Async on purpose: the fake registry lives in this process. */
function run(command, args, { env, input } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { env, stdio: ['pipe', 'pipe', 'pipe'] })
    let stdout = ''
    let stderr = ''
    child.stdout.setEncoding('utf8').on('data', (d) => (stdout += d))
    child.stderr.setEncoding('utf8').on('data', (d) => (stderr += d))
    const timer = setTimeout(() => {
      child.kill()
      reject(new Error(`${command} timed out\nstdout: ${stdout}\nstderr: ${stderr}`))
    }, 90_000)
    child.on('error', reject)
    child.on('close', (code) => {
      clearTimeout(timer)
      resolve({ code, stdout, stderr, out: stdout + stderr })
    })
    child.stdin.end(input)
  })
}

function cleanEnv(extra) {
  const env = { ...process.env, NO_COLOR: '1', ...extra }
  for (const key of Object.keys(env)) {
    if (/^(MAGINE_(VERSION|INSTALL_DIR|NO_MODIFY_PATH|NPM_REGISTRY)|ZDOTDIR)$/i.test(key) && !(key in extra)) delete env[key]
  }
  return env
}

// ---------------------------------------------------------------- install.sh

/** Git's sh on Windows (its launcher puts curl, tar and coreutils on PATH), /bin/sh elsewhere. */
function findSh() {
  if (!WINDOWS) return '/bin/sh'
  const r = spawnSync('git', ['--exec-path'], { encoding: 'utf8' })
  const sh = r.status === 0 ? join(r.stdout.trim(), '..', '..', '..', 'bin', 'sh.exe') : ''
  return sh && existsSync(sh) ? sh : null
}
const SH = findSh()
const posix = (p) => (WINDOWS ? p.replace(/^([A-Za-z]):/, (_, d) => `/${d.toLowerCase()}`).replace(/\\/g, '/') : p)
const SH_PLATFORM = WINDOWS ? 'linux-x64' : `${process.platform}-${process.arch}`

/** Pipes install.sh into sh, the way `curl -fsSL https://magine.cloud/install.sh | sh` does. */
function installSh(tmp, env) {
  if (!WINDOWS) return run(SH, [], { env: cleanEnv(env), input: INSTALL_SH })
  const fakeBin = join(tmp, 'fake-bin')
  mkdirSync(fakeBin, { recursive: true })
  writeFileSync(join(fakeBin, 'uname'), '#!/bin/sh\ncase "$1" in -m) echo x86_64 ;; *) echo Linux ;; esac\n')
  const script = 'PATH="$MAGINE_TEST_FAKE_BIN:$PATH"; export PATH; exec sh'
  return run(SH, ['-c', script], { env: cleanEnv({ ...env, MAGINE_TEST_FAKE_BIN: posix(fakeBin) }), input: INSTALL_SH })
}

function shPackage() {
  // On Windows a shell script plays the Linux binary; elsewhere it is the real binary.
  const bin = WINDOWS ? Buffer.from(`#!/bin/sh\necho ${VERSION}\n`) : readFileSync(CLI)
  return platformPackage(SH_PLATFORM, 'magine', bin)
}

const shOpts = { skip: SH ? false : 'no sh found (install Git for Windows)' }

test('install.sh installs ~/.magine/bin/magine, checks it and adds it to PATH once', shOpts, async () => {
  const tmp = tempDir()
  const home = join(tmp.dir, 'home')
  mkdirSync(home)
  const registry = await fakeRegistry(shPackage())
  try {
    const env = { HOME: posix(home), SHELL: '/bin/bash', XDG_CONFIG_HOME: posix(join(home, '.config')), MAGINE_NPM_REGISTRY: registry.base }
    const first = await installSh(tmp.dir, env)
    assert.equal(first.code, 0, first.out)
    assert.deepEqual(registry.seen[0], `/@magine%2fcli-${SH_PLATFORM}/latest`)
    assert.match(first.stdout, new RegExp(`Magine CLI ${VERSION.replace(/\./g, '\\.')} is installed`))
    assert.match(first.stdout, /Next: magine login/)
    assert.match(first.stdout, /Added .* to PATH in: .*\.bashrc/)
    const bin = join(home, '.magine', 'bin', 'magine')
    assert.ok(existsSync(bin), 'binary installed')
    if (!WINDOWS) assert.equal(spawnSync(bin, ['--version'], { encoding: 'utf8' }).stdout.trim(), VERSION)
    const line = `export PATH="${posix(join(home, '.magine', 'bin'))}:$PATH"`
    const count = () => readFileSync(join(home, '.bashrc'), 'utf8').split(line).length - 1
    assert.equal(count(), 1)

    const again = await installSh(tmp.dir, env)
    assert.equal(again.code, 0, again.out)
    assert.doesNotMatch(again.stdout, /Added/)
    assert.equal(count(), 1, 'the PATH line is not added twice')
    assert.deepEqual(readdirSync(join(home, '.magine', 'bin')), ['magine'])
  } finally {
    await registry.close()
    tmp.cleanup()
  }
})

test('install.sh leaves shell files alone with MAGINE_NO_MODIFY_PATH=1', shOpts, async () => {
  const tmp = tempDir()
  const home = join(tmp.dir, 'home')
  mkdirSync(home)
  const registry = await fakeRegistry(shPackage())
  try {
    const dir = join(tmp.dir, 'tools')
    const r = await installSh(tmp.dir, {
      HOME: posix(home),
      SHELL: '/bin/zsh',
      MAGINE_NPM_REGISTRY: registry.base,
      MAGINE_INSTALL_DIR: posix(dir),
      MAGINE_NO_MODIFY_PATH: '1',
      MAGINE_VERSION: VERSION,
    })
    assert.equal(r.code, 0, r.out)
    assert.equal(registry.seen[0], `/@magine%2fcli-${SH_PLATFORM}/${VERSION}`)
    assert.ok(existsSync(join(dir, 'magine')))
    assert.deepEqual(readdirSync(home), [], 'no shell file was created or changed')
    assert.match(r.stdout, /To use it in this terminal, run: export PATH=/)
  } finally {
    await registry.close()
    tmp.cleanup()
  }
})

test('install.sh refuses a download that does not match its checksum', shOpts, async () => {
  const tmp = tempDir()
  const registry = await fakeRegistry(shPackage(), { integrity: sri(Buffer.from('something else')) })
  try {
    const dir = join(tmp.dir, 'bin')
    const r = await installSh(tmp.dir, { HOME: posix(tmp.dir), MAGINE_NPM_REGISTRY: registry.base, MAGINE_INSTALL_DIR: posix(dir) })
    assert.notEqual(r.code, 0)
    assert.match(r.stderr, /does not match its checksum; nothing was installed/)
    assert.ok(!existsSync(join(dir, 'magine')))
  } finally {
    await registry.close()
    tmp.cleanup()
  }
})

test('install.sh only downloads over https', shOpts, async () => {
  const tmp = tempDir()
  try {
    const r = await installSh(tmp.dir, { HOME: posix(tmp.dir), MAGINE_NPM_REGISTRY: 'http://registry.example.com' })
    assert.notEqual(r.code, 0)
    assert.match(r.stderr, /MAGINE_NPM_REGISTRY must start with https:\/\//)
  } finally {
    tmp.cleanup()
  }
})

// ---------------------------------------------------------------- install.ps1

const psOpts = { skip: WINDOWS ? false : 'install.ps1 is for Windows' }

/** Pipes install.ps1 into Invoke-Expression, the way `irm https://magine.cloud/install.ps1 | iex` does. */
function installPs1(env) {
  const command = `Get-Content -Raw -LiteralPath '${INSTALL_PS1.replace(/'/g, "''")}' | Invoke-Expression`
  return run('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', command], {
    env: cleanEnv({ MAGINE_NO_MODIFY_PATH: '1', ...env }),
  })
}

const psPackage = () => platformPackage('win32-x64', 'magine.exe', readFileSync(CLI))

test('install.ps1 installs magine.exe, checks it and can update it in place', psOpts, async () => {
  const tmp = tempDir()
  const registry = await fakeRegistry(psPackage())
  try {
    const dir = join(tmp.dir, 'Programs', 'magine')
    const first = await installPs1({ MAGINE_NPM_REGISTRY: registry.base, MAGINE_INSTALL_DIR: dir })
    assert.equal(first.code, 0, first.out)
    assert.match(registry.seen[0], /^\/@magine%2fcli-win32-(x64|arm64)\/latest$/)
    assert.match(first.stdout, new RegExp(`Magine CLI ${VERSION.replace(/\./g, '\\.')} is installed`))
    assert.match(first.stdout, /Next: magine login/)
    assert.doesNotMatch(first.stdout, /Added .* to your user PATH/)
    const exe = join(dir, 'magine.exe')
    assert.equal(spawnSync(exe, ['--version'], { encoding: 'utf8' }).stdout.trim(), VERSION)

    const again = await installPs1({ MAGINE_NPM_REGISTRY: registry.base, MAGINE_INSTALL_DIR: dir })
    assert.equal(again.code, 0, again.out)
    assert.deepEqual(readdirSync(dir), ['magine.exe'], 'no .new or .old file is left behind')
  } finally {
    await registry.close()
    tmp.cleanup()
  }
})

test('install.ps1 refuses a download that does not match its checksum', psOpts, async () => {
  const tmp = tempDir()
  const registry = await fakeRegistry(psPackage(), { integrity: sri(Buffer.from('something else')) })
  try {
    const dir = join(tmp.dir, 'magine')
    const r = await installPs1({ MAGINE_NPM_REGISTRY: registry.base, MAGINE_INSTALL_DIR: dir })
    assert.notEqual(r.code, 0)
    assert.match(r.out, /does not match its checksum; nothing was installed/)
    assert.ok(!existsSync(join(dir, 'magine.exe')))
  } finally {
    await registry.close()
    tmp.cleanup()
  }
})

test('install.ps1 only downloads over https', psOpts, async () => {
  const r = await installPs1({ MAGINE_NPM_REGISTRY: 'http://registry.example.com' })
  assert.notEqual(r.code, 0)
  assert.match(r.out, /MAGINE_NPM_REGISTRY must start with https:\/\//)
})
