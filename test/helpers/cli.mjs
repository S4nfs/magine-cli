import { spawn } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

/** The magine binary under test: MAGINE_BIN, or the debug build from `cargo build`. */
export const CLI = process.env.MAGINE_BIN || fileURLToPath(new URL(`../../target/debug/magine${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url))

/** The version in Cargo.toml, which the binary reports. */
export const VERSION = /^version\s*=\s*"([^"]+)"/m.exec(readFileSync(new URL('../../Cargo.toml', import.meta.url), 'utf8'))[1]

export function tempDir(prefix = 'magine-cli-test-') {
  const dir = mkdtempSync(join(tmpdir(), prefix))
  return { dir, cleanup: () => rmSync(dir, { recursive: true, force: true }) }
}

/** Runs the CLI as a child process; stdin is closed unless `input` is given. */
export function runCli(args, { env = {}, input, cwd, timeoutMs = 30_000 } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(CLI, args, {
      cwd,
      env: {
        PATH: process.env.PATH,
        SystemRoot: process.env.SystemRoot,
        TEMP: process.env.TEMP,
        TMP: process.env.TMP,
        TMPDIR: process.env.TMPDIR,
        HOME: process.env.HOME,
        USERPROFILE: process.env.USERPROFILE,
        NO_COLOR: '1',
        MAGINE_NO_BROWSER: '1',
        ...env,
      },
      stdio: ['pipe', 'pipe', 'pipe'],
    })
    let stdout = ''
    let stderr = ''
    child.stdout.setEncoding('utf8')
    child.stderr.setEncoding('utf8')
    child.stdout.on('data', (d) => (stdout += d))
    child.stderr.on('data', (d) => (stderr += d))
    const timer = setTimeout(() => {
      child.kill()
      reject(new Error(`magine ${args.join(' ')} timed out\nstdout: ${stdout}\nstderr: ${stderr}`))
    }, timeoutMs)
    child.on('error', reject)
    child.on('close', (code) => {
      clearTimeout(timer)
      let json = null
      try {
        json = stdout.trim() ? JSON.parse(stdout) : null
      } catch {
        /* not JSON output */
      }
      let errorJson = null
      try {
        errorJson = stderr.trim().startsWith('{') ? JSON.parse(stderr.trim().split('\n').pop()) : null
      } catch {
        /* not JSON */
      }
      resolve({ code, stdout, stderr, json, errorJson })
    })
    if (input !== undefined) child.stdin.end(input)
    else child.stdin.end()
  })
}
