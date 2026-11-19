// Keeps one version in Cargo.toml, Cargo.lock and the npm launcher package: Cargo.toml's, or inside the
// Magine web app's repository the app's (VERSION in ../lib/constants.ts). `--check` only verifies (CI).
import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const file = (p) => fileURLToPath(new URL(p, import.meta.url))
const constants = file('../../lib/constants.ts')
const source = existsSync(constants) ? constants : file('../Cargo.toml')
const m = (source === constants ? /export const VERSION = ['"]v?(\d+\.\d+\.\d+(?:-[\w.]+)?)['"]/ : /^version\s*=\s*"([^"]+)"/m).exec(readFileSync(source, 'utf8'))
if (!m) {
  console.error(`sync-version: no version in ${source}`)
  process.exit(1)
}
const want = m[1]

const edits = [
  { path: file('../Cargo.toml'), re: /^(version\s*=\s*")([^"]+)(")/m },
  { path: file('../Cargo.lock'), re: /(\[\[package\]\]\r?\nname = "magine"\r?\nversion = ")([^"]+)(")/ },
  { path: file('../npm/magine/package.json'), re: /("version":\s*")([^"]+)(")/ },
]
let stale = []
for (const e of edits) {
  if (!existsSync(e.path)) continue
  const text = readFileSync(e.path, 'utf8')
  const found = e.re.exec(text)
  if (!found) {
    console.error(`sync-version: no version in ${e.path}`)
    process.exit(1)
  }
  if (found[2] === want) continue
  stale.push(`${e.path} has ${found[2]}`)
  if (!process.argv.includes('--check')) writeFileSync(e.path, text.replace(e.re, `$1${want}$3`))
}
if (stale.length && process.argv.includes('--check')) {
  console.error(`sync-version: the version is ${want}, but ${stale.join('; ')}. Run "node scripts/sync-version.mjs".`)
  process.exit(1)
}
console.log(stale.length ? `sync-version: set ${want} (${stale.length} file${stale.length === 1 ? '' : 's'})` : `sync-version: ${want}`)
