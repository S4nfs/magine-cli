import test from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, readFileSync, realpathSync, statSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { startMockServer } from './helpers/mock-server.mjs'
import { runCli, tempDir, VERSION } from './helpers/cli.mjs'

/** A fresh mock server, config folder and helpers for one test. */
async function setup(t, { signedIn = true } = {}) {
  const server = await startMockServer()
  const tmp = tempDir()
  t.after(async () => {
    await server.close()
    tmp.cleanup()
  })
  const env = { MAGINE_API_URL: server.url, MAGINE_CONFIG_DIR: tmp.dir }
  let key = null
  if (signedIn) {
    key = server.state.addKey('web')
    const r = await runCli(['login', '--key', '--json'], { env, input: `${key}\n` })
    assert.equal(r.code, 0, r.stderr)
  }
  const cli = (args, opts = {}) => runCli(args, { env: { ...env, ...(opts.env || {}) }, input: opts.input, cwd: opts.cwd ?? tmp.dir })
  return { server, state: server.state, env, dir: tmp.dir, key, cli }
}

test('--version and help work without signing in', async (t) => {
  const { cli } = await setup(t, { signedIn: false })
  const v = await cli(['--version'])
  assert.equal(v.code, 0)
  assert.equal(v.stdout.trim(), VERSION)
  const h = await cli(['--help'])
  assert.equal(h.code, 0)
  assert.match(h.stdout, /magine login/)
  const sub = await cli(['agents', 'run', '--help'])
  assert.match(sub.stdout, /Usage: magine agents run <agent>/)
  const group = await cli(['agents', '--help'])
  assert.match(group.stdout, /magine agents <command>/)
  const g = await cli(['guide', '--json'])
  assert.equal(g.code, 0)
  assert.ok(g.json.commands.length > 20)
})

test('unknown commands and options exit 2 with a suggestion', async (t) => {
  const { cli } = await setup(t, { signedIn: false })
  const r = await cli(['agnets', 'list'])
  assert.equal(r.code, 2)
  assert.match(r.stderr, /Unknown command/)
  assert.match(r.stderr, /Did you mean "magine agents list"/)
  const sub = await cli(['agents', 'strat', 'x'])
  assert.equal(sub.code, 2)
  const opt = await cli(['agents', 'list', '--colour'])
  assert.equal(opt.code, 2)
  assert.match(opt.stderr, /Unknown option --colour/)
})

test('commands need a sign-in: exit 4 with a hint', async (t) => {
  const { cli } = await setup(t, { signedIn: false })
  const r = await cli(['agents', 'list', '--json'])
  assert.equal(r.code, 4)
  assert.equal(r.errorJson.error.code, 'unauthorized')
  assert.match(r.errorJson.error.hint, /magine login/)
})

test('login without a method asks the agent to ask its user (choose_method)', async (t) => {
  const { cli } = await setup(t, { signedIn: false })
  const r = await cli(['login', '--json'])
  assert.equal(r.code, 0)
  assert.equal(r.json.step, 'choose_method')
  assert.deepEqual(
    r.json.methods.map((m) => m.id),
    ['browser', 'api_key']
  )
  assert.match(r.json.message, /Ask the user/)
})

test('login --key: from stdin, checked, saved for this user only, never echoed', async (t) => {
  const { cli, state, dir } = await setup(t, { signedIn: false })
  const key = state.addKey('web')
  const r = await cli(['login', '--key', '--json'], { input: `${key}\n` })
  assert.equal(r.code, 0, r.stderr)
  assert.equal(r.json.step, 'done')
  assert.equal(r.json.user.username, 'sagar')
  assert.ok(!r.stdout.includes(key) && !r.stderr.includes(key))
  const file = join(dir, 'credentials.json')
  const saved = JSON.parse(readFileSync(file, 'utf8'))
  assert.equal(saved.apiKey, key)
  assert.equal(saved.method, 'key')
  if (process.platform !== 'win32') assert.equal(statSync(file).mode & 0o777, 0o600)
  const who = await cli(['whoami', '--json'])
  assert.equal(who.code, 0)
  assert.equal(who.json.user.username, 'sagar')
})

