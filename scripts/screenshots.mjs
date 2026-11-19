// Real screenshots of the CLI for the README: runs the built binary against the test mock server
// with colours on, turns its ANSI output into HTML and photographs it with Playwright.
//
//   node scripts/screenshots.mjs --out <dir> [--text]
//
// --out   where the PNGs go (inside the Magine web app's repository the default is its public/cli,
//         which magine.cloud serves at /cli/ and the README links to)
// --text  print the scenes as plain text instead (to check them without a browser)
// MAGINE_BIN picks the binary (default: target/release/magine). Needs Playwright (npm i playwright).
import { spawn } from 'node:child_process'
import { closeSync, existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createState, startMockServer } from '../test/helpers/mock-server.mjs'

const root = fileURLToPath(new URL('..', import.meta.url))
const win = process.platform === 'win32'
const BIN = process.env.MAGINE_BIN || join(process.env.CARGO_TARGET_DIR || join(root, 'target'), 'release', win ? 'magine.exe' : 'magine')
const COLUMNS = 92
const HOUR = 3_600_000

const args = process.argv.slice(2)
const textOnly = args.includes('--text')
const outIndex = args.indexOf('--out')
const appCli = join(root, '..', 'public', 'cli')
if (outIndex < 0 && !textOnly && !existsSync(appCli)) {
  console.error('screenshots: pass --out <dir> for the PNGs')
  process.exit(2)
}
const OUT = resolve(outIndex >= 0 ? args[outIndex + 1] : appCli)

const ago = (ms) => new Date(Date.now() - ms).toISOString()

/** A believable account: three agents with history, files and a wallet. */
function seed(state) {
  state.wallet = { tokens: 3_456_789, freeTokens: 456_789, paidTokens: 3_000_000, usedTokens: 1_543_211, cats: 4, agentSlots: 0 }
  state.runMs = 300
  state.runDurationMs = 38_000
  const news = state.addAgent({
    name: 'news-bot',
    prompt: 'Every morning, list the top 10 Hacker News stories with their links',
    active: true,
    schedule: 'every day at 8am',
    cron: '0 8 * * *',
    createdAt: ago(240 * HOUR),
    lastRunAt: ago(3 * HOUR),
  })
  state.addAgent({
    name: 'price-watch',
    prompt: 'Watch the Sony WH-1000XM6 on amazon.in and tell me when it drops below Rs 25,000',
    mode: 'heartbeat',
    active: true,
    createdAt: ago(120 * HOUR),
    lastRunAt: ago(0.4 * HOUR),
  })
  state.addAgent({ name: 'jobs-digest', prompt: 'Find new remote Rust jobs on Wellfound and LinkedIn', active: false, createdAt: ago(30 * HOUR) })
  state.runs.get(news.id).push({
    id: 'r1',
    agentId: news.id,
    agentName: 'news-bot',
    status: 'success',
    prompt: news.prompt,
    summary: 'Top 10 Hacker News stories, 08:00',
    tokensUsed: 18_204,
    durationMs: 41_000,
    mode: 'agent',
    finishedAt: ago(3 * HOUR),
    steps: [],
    images: [],
  })
  state.summaryFor = (prompt) =>
    prompt.startsWith('Every morning')
      ? '1. Show HN: A terminal UI for Kubernetes (412 points)\n2. Rust 1.92 is out (388 points)\n3. The case for boring technology, ten years on (301 points)'
      : `Done: ${prompt}`
  state.addFile({
    id: '6743a1f09c2b4e0012ab34cd',
    name: 'hn-top10-2026-11-18.md',
    kind: 'text',
    source: 'agent',
    agentName: 'news-bot',
    content: Buffer.alloc(2_310, 'x'),
    createdAt: ago(3 * HOUR),
  })
}

