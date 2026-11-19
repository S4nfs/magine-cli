// A stand-in for the Magine /api/v1 contract (lib/v1/router.ts) for the CLI tests.
import http from 'node:http'
import https from 'node:https'
import crypto from 'node:crypto'

const json = (res, status, data, headers = {}) => {
  res.writeHead(status, { 'content-type': 'application/json; charset=utf-8', 'x-magine-version': 'v1.7.5', 'cache-control': 'no-store', ...headers })
  res.end(JSON.stringify(data))
}
const fail = (res, status, code, message, extra = {}) => json(res, status, { error: { code, message, ...extra } })
const hex = (n) => crypto.randomBytes(n).toString('hex')
const newKey = () => `mk_${hex(32)}`
const oid = () => hex(12)
const iso = (ms = Date.now()) => new Date(ms).toISOString()

async function readBody(req) {
  const chunks = []
  for await (const c of req) chunks.push(c)
  return Buffer.concat(chunks)
}

export function createState() {
  const state = {
    user: { id: 'u'.repeat(24), username: 'sagar', email: 'sagar@example.com', githubUsername: 'sagar', signedUpAt: iso(Date.parse('2026-01-01')) },
    keys: new Map(),
    agents: [],
    runs: new Map(),
    running: new Map(),
    logs: new Map(),
    cattery: [],
    devices: new Map(),
    orders: new Map(),
    wallet: { tokens: 1_200_000, freeTokens: 200_000, paidTokens: 1_000_000, usedTokens: 50_000, cats: 2, agentSlots: 0 },
    requests: [],
    runMs: 600,
    deviceInterval: 1,
    marketplaceOpen: true,
  }
  state.addKey = (source = 'web', extra = {}) => {
    const key = newKey()
    state.keys.set(key, {
      id: oid(),
      name: source === 'cli' ? 'Magine CLI on test (Linux)' : 'Key 1',
      source,
      createdAt: iso(),
      lastUsedAt: null,
      expiresAt: null,
      revoked: false,
      parentId: null,
      ...extra,
    })
    return key
  }
  state.addAgent = (a) => {
    const agent = {
      id: oid(),
      name: 'news-bot',
      mode: 'agent',
      workflow: 'cdp',
      prompt: 'List the top 10 Hacker News stories',
      active: false,
      schedule: null,
      cron: null,
      heartbeat: null,
      marketplace: null,
      system: null,
      createdAt: iso(),
      lastRunAt: null,
      ...a,
    }
    state.agents.push(agent)
    state.runs.set(agent.id, [])
    state.logs.set(agent.id, [{ id: oid(), agentId: agent.id, agentName: agent.name, action: 'created', status: 'success', details: 'Agent created', tokensUsed: 0, at: iso() }])
    return agent
  }
  state.addFile = (f) => {
    const content = f.content || Buffer.from('hello cattery\n')
    const item = {
      id: f.id || oid(),
      name: 'report.txt',
      kind: 'text',
      source: 'uploaded',
      mimeType: 'text/plain',
      agentId: null,
      agentName: null,
      prompt: null,
      createdAt: iso(),
      ...f,
      content,
      bytes: content.length,
    }
    state.cattery.push(item)
    return item
  }
  state.runs.set('agent-do', [])
  state.logs.set('agent-do', [])
  return state
}

function startRun(state, agentId, agentName, prompt, outcome = 'success') {
  state.running.set(agentId, true)
  const startedAt = iso()
  setTimeout(() => {
    state.running.delete(agentId)
    const ok = outcome === 'success'
    state.runs.get(agentId).unshift({
      id: oid(),
      agentId,
      agentName,
      status: ok ? 'success' : 'failed',
      prompt,
      summary: ok ? state.summaryFor?.(prompt) ?? `Finished: ${prompt}` : '',
      error: ok ? null : 'The page did not load',
      tokensUsed: 4321,
      durationMs: state.runDurationMs ?? state.runMs,
      mode: 'agent',
      finishedAt: iso(),
      steps: [{ step: 1, action: 'navigate', explanation: 'Opened the page', success: true, url: 'https://news.ycombinator.com', title: 'HN', extracted: null }],
      images: [],
    })
    state.wallet.tokens -= 4321
  }, state.runMs).unref()
  return startedAt
}