test('a key on the command line is refused and not printed', async (t) => {
  const { cli, state } = await setup(t, { signedIn: false })
  const key = state.addKey('web')
  for (const args of [
    ['login', key],
    ['login', '--key', key],
  ]) {
    const r = await cli(args)
    assert.equal(r.code, 2)
    assert.ok(!r.stderr.includes(key), r.stderr)
    assert.match(r.stderr, /Never put an API key on the command line|Unexpected argument/)
  }
  const inline = await cli(['login', `--key=${key}`])
  assert.equal(inline.code, 2)
  assert.ok(!inline.stderr.includes(key))
})

test('login --key rejects a wrong or revoked key', async (t) => {
  const { cli, state } = await setup(t, { signedIn: false })
  const bad = await cli(['login', '--key'], { input: 'not-a-key\n' })
  assert.equal(bad.code, 4)
  const key = state.addKey('web')
  state.keys.get(key).revoked = true
  const revoked = await cli(['login', '--key', '--json'], { input: `${key}\n` })
  assert.equal(revoked.code, 4)
  assert.equal(revoked.errorJson.error.code, 'unauthorized')
})

test('browser sign-in for AI agents: approve step, then --wait until done', async (t) => {
  const { cli, state, dir } = await setup(t, { signedIn: false })
  const start = await cli(['login', '--browser', '--json'])
  assert.equal(start.code, 0, start.stderr)
  assert.equal(start.json.step, 'approve')
  assert.equal(start.json.userCode, 'WDJB-MJHT')
  assert.match(start.json.verificationUriComplete, /\/device\?code=WDJB-MJHT$/)
  assert.equal(start.json.next, 'magine login --wait --json')
  assert.ok(existsSync(join(dir, 'pending-login.json')))
  const req = state.requests.find((r) => r.path === '/api/v1/auth/device')
  assert.equal(req.headers.authorization, undefined)

  const waiting = await cli(['login', '--wait', '--json', '--timeout', '3s'])
  assert.equal(waiting.code, 9, waiting.stderr)
  assert.equal(waiting.json.step, 'waiting')

  state.devices.get(state.lastDevice).status = 'approved'
  const done = await cli(['login', '--wait', '--json'])
  assert.equal(done.code, 0, done.stderr)
  assert.equal(done.json.step, 'done')
  assert.ok(!existsSync(join(dir, 'pending-login.json')))
  const saved = JSON.parse(readFileSync(join(dir, 'credentials.json'), 'utf8'))
  assert.equal(saved.method, 'browser')
  assert.ok(saved.expiresAt)
  const who = await cli(['whoami', '--json'])
  assert.equal(who.json.key.source, 'cli')
})

test('browser sign-in honours slow_down and reports a denial (exit 4)', async (t) => {
  const { cli, state } = await setup(t, { signedIn: false })
  state.slowDownOnce = true
  await cli(['login', '--browser', '--json'])
  const w = await cli(['login', '--wait', '--json', '--timeout', '4s'])
  assert.equal(w.code, 9)
  state.devices.get(state.lastDevice).status = 'denied'
  const denied = await cli(['login', '--wait', '--json'])
  assert.equal(denied.code, 4)
  assert.equal(denied.errorJson.error.code, 'access_denied')
  const again = await cli(['login', '--wait'])
  assert.equal(again.code, 2)
})

test("a second browser sign-in revokes this computer's previous key", async (t) => {
  const { cli, state } = await setup(t, { signedIn: false })
  state.autoApproveAfterPolls = 0
  await cli(['login', '--browser', '--json'])
  assert.equal((await cli(['login', '--wait', '--json'])).code, 0)
  const first = [...state.keys.entries()].find(([, k]) => k.source === 'cli')
  await cli(['login', '--browser', '--json'])
  assert.equal((await cli(['login', '--wait', '--json'])).code, 0)
  assert.equal(state.keys.get(first[0]).revoked, true)
})

test('logout revokes a browser sign-in but keeps a key the user brought', async (t) => {
  const { cli, state, dir, key } = await setup(t)
  const out = await cli(['logout', '--json'])
  assert.equal(out.code, 0)
  assert.equal(out.json.keyRevoked, false)
  assert.equal(state.keys.get(key).revoked, false)
  assert.ok(!existsSync(join(dir, 'credentials.json')))
  assert.equal((await cli(['agents', 'list'])).code, 4)

  state.autoApproveAfterPolls = 0
  await cli(['login', '--browser', '--json'])
  await cli(['login', '--wait', '--json'])
  const cliKey = [...state.keys.entries()].find(([, k]) => k.source === 'cli')
  const out2 = await cli(['logout', '--json'])
  assert.equal(out2.json.keyRevoked, true)
  assert.equal(state.keys.get(cliKey[0]).revoked, true)
})