const SCENES = [
  // The top of the help screen, as a terminal 17 rows tall shows it: the README's banner.
  { name: 'banner', title: 'magine', steps: [['--help']], signedIn: false, maxLines: 15 },
  { name: 'help', title: 'magine --help', steps: [['--help']], signedIn: false },
  {
    name: 'login',
    title: 'Sign in',
    signedIn: false,
    prepare: (state) => (state.autoApproveAfterPolls = 0),
    steps: [
      ['login', '--browser'],
      ['login', '--wait'],
    ],
  },
  {
    name: 'agents',
    title: 'Agents',
    steps: [
      ['agents', 'list'],
      ['agents', 'run', 'news-bot'],
    ],
  },
  { name: 'wallet', title: 'Wallet and files', steps: [['wallet'], ['cattery', 'list']] },
  { name: 'errors', title: 'Mistakes get a hint', signedIn: false, steps: [['agnets', 'list'], ['agents', 'run'], ['wallet']] },
]

function run(cmdArgs, env, input) {
  const work = mkdtempSync(join(tmpdir(), 'magine-shot-'))
  const file = join(work, 'out.txt')
  const fd = openSync(file, 'w')
  return new Promise((resolveRun, reject) => {
    // One file for stdout and stderr keeps them in the order the CLI wrote them.
    const child = spawn(BIN, cmdArgs, { env, stdio: [input === undefined ? 'ignore' : 'pipe', fd, fd] })
    if (input !== undefined) child.stdin.end(input)
    child.on('error', reject)
    child.on('close', (code) => {
      closeSync(fd)
      const text = readFileSync(file, 'utf8')
      rmSync(work, { recursive: true, force: true })
      resolveRun({ code, text })
    })
  })
}

async function capture(scene) {
  const state = createState()
  seed(state)
  scene.prepare?.(state)
  const server = await startMockServer({ state })
  const config = mkdtempSync(join(tmpdir(), 'magine-shot-config-'))
  const env = {
    PATH: process.env.PATH,
    SystemRoot: process.env.SystemRoot,
    TEMP: process.env.TEMP,
    TMP: process.env.TMP,
    HOME: config,
    USERPROFILE: config,
    MAGINE_API_URL: server.url,
    MAGINE_CONFIG_DIR: config,
    MAGINE_NO_BROWSER: '1',
    FORCE_COLOR: '3',
    COLORTERM: 'truecolor',
    COLUMNS: String(COLUMNS),
  }
  try {
    if (scene.signedIn !== false) {
      const r = await run(['login', '--key'], env, `${state.addKey('cli', { name: 'Magine CLI on sagar-laptop (Windows)' })}\n`)
      if (r.code !== 0) throw new Error(`sign-in failed: ${r.text}`)
    }
    const blocks = []
    for (const step of scene.steps) {
      const r = await run(step, env)
      let output = r.text.replace(/\r\n/g, '\n').replace(/\n+$/, '')
      if (scene.maxLines) output = output.split('\n').slice(0, scene.maxLines).join('\n')
      blocks.push({ command: `magine ${step.join(' ')}`, output })
    }
    return blocks
  } finally {
    await server.close()
    rmSync(config, { recursive: true, force: true })
  }
}

// ------------------------------------------------------------------- ANSI to HTML

const PALETTE = [
  '#484f58',
  '#ff7b72',
  '#3fb950',
  '#d29922',
  '#58a6ff',
  '#bc8cff',
  '#39c5cf',
  '#b1bac4',
  '#6e7681',
  '#ffa198',
  '#56d364',
  '#e3b341',
  '#79c0ff',
  '#d2a8ff',
  '#56d4dd',
  '#ffffff',
]
const escapeHtml = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')

