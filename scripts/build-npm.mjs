// Assembles the npm packages from built binaries, the way swc and Biome ship native code:
//   <out>/magine            the "magine" package: a small launcher (bin/magine.js) with one
//                           optional dependency per platform, so npm downloads only the match
//   <out>/cli-<platform>    "@magine/cli-<platform>": the native binary for that platform
//
// node scripts/build-npm.mjs [--bin <rust-target>=<path>]... [--out dist-npm]
// Without --bin it uses target/<rust-target>/release/magine(.exe) for every target that was built.
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { registryReadme } from './registry-readme.mjs'

const root = fileURLToPath(new URL('..', import.meta.url))

export const TARGETS = {
  'x86_64-pc-windows-msvc': { pkg: 'win32-x64', os: 'win32', cpu: 'x64', label: 'Windows x64' },
  'aarch64-pc-windows-msvc': { pkg: 'win32-arm64', os: 'win32', cpu: 'arm64', label: 'Windows on Arm' },
  'x86_64-apple-darwin': { pkg: 'darwin-x64', os: 'darwin', cpu: 'x64', label: 'macOS Intel' },
  'aarch64-apple-darwin': { pkg: 'darwin-arm64', os: 'darwin', cpu: 'arm64', label: 'macOS Apple silicon' },
  'x86_64-unknown-linux-musl': { pkg: 'linux-x64', os: 'linux', cpu: 'x64', label: 'Linux x64 (static, any distribution)' },
  'aarch64-unknown-linux-musl': { pkg: 'linux-arm64', os: 'linux', cpu: 'arm64', label: 'Linux Arm64 (static, any distribution)' },
  // Builds made without Visual Studio (zig links them) stand in for the MSVC build locally.
  'x86_64-pc-windows-gnu': { pkg: 'win32-x64', os: 'win32', cpu: 'x64', label: 'Windows x64' },
}

/** One npm package per platform, in the order the launcher lists them. */
export const PLATFORM_PACKAGES = ['win32-x64', 'win32-arm64', 'darwin-x64', 'darwin-arm64', 'linux-x64', 'linux-arm64']

export function cargoVersion() {
  return /^version\s*=\s*"([^"]+)"/m.exec(readFileSync(join(root, 'Cargo.toml'), 'utf8'))[1]
}

const json = (v) => JSON.stringify(v, null, 2) + '\n'

/** Writes the packages; `bins` maps Rust targets to binary paths. Returns the folders written. */
export function buildNpm({ bins, out }) {
  const version = cargoVersion()
  rmSync(out, { recursive: true, force: true })
  const written = { main: join(out, 'magine'), platforms: {} }
  const license = readFileSync(join(root, 'LICENSE'))
  const template = JSON.parse(readFileSync(join(root, 'npm', 'magine', 'package.json'), 'utf8'))

  for (const [target, binPath] of Object.entries(bins)) {
    const t = TARGETS[target]
    if (!t) throw new Error(`unknown Rust target ${target}`)
    if (!existsSync(binPath)) throw new Error(`no binary at ${binPath} for ${target}`)
    const exe = t.os === 'win32' ? 'magine.exe' : 'magine'
    const dir = join(out, `cli-${t.pkg}`)
    mkdirSync(dir, { recursive: true })
    copyFileSync(binPath, join(dir, exe))
    chmodSync(join(dir, exe), 0o755)
    writeFileSync(join(dir, 'LICENSE'), license)
    writeFileSync(
      join(dir, 'README.md'),
      `# @magine/cli-${t.pkg}\n\nThe [Magine CLI](https://magine.cloud/docs#cli) binary for ${t.label}.\n\nYou do not install this package yourself: \`npm install -g magine\` picks the right one for your computer.\n`
    )
    writeFileSync(
      join(dir, 'package.json'),
      json({
        name: `@magine/cli-${t.pkg}`,
        version,
        description: `The Magine CLI binary for ${t.label}. Install "magine" instead.`,
        homepage: 'https://magine.cloud/docs#cli',
        repository: template.repository,
        license: 'MIT',
        author: 'Zeupiter (https://zeupiter.com)',
        os: [t.os],
        cpu: [t.cpu],
        files: [exe, 'README.md', 'LICENSE'],
        preferUnplugged: true,
        publishConfig: { registry: 'https://registry.npmjs.org/', access: 'public' },
      })
    )
    written.platforms[t.pkg] = dir
  }

  const main = written.main
  mkdirSync(join(main, 'bin'), { recursive: true })
  template.version = version
  template.optionalDependencies = Object.fromEntries(PLATFORM_PACKAGES.map((p) => [`@magine/cli-${p}`, version]))
  writeFileSync(join(main, 'package.json'), json(template))
  copyFileSync(join(root, 'npm', 'magine', 'bin', 'magine.js'), join(main, 'bin', 'magine.js'))
  chmodSync(join(main, 'bin', 'magine.js'), 0o755)
  writeFileSync(join(main, 'README.md'), registryReadme(readFileSync(join(root, 'README.md'), 'utf8')))
  writeFileSync(join(main, 'LICENSE'), license)
  return written
}

function parseArgs(argv) {
  const bins = {}
  let out = join(root, 'dist-npm')
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--bin') {
      const [target, path] = argv[++i].split(/=(.*)/s)
      bins[target] = resolve(path)
    } else if (argv[i] === '--out') out = resolve(argv[++i])
    else throw new Error(`unknown argument ${argv[i]}`)
  }
  if (!Object.keys(bins).length) {
    const targetDir = process.env.CARGO_TARGET_DIR || join(root, 'target')
    for (const [target, t] of Object.entries(TARGETS)) {
      const p = join(targetDir, target, 'release', t.os === 'win32' ? 'magine.exe' : 'magine')
      if (existsSync(p) && !Object.values(bins).some((b) => b === p)) bins[target] = p
    }
  }
  if (!Object.keys(bins).length) throw new Error('no binaries found; build with cargo first or pass --bin <target>=<path>')
  return { bins, out }
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const { bins, out } = parseArgs(process.argv.slice(2))
  const w = buildNpm({ bins, out })
  console.log(`build-npm: magine@${cargoVersion()} in ${w.main}`)
  for (const [pkg, dir] of Object.entries(w.platforms)) console.log(`build-npm: @magine/cli-${pkg} in ${dir}`)
}