test('MAGINE_API_KEY works without a saved sign-in; a malformed one is refused', async (t) => {
  const { cli, state } = await setup(t, { signedIn: false })
  const key = state.addKey('web')
  const r = await cli(['whoami', '--json'], { env: { MAGINE_API_KEY: key } })
  assert.equal(r.code, 0)
  assert.equal(r.json.credentials, 'MAGINE_API_KEY')
  const bad = await cli(['whoami'], { env: { MAGINE_API_KEY: 'oops' } })
  assert.equal(bad.code, 4)
})

test('a saved key is never sent to a different server', async (t) => {
  const { cli } = await setup(t)
  const other = await startMockServer()
  t.after(() => other.close())
  const r = await cli(['agents', 'list', '--json', '--api-url', other.url])
  assert.equal(r.code, 4)
  assert.match(r.errorJson.error.message, /signed in to/)
  assert.equal(other.state.requests.length, 0)
})

test('expired sign-in: a clear message and exit 4', async (t) => {
  const { cli, dir } = await setup(t)
  const file = join(dir, 'credentials.json')
  const saved = JSON.parse(readFileSync(file, 'utf8'))
  writeFileSync(file, JSON.stringify({ ...saved, expiresAt: '2020-01-01T00:00:00.000Z' }))
  const r = await cli(['agents', 'list', '--json'])
  assert.equal(r.code, 4)
  assert.equal(r.errorJson.error.code, 'key_expired')
})