function ansiToHtml(text) {
  let fg = null
  let bold = false
  let dim = false
  let html = ''
  const span = (s) => {
    if (!s) return
    const style = [fg && `color:${fg}`, bold && 'font-weight:700', dim && 'opacity:.7'].filter(Boolean).join(';')
    html += style ? `<span style="${style}">${escapeHtml(s)}</span>` : escapeHtml(s)
  }
  const parts = text.split(/(\x1b\[[0-9;]*[A-Za-z])/)
  for (const part of parts) {
    const m = /^\x1b\[([0-9;]*)([A-Za-z])$/.exec(part)
    if (!m) {
      span(part)
      continue
    }
    if (m[2] !== 'm') continue
    const codes = m[1] === '' ? [0] : m[1].split(';').map(Number)
    for (let i = 0; i < codes.length; i++) {
      const c = codes[i]
      if (c === 0) (fg = null), (bold = false), (dim = false)
      else if (c === 1) bold = true
      else if (c === 2) dim = true
      else if (c === 22) (bold = false), (dim = false)
      else if (c === 39) fg = null
      else if (c >= 30 && c <= 37) fg = PALETTE[c - 30]
      else if (c >= 90 && c <= 97) fg = PALETTE[c - 90 + 8]
      else if (c === 38 && codes[i + 1] === 2) (fg = `rgb(${codes[i + 2]},${codes[i + 3]},${codes[i + 4]})`), (i += 4)
      else if (c === 38 && codes[i + 1] === 5) (fg = PALETTE[codes[i + 2]] || null), (i += 2)
    }
  }
  return html
}

function page(scene, blocks) {
  const body = blocks
    .map((b) => `<div class="cmd"><span class="ps">~</span> <span class="dollar">$</span> ${escapeHtml(b.command)}</div>${b.output ? `<pre>${ansiToHtml(b.output)}</pre>` : ''}`)
    .join('\n')
  return `<!doctype html><html><head><meta charset="utf-8"><style>
  html, body { margin: 0; background: transparent; }
  body { padding: 28px; display: inline-block; }
  .window { min-width: ${Math.round(COLUMNS * 8.45)}px; width: max-content; border-radius: 12px; overflow: hidden; background: #0d1117; border: 1px solid #30363d;
    box-shadow: 0 22px 48px rgba(1, 4, 9, .45), 0 2px 6px rgba(1, 4, 9, .3); }
  .bar { height: 38px; display: flex; align-items: center; gap: 8px; padding: 0 14px; background: #161b22; border-bottom: 1px solid #30363d; }
  .dot { width: 12px; height: 12px; border-radius: 50%; }
  .title { flex: 1; text-align: center; margin-right: 52px; color: #8b949e; font: 600 13px -apple-system, "Segoe UI", system-ui, sans-serif; }
  .screen { padding: 14px 18px 18px; color: #c9d1d9; font: 14px/1.42 "Cascadia Mono", "Cascadia Code", Consolas, "SF Mono", Menlo, monospace; }
  .cmd { color: #e6edf3; white-space: pre; margin-top: 10px; }
  .cmd:first-child { margin-top: 0; }
  .ps { color: #a371f7; } .dollar { color: #3fb950; }
  pre { margin: 2px 0 0; font: inherit; white-space: pre; }
</style></head><body><div class="window"><div class="bar">
  <span class="dot" style="background:#ff5f57"></span><span class="dot" style="background:#febc2e"></span><span class="dot" style="background:#28c840"></span>
  <span class="title">${escapeHtml(scene.title)}</span></div><div class="screen">${body}</div></div></body></html>`
}

// ------------------------------------------------------------------- main

const results = []
for (const scene of SCENES) results.push({ scene, blocks: await capture(scene) })

if (textOnly) {
  for (const { scene, blocks } of results) {
    console.log(`\n=== ${scene.name}.png: ${scene.title}`)
    for (const b of blocks) console.log(`$ ${b.command}\n${b.output.replace(/\x1b\[[0-9;]*[A-Za-z]/g, '')}`)
  }
} else {
  const { chromium } = await import('playwright')
  const browser = await chromium.launch()
  try {
    const context = await browser.newContext({ deviceScaleFactor: 2, viewport: { width: 1200, height: 800 } })
    const tab = await context.newPage()
    mkdirSync(OUT, { recursive: true })
    for (const { scene, blocks } of results) {
      await tab.setContent(page(scene, blocks))
      await tab.evaluate(() => document.fonts.ready)
      const file = join(OUT, `${scene.name}.png`)
      await tab.locator('body').screenshot({ path: file, omitBackground: true })
      console.log(`screenshots: ${file}`)
    }
  } finally {
    await browser.close()
  }
}
