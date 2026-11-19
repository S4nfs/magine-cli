// The README's images are files the Magine web app serves, so they are linked by their magine.cloud
// address. A ../public/... link works only inside the app's repository; this turns any left into one.
//   node scripts/registry-readme.mjs          rewrites README.md in place
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

export const registryReadme = (text) => text.replaceAll('../public/', 'https://magine.cloud/')

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const file = join(dirname(fileURLToPath(import.meta.url)), '..', 'README.md')
  writeFileSync(file, registryReadme(readFileSync(file, 'utf8')))
}