test('the key goes only in the Authorization header, never in a URL', async (t) => {
  const { cli, state, key } = await setup(t)
  state.addAgent({ name: 'news-bot' })
  await cli(['agents', 'list'])
  await cli(['wallet'])
  for (const r of state.requests) {
    assert.ok(!r.query.includes(key) && !r.path.includes(key), `${r.method} ${r.path}${r.query}`)
    if (!r.path.startsWith('/api/v1/auth/')) assert.equal(r.headers.authorization, `Bearer ${key}`)
    assert.match(r.headers['user-agent'], /^magine-cli\/\d+\.\d+\.\d+ \(/)
  }
})

test('agents: list, show, create, edit, enable, schedule, delete', async (t) => {
  const { cli, state, dir } = await setup(t)
  const empty = await cli(['agents'])
  assert.match(empty.stdout, /No agents yet/)

  const created = await cli(['agents', 'create', 'Every morning list HN top 10', '--name', 'news-bot', '--schedule', 'every day at 8am', '--json'], { env: { TZ: 'Asia/Kolkata' } })
  assert.equal(created.code, 0, created.stderr)
  assert.equal(created.json.agent.name, 'news-bot')
  assert.equal(created.json.catsCharged, 1)
  assert.equal(state.lastCreateBody.prompt, 'Every morning list HN top 10')
  assert.equal(state.lastCreateBody.schedule, 'every day at 8am')
  assert.ok(state.lastCreateBody.timezone)

  const human = await cli(['agents', 'list'])
  assert.match(human.stdout, /NAME\s+MODE\s+STATUS/)
  assert.match(human.stdout, /news-bot/)

  const show = await cli(['agents', 'show', 'news-bot', '--json'])
  assert.equal(show.json.agent.name, 'news-bot')

  const promptFile = join(dir, 'p.txt')
  writeFileSync(promptFile, 'A new prompt from a file')
  const edit = await cli(['agents', 'edit', 'news-bot', '--prompt-file', promptFile, '--json'])
  assert.equal(edit.code, 0, edit.stderr)
  assert.equal(edit.json.agent.prompt, 'A new prompt from a file')
  const nothing = await cli(['agents', 'edit', 'news-bot'])
  assert.equal(nothing.code, 2)

  const on = await cli(['agents', 'enable', 'news-bot', '--json'])
  assert.equal(on.json.agent.active, true)

  const sched = await cli(['agents', 'schedule', 'news-bot', 'every', 'day', 'at', '9am', '--timezone', 'Europe/Berlin', '--json'])
  assert.equal(sched.code, 0)
  assert.equal(state.lastScheduleBody.schedule, 'every day at 9am')
  assert.equal(state.lastScheduleBody.timezone, 'Europe/Berlin')
  const clear = await cli(['agents', 'schedule', 'news-bot', '--clear', '--json'])
  assert.equal(clear.json.schedule, null)

  const noYes = await cli(['agents', 'delete', 'news-bot'])
  assert.equal(noYes.code, 2)
  assert.match(noYes.stderr, /--yes/)
  const del = await cli(['agents', 'delete', 'news-bot', '--yes', '--json'])
  assert.equal(del.code, 0)
  assert.equal(state.agents.length, 0)
  const gone = await cli(['agents', 'show', 'news-bot', '--json'])
  assert.equal(gone.code, 3)
})

test('agents create: payment needed (exit 5), name taken (exit 6), heartbeat needs watch/action', async (t) => {
  const { cli, state } = await setup(t)
  state.addAgent({ name: 'taken' })
  const taken = await cli(['agents', 'create', 'do things', '--name', 'taken', '--json'])
  assert.equal(taken.code, 6)
  const hb = await cli(['agents', 'create', '--mode', 'heartbeat', '--watch', 'new mail'])
  assert.equal(hb.code, 2)
  state.wallet.cats = 0
  const broke = await cli(['agents', 'create', 'do things', '--json'])
  assert.equal(broke.code, 5)
  assert.equal(broke.errorJson.error.code, 'payment_required')
  assert.match(broke.errorJson.error.hint, /wallet buy/)
})

test('agents run waits for the result by polling; --no-wait returns at once', async (t) => {
  const { cli, state } = await setup(t)
  state.addAgent({ name: 'news-bot' })
  const r = await cli(['agents', 'run', 'news-bot', '--task', 'only today', '--json'])
  assert.equal(r.code, 0, r.stderr)
  assert.equal(r.json.run.status, 'success')
  assert.equal(r.json.run.summary, 'Finished: only today')
  assert.equal(state.lastRunBody.wait, false)

  const human = await cli(['agents', 'run', 'news-bot'])
  assert.equal(human.code, 0)
  assert.match(human.stdout, /Done: news-bot/)

  const nowait = await cli(['agents', 'run', 'news-bot', '--no-wait', '--json'])
  assert.equal(nowait.code, 0)
  assert.equal(nowait.json.started, true)
  const busy = await cli(['agents', 'run', 'news-bot', '--json'])
  assert.equal(busy.code, 6)
  await new Promise((r) => setTimeout(r, 900))

  state.nextOutcome = 'failed'
  const failed = await cli(['agents', 'run', 'news-bot', '--json'])
  assert.equal(failed.code, 1)
  assert.equal(failed.json.run.status, 'failed')

  state.runMs = 8000
  const capped = await cli(['agents', 'run', 'news-bot', '--timeout', '2s', '--json'])
  assert.equal(capped.code, 9)
  assert.equal(capped.errorJson.error.code, 'waiting')
})

test('do: a quick task, waited for; payment needed is exit 5', async (t) => {
  const { cli, state } = await setup(t)
  const r = await cli(['do', 'Find', 'three', 'cheap', 'flights', '--json'])
  assert.equal(r.code, 0, r.stderr)
  assert.equal(r.json.run.summary, 'Finished: Find three cheap flights')
  const runs = await cli(['agents', 'runs', 'quick', '--json'])
  assert.equal(runs.json.runs.length, 1)
  state.wallet.tokens = 10
  const broke = await cli(['do', 'anything at all', '--json'])
  assert.equal(broke.code, 5)
})

test('agents runs, logs and stop', async (t) => {
  const { cli, state } = await setup(t)
  state.addAgent({ name: 'news-bot' })
  await cli(['agents', 'run', 'news-bot'])
  const runs = await cli(['agents', 'runs', 'news-bot'])
  assert.match(runs.stdout, /FINISHED\s+STATUS\s+TOKENS\s+SUMMARY/)
  const logs = await cli(['agents', 'logs', 'news-bot', '--json'])
  assert.equal(logs.json.logs.length, 1)
  const stop = await cli(['agents', 'stop', 'news-bot', '--json'])
  assert.equal(stop.code, 0)
})

test('cattery: upload, list, show, download (byte for byte), refuse overwrite, delete', async (t) => {
  const { cli, state, dir } = await setup(t)
  const src = join(dir, 'pic.png')
  const bytes = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 1, 2, 3, 255, 254, 253])
  writeFileSync(src, bytes)
  const up = await cli(['cattery', 'upload', src, '--json'])
  assert.equal(up.code, 0, up.stderr)
  const id = up.json.item.id
  const upload = state.requests.find((r) => r.method === 'POST' && r.path === '/api/v1/cattery')
  assert.equal(upload.query, '?name=pic.png')
  assert.equal(upload.bytes, bytes.length)

  const list = await cli(['cattery', '--json'])
  assert.equal(list.json.items.length, 1)
  const show = await cli(['cattery', 'show', id, '--json'])
  assert.equal(show.json.item.name, 'pic.png')

  const out = join(dir, 'out')
  const dl = await cli(['cattery', 'download', id, '-o', join(dir, 'copy.png'), '--json'])
  assert.equal(dl.code, 0, dl.stderr)
  assert.deepEqual(readFileSync(join(dir, 'copy.png')), bytes)
  const again = await cli(['cattery', 'download', id, '-o', join(dir, 'copy.png')])
  assert.equal(again.code, 6)
  const forced = await cli(['cattery', 'download', id, '-o', join(dir, 'copy.png'), '--force'])
  assert.equal(forced.code, 0)
  const noDir = await cli(['cattery', 'download', id, '-o', join(out, 'missing', 'x.png')])
  assert.equal(noDir.code, 2)

  const stdout = await cli(['cattery', 'download', id, '-o', '-'])
  assert.equal(stdout.code, 0)
  assert.equal(stdout.stdout.length > 0, true)

  const missing = await cli(['cattery', 'upload', join(dir, 'nope.bin')])
  assert.equal(missing.code, 2)
  const del = await cli(['cattery', 'delete', id, '--yes', '--json'])
  assert.equal(del.code, 0)
  assert.equal(state.cattery.length, 0)
})