export async function startMockServer({ tls, state = createState() } = {}) {
  const handler = async (req, res) => {
    const url = new URL(req.url, 'http://x')
    const body = await readBody(req)
    state.requests.push({ method: req.method, path: url.pathname, query: url.search, headers: { ...req.headers }, bytes: body.length })
    if (!url.pathname.startsWith('/api/v1/')) return fail(res, 404, 'not_found', 'Not here')
    const parts = url.pathname.slice('/api/v1/'.length).split('/').map(decodeURIComponent)
    const route = `${req.method} ${parts
      .map((p, i) => (i > 0 && !['run', 'stop', 'runs', 'logs', 'schedule', 'content', 'deploy', 'confirm', 'token', 'usage', 'checkout', 'device'].includes(p) ? ':' : p))
      .join('/')}`
    let jsonBody = {}
    if (body.length && String(req.headers['content-type'] || '').includes('application/json')) {
      try {
        jsonBody = JSON.parse(body.toString('utf8'))
      } catch {
        return fail(res, 400, 'bad_request', 'The request body is not valid JSON.')
      }
    }

    // Sign-in endpoints need no key.
    if (route === 'POST auth/device') {
      const deviceCode = crypto.randomBytes(32).toString('base64url')
      const userCode = 'WDJB-MJHT'
      state.devices.set(deviceCode, { userCode, status: 'pending', polls: 0, interval: state.deviceInterval, meta: jsonBody, lastPoll: 0 })
      state.lastDevice = deviceCode
      return json(res, 201, {
        deviceCode,
        userCode,
        verificationUri: 'https://magine.cloud/device',
        verificationUriComplete: `https://magine.cloud/device?code=${userCode}`,
        expiresIn: state.deviceExpiresIn ?? 600,
        interval: state.deviceInterval,
      })
    }
    if (route === 'POST auth/device/token') {
      const d = state.devices.get(jsonBody.deviceCode)
      if (!d || d.status === 'consumed' || d.status === 'expired') return fail(res, 400, 'expired_token', 'This sign-in code has expired or was used.')
      d.polls++
      if (state.slowDownOnce && d.polls === 1) return fail(res, 400, 'slow_down', 'Polling too fast.', { interval: d.interval + 1 })
      if (state.autoApproveAfterPolls !== undefined && d.polls > state.autoApproveAfterPolls && d.status === 'pending') d.status = 'approved'
      if (d.status === 'denied') return fail(res, 400, 'access_denied', 'The sign-in was denied in the browser.')
      if (d.status !== 'approved') return fail(res, 400, 'authorization_pending', 'Waiting for approval.', { interval: d.interval })
      d.status = 'consumed'
      const apiKey = state.addKey('cli', { expiresAt: iso(Date.now() + 90 * 86_400_000) })
      const k = state.keys.get(apiKey)
      return json(res, 200, { apiKey, keyId: k.id, keyPrefix: `${apiKey.slice(0, 11)}...`, expiresAt: k.expiresAt, user: { id: state.user.id, username: state.user.username } })
    }

    for (const p of ['token', 'key', 'api_key', 'apikey', 'access_token', 'session']) {
      if (url.searchParams.has(p)) return fail(res, 400, 'bad_request', 'Send the API key in the Authorization header, never in the URL.')
    }
    const m = /^Bearer\s+(\S+)$/i.exec(req.headers.authorization || '')
    const key = m && state.keys.get(m[1])
    if (!key || key.revoked) return fail(res, 401, 'unauthorized', 'Invalid or revoked API key.')
    if (key.expiresAt && Date.parse(key.expiresAt) <= Date.now()) return fail(res, 401, 'key_expired', 'This API key has expired.')
    key.lastUsedAt = iso()
    if (state.failNext) {
      const f = state.failNext
      state.failNext = null
      return fail(res, f.status, f.code, f.message, f.extra || {})
    }

    const findAgent = (ref) => {
      if (ref === 'quick' || ref === 'agent-do') return { id: 'agent-do', name: 'Quick Task', quick: true }
      return state.agents.find((a) => a.id === ref || a.name === String(ref).toLowerCase())
    }
    const agentRoute = parts[0] === 'agents' && parts[1] ? findAgent(parts[1]) : null
    if (parts[0] === 'agents' && parts[1] && !agentRoute) return fail(res, 404, 'not_found', `No agent called "${parts[1]}".`)
    const sp = url.searchParams

    switch (route) {
      case 'GET me':
        return json(res, 200, {
          user: state.user,
          key: { id: key.id, name: key.name, source: key.source, expiresAt: key.expiresAt },
          wallet: { tokens: state.wallet.tokens, cats: state.wallet.cats, agentSlots: state.wallet.agentSlots },
          server: { version: 'v1.7.5' },
        })
      case 'GET keys':
        return json(res, 200, {
          keys: [...state.keys.values()]
            .filter((k) => !k.revoked)
            .map((k) => ({
              id: k.id,
              prefix: 'mk_abcdef12...',
              name: k.name,
              source: k.source,
              createdAt: k.createdAt,
              lastUsedAt: k.lastUsedAt,
              expiresAt: k.expiresAt,
              current: k.id === key.id,
            })),
        })
      case 'POST keys': {
        const created = state.addKey('api', { name: jsonBody.name || 'Key 2', parentId: key.id })
        const k = state.keys.get(created)
        return json(res, 201, { key: created, id: k.id, prefix: `${created.slice(0, 11)}...`, expiresAt: null })
      }
      case 'DELETE keys/:': {
        const id = parts[1] === 'current' ? key.id : parts[1]
        const entry = [...state.keys.values()].find((k) => k.id === id && !k.revoked)
        if (!entry) return fail(res, 404, 'not_found', 'API key not found or already revoked.')
        entry.revoked = true
        let revoked = 1
        for (const k of state.keys.values()) if (k.parentId === id && !k.revoked) (k.revoked = true), revoked++
        return json(res, 200, { revoked, id, current: id === key.id })
      }
      case 'GET agents':
        return json(res, 200, { agents: sp.get('all') ? state.agents : state.agents.filter((a) => !a.system) })
      case 'POST agents': {
        if (!jsonBody.prompt || String(jsonBody.prompt).length < 3) return fail(res, 400, 'bad_request', 'Describe what the agent should do.')
        const cost = jsonBody.mode === 'heartbeat' ? 2 : 1
        if (state.wallet.cats < cost)
          return fail(res, 402, 'payment_required', 'You must own at least 1 Cat to create CatBots!', { hint: 'Buy Cats with "magine wallet buy <cats>".' })
        if (jsonBody.name && state.agents.some((a) => a.name === jsonBody.name)) return fail(res, 409, 'conflict', `The name "${jsonBody.name}" is already taken.`)
        state.wallet.cats -= cost
        const a = state.addAgent({
          name: jsonBody.name || `agent-${state.agents.length + 1}`,
          mode: jsonBody.mode || 'agent',
          prompt: jsonBody.prompt,
          workflow: jsonBody.workflow || 'cdp',
        })
        state.lastCreateBody = jsonBody
        const schedule = jsonBody.schedule
          ? { schedule: '0 8 * * *', cron: '0 8 * * *', label: 'every day at 08:00', interpretedFrom: jsonBody.schedule, active: false, needsEnable: true }
          : null
        if (schedule) a.schedule = schedule.schedule
        return json(res, 201, { agent: a, catsCharged: cost, schedule })
      }
      case 'GET agents/:':
        return json(res, 200, { agent: { ...agentRoute, running: state.running.has(agentRoute.id) } })
      case 'PATCH agents/:': {
        const a = agentRoute
        if (jsonBody.prompt !== undefined) a.prompt = jsonBody.prompt
        if (jsonBody.name !== undefined) a.name = jsonBody.name
        if (jsonBody.workflow !== undefined) a.workflow = jsonBody.workflow
        if (typeof jsonBody.active === 'boolean') a.active = jsonBody.active
        return json(res, 200, { agent: a })
      }
      case 'DELETE agents/:':
        state.agents = state.agents.filter((a) => a.id !== agentRoute.id)
        return json(res, 200, { deleted: true, id: agentRoute.id, name: agentRoute.name })
      case 'POST agents/:/run': {
        if (state.running.has(agentRoute.id)) return fail(res, 409, 'conflict', 'This agent is already running.')
        if (state.wallet.tokens < 500) return fail(res, 402, 'payment_required', 'Insufficient tokens', { tokensRequired: 500 })
        const startedAt = startRun(state, agentRoute.id, agentRoute.name, jsonBody.task || agentRoute.prompt, state.nextOutcome || 'success')
        state.nextOutcome = null
        agentRoute.active = true
        state.lastRunBody = jsonBody
        return json(res, 202, { started: true, finished: false, agent: { id: agentRoute.id, name: agentRoute.name }, startedAt })
      }
      case 'POST do': {
        if (!jsonBody.prompt || String(jsonBody.prompt).length < 3) return fail(res, 400, 'bad_request', 'Describe the task (at least 3 characters).')
        if (state.wallet.tokens < 300) return fail(res, 402, 'payment_required', 'Not enough tokens.', { tokensRequired: 300 })
        const startedAt = startRun(state, 'agent-do', 'Quick Task', jsonBody.prompt, state.nextOutcome || 'success')
        state.nextOutcome = null
        return json(res, 202, { started: true, finished: false, agent: { id: 'agent-do', name: 'Quick Task' }, startedAt })
      }
      case 'POST agents/:/stop': {
        const was = state.running.delete(agentRoute.id)
        return json(res, 200, { agent: { id: agentRoute.id, name: agentRoute.name }, stopped: was ? ['in-flight run'] : [], note: was ? '' : 'Bot was already idle.' })
      }
      case 'GET agents/:/runs': {
        const limit = Number(sp.get('limit') || 10)
        return json(res, 200, {
          agent: { id: agentRoute.id, name: agentRoute.name },
          running: state.running.has(agentRoute.id),
          runs: state.runs.get(agentRoute.id).slice(0, limit),
          page: 0,
          nextPage: null,
        })
      }
      case 'GET agents/:/logs':
        return json(res, 200, { agent: { id: agentRoute.id, name: agentRoute.name }, logs: state.logs.get(agentRoute.id) || [], page: 0, nextPage: null })
      case 'PUT agents/:/schedule':
        if (!jsonBody.schedule) return fail(res, 400, 'bad_request', 'Say when to run.')
        agentRoute.schedule = jsonBody.schedule
        state.lastScheduleBody = jsonBody
        return json(res, 200, {
          agent: { id: agentRoute.id, name: agentRoute.name },
          schedule: jsonBody.schedule,
          cron: '0 9 * * *',
          label: 'every day at 09:00',
          interpretedFrom: jsonBody.schedule,
          active: agentRoute.active,
          needsEnable: !agentRoute.active,
        })
      case 'DELETE agents/:/schedule':
        agentRoute.schedule = null
        return json(res, 200, {
          agent: { id: agentRoute.id, name: agentRoute.name },
          schedule: null,
          cron: null,
          label: null,
          interpretedFrom: null,
          active: false,
          needsEnable: false,
        })
      case 'GET cattery':
        return json(res, 200, { items: state.cattery.map(({ content, ...i }) => i), page: 0, nextPage: null, counts: { total: state.cattery.length } })
      case 'POST cattery': {
        const name = sp.get('name')
        if (!name) return fail(res, 400, 'bad_request', 'Name the file.')
        if (!body.length) return fail(res, 400, 'bad_request', 'The file is empty.')
        const item = state.addFile({ name, content: body, kind: name.endsWith('.png') ? 'image' : 'binary', mimeType: 'application/octet-stream' })
        const { content, ...rest } = item
        return json(res, 201, { item: rest })
      }
      case 'GET cattery/:': {
        const item = state.cattery.find((i) => i.id === parts[1])
        if (!item) return fail(res, 404, 'not_found', 'No Cattery file with that id.')
        const { content, ...rest } = item
        return json(res, 200, { item: rest })
      }
      case 'GET cattery/:/content': {
        const item = state.cattery.find((i) => i.id === parts[1])
        if (!item) return fail(res, 404, 'not_found', 'No Cattery file with that id.')
        res.writeHead(200, { 'content-type': item.mimeType, 'content-length': String(item.content.length), 'content-disposition': `attachment; filename="${item.name}"` })
        return res.end(item.content)
      }
      case 'DELETE cattery/:': {
        const item = state.cattery.find((i) => i.id === parts[1])
        if (!item) return fail(res, 404, 'not_found', 'No Cattery file with that id.')
        state.cattery = state.cattery.filter((i) => i !== item)
        return json(res, 200, { deleted: true, id: item.id })
      }
      case 'GET neurons': {
        const q = (sp.get('q') || '').toLowerCase()
        const nodes = [
          { id: 'n1', kind: 'agent', label: 'news-bot', weight: 0.9, details: null },
          { id: 'n2', kind: 'site', label: 'news.ycombinator.com', weight: 0.7, details: null },
          { id: 'n3', kind: 'fact', label: 'Prefers morning reports', weight: 0.4, details: null },
        ].filter((n) => !q || n.label.toLowerCase().includes(q))
        return json(res, 200, {
          nodes,
          links: [{ from: 'n1', to: 'n2', kind: 'visited', weight: 3 }],
          totals: { nodes: 3, links: 1, byKind: { agent: 1, site: 1, fact: 1 } },
          generatedAt: iso(),
        })
      }
      case 'GET wallet':
        return json(res, 200, {
          balance: state.wallet,
          pricing: { catPriceUsd: 5, tokensPerCat: 5_000_000 },
          packages: [
            { cats: 1, priceUsd: 5, tokens: 5_000_000 },
            { cats: 5, priceUsd: 15, tokens: 20_000_000 },
          ],
          providers: ['dodo', 'paypal'],
        })
      case 'GET wallet/usage':
        return json(res, 200, { usage: [{ action: 'catbot_step', tokensUsed: 4321, details: 'Step 1', at: iso() }], page: 0, nextPage: null })
      case 'POST wallet/checkout': {
        const cats = Number(jsonBody.cats)
        if (!Number.isInteger(cats) || cats < 1 || cats > 100) return fail(res, 400, 'bad_request', 'cats must be a whole number from 1 to 100.')
        const orderId = `pay_${hex(6)}`
        state.orders.set(orderId, { provider: jsonBody.provider || 'dodo', cats, checks: 0 })
        return json(res, 201, {
          orderId,
          provider: jsonBody.provider || 'dodo',
          cats,
          tokens: cats * 5_000_000,
          amountUsd: cats * 5,
          url: `https://checkout.example.com/pay/${orderId}`,
        })
      }
      case 'POST wallet/checkout/:/confirm': {
        const order = state.orders.get(parts[2])
        if (!order) return fail(res, 404, 'not_found', 'No order with that id on this account.')
        order.checks++
        const paid = order.checks >= (state.payAfterChecks ?? 2)
        return json(res, 200, {
          orderId: parts[2],
          provider: jsonBody.provider,
          status: paid ? 'completed' : 'pending',
          tokensGranted: paid ? order.cats * 5_000_000 : 0,
          message: paid ? 'Payment verified!' : 'Payment is still processing',
        })
      }
      case 'GET marketplace': {
        const templates = [
          {
            id: 'b'.repeat(24),
            title: 'YouTube Comment Moderator',
            description: 'Keeps your comments clean.',
            mode: 'agent',
            official: true,
            installs: 42,
            runs: 900,
            tags: [],
            requiresLogin: true,
            companions: 1,
            deployCostCats: 2,
            author: 'magine',
            createdAt: iso(),
          },
        ].filter((t) => !sp.get('q') || t.title.toLowerCase().includes(sp.get('q').toLowerCase()))
        return json(res, 200, { open: state.marketplaceOpen, templates: state.marketplaceOpen ? templates : [], page: 0, nextPage: null, total: templates.length })
      }
      case 'GET marketplace/:':
        if (parts[1] !== 'b'.repeat(24)) return fail(res, 404, 'not_found', 'Not found.')
        return json(res, 200, {
          template: {
            id: parts[1],
            title: 'YouTube Comment Moderator',
            description: 'Keeps your comments clean.',
            mode: 'agent',
            official: true,
            installs: 42,
            runs: 900,
            tags: [],
            requiresLogin: true,
            companions: 1,
            deployCostCats: 2,
            author: 'magine',
            createdAt: iso(),
          },
        })
      case 'POST marketplace/:/deploy':
        if (state.wallet.cats < 2) return fail(res, 402, 'payment_required', 'This template needs 2 Cats.', { required: 2, available: state.wallet.cats })
        state.wallet.cats -= 2
        state.addAgent({ name: 'youtube-comment-moderator' })
        return json(res, 201, { agentId: oid(), agents: ['youtube-comment-moderator', 'youtube-reply-helper'], companionFailures: [], catsRemaining: state.wallet.cats })
      case 'GET github/:':
        if (parts[1] === 'nobody') return fail(res, 404, 'not_found', `GitHub user "${parts[1]}" not found.`)
        return json(res, 200, {
          user: parts[1],
          profile: {
            name: 'Sagar',
            joined: '2018-01-01',
            followers: 10,
            publicRepos: 20,
            totalStars: 30,
            totalCommits: 400,
            streakDays: 5,
            mostProductiveHour: 22,
            linesOfCode: 120000,
            codingHours: 12000,
            languages: [{ name: 'TypeScript', percentage: 70, lines: 1000 }],
          },
          ai: { aiPercentage: 35, humanPercentage: 65, confidence: 'high', commitsAnalyzed: 400, tools: [{ tool: 'Claude Code', percentage: 30, commits: 120, prs: 4 }] },
          worth: { estimatedUsdPerYear: 120000, hourlyUsd: 60, tier: 'Senior', confidence: 'medium', topLanguages: ['TypeScript'] },
        })
    }
    return fail(res, 404, 'not_found', `Unknown API endpoint ${req.method} ${url.pathname}`)
  }

  const server = tls ? https.createServer({ key: tls.key, cert: tls.cert }, handler) : http.createServer(handler)
  const sockets = new Set()
  server.on('connection', (s) => {
    sockets.add(s)
    s.on('close', () => sockets.delete(s))
  })
  await new Promise((r) => server.listen(0, '127.0.0.1', r))
  const port = server.address().port
  return {
    state,
    port,
    url: `${tls ? 'https' : 'http'}://127.0.0.1:${port}`,
    close: () =>
      new Promise((r) => {
        for (const s of sockets) s.destroy()
        server.close(() => r())
      }),
  }
}