test('cattery download makes a hostile file name safe', async (t) => {
  const { cli, state, dir } = await setup(t)
  const item = state.addFile({ name: '../../evil.txt', content: Buffer.from('x') })
  const r = await cli(['cattery', 'download', item.id, '--json'], { cwd: dir })
  assert.equal(r.code, 0, r.stderr)
  assert.equal(realpathSync(r.json.path), realpathSync(join(dir, 'evil.txt')))
})

test('tags: list @agents (Paws ones too) and #files, filter by kind, and check tags in tasks', async (t) => {
  const { cli, state } = await setup(t)
  state.addAgent({ name: 'news-bot', active: true, schedule: 'every day at 8am' })
  state.addAgent({ name: 'price-watch' })
  state.addAgent({ name: 'paws-2', system: 'paws' })
  state.addFile({ name: 'my report (final).pdf' })
  state.addFile({ name: 'notes.md' })

  const all = await cli(['tags', '--json'])
  assert.equal(all.code, 0, all.stderr)
  assert.deepEqual(
    all.json.tags.map((x) => x.tag),
    ['@news-bot', '@price-watch', '@paws-2', '#my_report__final_.pdf', '#notes.md']
  )
  assert.equal(all.json.tags[2].detail.startsWith('Paws'), true)
  assert.equal(all.json.tags[3].name, 'my report (final).pdf')
  assert.equal(state.requests.some((r) => r.path === '/api/v1/agents' && r.query === '?all=1'), true)

  const agents = await cli(['tags', '@pr', '--json'])
  assert.deepEqual(agents.json.tags.map((x) => x.tag), ['@price-watch'])
  const files = await cli(['tags', '#rep', '--json'])
  assert.deepEqual(files.json.tags.map((x) => x.tag), ['#my_report__final_.pdf'])
  const human = await cli(['tags', 'news'])
  assert.match(human.stdout, /TAG\s+TYPE\s+DETAILS/)
  assert.match(human.stdout, /@news-bot\s+agent/)
  const none = await cli(['tags', '@zzz'])
  assert.match(none.stdout, /Nothing matches "@zzz"/)

  const ok = await cli(['do', 'Summarise #notes.md for @news-bot', '--json'])
  assert.equal(ok.code, 0, ok.stderr)
  assert.doesNotMatch(ok.stderr, /warning/)
  const typos = await cli(['do', 'Summarise #notse.md like @news-bot. and tweet @elonmusk', '--json'])
  assert.equal(typos.code, 0, 'unknown tags only warn: the server reads @handles as text')
  assert.match(typos.stderr, /warning: #notse\.md is not in your Cattery.*Did you mean #notes\.md\?/)
  assert.match(typos.stderr, /warning: @news-bot\. is not one of your agents.*Did you mean @news-bot\?/)
  assert.match(typos.stderr, /warning: @elonmusk is not one of your agents/)
  assert.equal(state.runs.get('agent-do').length >= 2, true)

  const noTask = await cli(['do', '--json'])
  assert.equal(noTask.code, 2, 'without a terminal, a task must be given')
})

test('cattery: save a result with --save, upload from stdin, use #name instead of an id', async (t) => {
  const { cli, state, dir } = await setup(t)
  state.addAgent({ name: 'news-bot' })
  state.summaryFor = () => '1. Rust 1.92 is out\n2. A terminal UI for Kubernetes'

  const saved = await cli(['do', 'List the top stories', '--save', 'top-stories', '--json'])
  assert.equal(saved.code, 0, saved.stderr)
  assert.equal(saved.json.saved.name, 'top-stories.md')
  const file = state.cattery.find((f) => f.name === 'top-stories.md')
  assert.equal(file.content.toString(), '1. Rust 1.92 is out\n2. A terminal UI for Kubernetes\n')
  const up = state.requests.find((r) => r.method === 'POST' && r.path === '/api/v1/cattery')
  assert.equal(up.query, '?name=top-stories.md&agent=quick')

  const human = await cli(['agents', 'run', 'news-bot', '--save', 'hn.txt'])
  assert.equal(human.code, 0, human.stderr)
  assert.match(human.stdout, /Saved to your Cattery as hn\.txt\. Use it in a task as #hn\.txt\./)
  const noWait = await cli(['agents', 'run', 'news-bot', '--save', 'x', '--no-wait'])
  assert.equal(noWait.code, 2)
  await new Promise((r) => setTimeout(r, 900))
  state.nextOutcome = 'failed'
  const failed = await cli(['agents', 'run', 'news-bot', '--save', 'never.md', '--json'])
  assert.equal(failed.code, 1)
  assert.match(failed.stderr, /nothing was saved/)
  assert.equal(state.cattery.some((f) => f.name === 'never.md'), false)

  const piped = await cli(['cattery', 'upload', '-', '--name', 'piped.json', '--json'], { input: '{"a":1}' })
  assert.equal(piped.code, 0, piped.stderr)
  assert.equal(state.cattery.find((f) => f.name === 'piped.json').content.toString(), '{"a":1}')
  const unnamed = await cli(['cattery', 'upload', '-'], { input: 'x' })
  assert.equal(unnamed.code, 2)

  const show = await cli(['cattery', 'show', '#piped.json', '--json'])
  assert.equal(show.code, 0, show.stderr)
  assert.equal(show.json.item.name, 'piped.json')
  const byBase = await cli(['cattery', 'download', '#top-stories', '-o', join(dir, 'top.md'), '--json'])
  assert.equal(byBase.code, 0, byBase.stderr)
  assert.equal(readFileSync(join(dir, 'top.md'), 'utf8').startsWith('1. Rust'), true)
  const missing = await cli(['cattery', 'show', '#nope.md'])
  assert.equal(missing.code, 3)
  assert.match(missing.stderr, /No Cattery file called #nope\.md/)
  const del = await cli(['cattery', 'delete', 'piped.json', '--yes', '--json'])
  assert.equal(del.code, 0, del.stderr)
  assert.equal(state.cattery.some((f) => f.name === 'piped.json'), false)
})

test('neurons, marketplace, github', async (t) => {
  const { cli, state } = await setup(t)
  const n = await cli(['neurons', 'news', '--json'])
  assert.equal(n.code, 0)
  assert.ok(n.json.nodes.every((x) => x.label.includes('news')))
  const badKind = await cli(['neurons', '--kind', 'planet'])
  assert.equal(badKind.code, 2)

  const list = await cli(['marketplace'])
  assert.match(list.stdout, /YouTube Comment Moderator/)
  const search = await cli(['marketplace', 'search', 'youtube', '--json'])
  assert.equal(search.json.templates.length, 1)
  const show = await cli(['marketplace', 'show', 'b'.repeat(24), '--json'])
  assert.equal(show.json.template.deployCostCats, 2)
  const deploy = await cli(['marketplace', 'deploy', 'b'.repeat(24), '--json'])
  assert.equal(deploy.code, 0)
  assert.deepEqual(deploy.json.agents, ['youtube-comment-moderator', 'youtube-reply-helper'])
  state.wallet.cats = 0
  const broke = await cli(['marketplace', 'deploy', 'b'.repeat(24)])
  assert.equal(broke.code, 5)
  state.marketplaceOpen = false
  const closed = await cli(['marketplace'])
  assert.match(closed.stdout, /closed/)

  const gh = await cli(['github', 'sagar'])
  assert.equal(gh.code, 0)
  assert.match(gh.stdout, /Claude Code 30%/)
  const nobody = await cli(['github', 'nobody', '--json'])
  assert.equal(nobody.code, 3)
})

test('wallet: balance, usage, buy for an AI agent, confirm until paid', async (t) => {
  const { cli } = await setup(t)
  const w = await cli(['wallet'])
  assert.match(w.stdout, /1 Cat = \$5/)
  const u = await cli(['wallet', 'usage', '--json'])
  assert.equal(u.json.usage.length, 1)
  const bad = await cli(['wallet', 'buy', '0'])
  assert.equal(bad.code, 2)
  const buy = await cli(['wallet', 'buy', '5', '--json'])
  assert.equal(buy.code, 0, buy.stderr)
  assert.equal(buy.json.step, 'pay')
  assert.match(buy.json.url, /^https:\/\/checkout\.example\.com\//)
  assert.match(buy.json.next, /magine wallet confirm pay_\w+ --provider dodo --json/)
  const first = await cli(['wallet', 'confirm', buy.json.orderId, '--provider', 'dodo', '--json'])
  assert.equal(first.code, 9)
  assert.equal(first.json.status, 'pending')
  const second = await cli(['wallet', 'confirm', buy.json.orderId, '--provider', 'dodo', '--json'])
  assert.equal(second.code, 0)
  assert.equal(second.json.status, 'completed')
  const noProvider = await cli(['wallet', 'confirm', buy.json.orderId])
  assert.equal(noProvider.code, 2)
})

test('keys: list, create (shown once), revoke with --yes, revoking the current key', async (t) => {
  const { cli, state } = await setup(t)
  const list = await cli(['keys', '--json'])
  assert.equal(list.json.keys.length, 1)
  assert.equal(list.json.keys[0].current, true)
  const created = await cli(['keys', 'create', '--name', 'ci', '--json'])
  assert.equal(created.code, 0)
  assert.match(created.json.key, /^mk_[0-9a-f]{64}$/)
  const human = await cli(['keys', 'create', '--name', 'ci2'])
  assert.match(human.stdout, /Shown only now/)
  const badDays = await cli(['keys', 'create', '--expires-in-days', '999'])
  assert.equal(badDays.code, 2)
  const noYes = await cli(['keys', 'revoke', created.json.id])
  assert.equal(noYes.code, 2)
  const rev = await cli(['keys', 'revoke', created.json.id, '--yes', '--json'])
  assert.equal(rev.code, 0)
  assert.equal(state.keys.get(created.json.key).revoked, true)
})

test('server errors and rate limits map to exit codes; JSON errors go to stderr', async (t) => {
  const { cli, state } = await setup(t)
  state.failNext = { status: 429, code: 'rate_limited', message: 'Too many requests with this key.', extra: { retryAfter: 1 } }
  const limited = await cli(['agents', 'create', 'x y z', '--json'])
  assert.equal(limited.code, 7)
  assert.equal(limited.stdout, '')
  assert.equal(limited.errorJson.error.code, 'rate_limited')
  state.failNext = { status: 502, code: 'server_error', message: 'Something went wrong on our side.' }
  const server = await cli(['agents', 'create', 'x y z', '--json'])
  assert.equal(server.code, 8)
  const retried = await (async () => {
    state.failNext = { status: 503, code: 'unavailable', message: 'busy' }
    return cli(['agents', 'list', '--json'])
  })()
  assert.equal(retried.code, 0, 'a GET is retried once the server is back')
})

test('network failures: unreachable server is exit 8, a non-Magine server is a config error', async (t) => {
  const { cli, env } = await setup(t, { signedIn: false })
  const down = await cli(['login', '--browser', '--json'], { env: { ...env, MAGINE_API_URL: 'http://127.0.0.1:9' } })
  assert.equal(down.code, 8)
  const insecure = await cli(['login', '--json', '--api-url', 'http://example.com'])
  assert.equal(insecure.code, 10)
})
