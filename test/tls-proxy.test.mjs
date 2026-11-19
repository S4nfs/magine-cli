import test from 'node:test'
import assert from 'node:assert/strict'
import http from 'node:http'
import net from 'node:net'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { startMockServer } from './helpers/mock-server.mjs'
import { runCli, tempDir } from './helpers/cli.mjs'

/** The test CA that signed the mock server's certificate: what a company proxy's CA file would be. */
const CA = fileURLToPath(new URL('./fixtures/test-ca.pem', import.meta.url))
const tls = { cert: readFileSync(new URL('./fixtures/test-cert.pem', import.meta.url)), key: readFileSync(new URL('./fixtures/test-key.pem', import.meta.url)) }

/** A CONNECT proxy that sends every tunnel to the local HTTPS server and records what it saw. */
async function startProxy({ requireAuth, refuse } = {}) {
  const seen = []
  const server = http.createServer((req, res) => {
    seen.push({ method: req.method, url: req.url, headers: req.headers })
    res.writeHead(400).end()
  })
  server.on('connect', (req, client, head) => {
    seen.push({ method: 'CONNECT', url: req.url, headers: req.headers })
    // The scheme name is case-insensitive (RFC 7617); the credentials are not.
    const [scheme, creds] = String(req.headers['proxy-authorization'] || '').split(' ')
    const authorized = !requireAuth || (scheme.toLowerCase() === 'basic' && creds === requireAuth.split(' ')[1])
    if (refuse || !authorized) {
      client.end('HTTP/1.1 407 Proxy Authentication Required\r\n\r\n')
      return
    }
    const port = Number(req.url.split(':').pop())
    const upstream = net.connect(port, '127.0.0.1', () => {
      client.write('HTTP/1.1 200 Connection Established\r\n\r\n')
      if (head.length) upstream.write(head)
      upstream.pipe(client)
      client.pipe(upstream)
    })
    upstream.on('error', () => client.destroy())
    client.on('error', () => upstream.destroy())
  })
  await new Promise((r) => server.listen(0, '127.0.0.1', r))
  const sockets = new Set()
  server.on('connection', (s) => {
    sockets.add(s)
    s.on('close', () => sockets.delete(s))
  })
  const close = () =>
    new Promise((r) => {
      for (const s of sockets) s.destroy()
      server.close(() => r())
    })
  return { seen, port: server.address().port, close }
}

async function setup(t) {
  const api = await startMockServer({ tls })
  const tmp = tempDir()
  t.after(async () => {
    await api.close()
    tmp.cleanup()
  })
  return { api, key: api.state.addKey('web'), dir: tmp.dir }
}

test('TLS certificates are verified: an untrusted one is refused (exit 8)', async (t) => {
  const { api, key, dir } = await setup(t)
  const r = await runCli(['whoami', '--json'], { env: { MAGINE_API_URL: api.url, MAGINE_API_KEY: key, MAGINE_CONFIG_DIR: dir } })
  assert.equal(r.code, 8, r.stderr)
  assert.equal(r.errorJson.error.code, 'tls')
  assert.match(r.errorJson.error.hint, /MAGINE_CA_CERTS/)
  assert.equal(api.state.requests.length, 0)
})

test('MAGINE_CA_CERTS trusts a company certificate', async (t) => {
  const { api, key, dir } = await setup(t)
  const r = await runCli(['whoami', '--json'], { env: { MAGINE_API_URL: api.url, MAGINE_API_KEY: key, MAGINE_CONFIG_DIR: dir, MAGINE_CA_CERTS: CA } })
  assert.equal(r.code, 0, r.stderr)
  assert.equal(r.json.user.username, 'sagar')
})

test('NODE_EXTRA_CA_CERTS and SSL_CERT_FILE are honoured too', async (t) => {
  const { api, key, dir } = await setup(t)
  for (const name of ['NODE_EXTRA_CA_CERTS', 'SSL_CERT_FILE']) {
    const r = await runCli(['whoami', '--json'], { env: { MAGINE_API_URL: api.url, MAGINE_API_KEY: key, MAGINE_CONFIG_DIR: dir, [name]: CA } })
    assert.equal(r.code, 0, `${name}: ${r.stderr}`)
  }
})

test('a CA file that cannot be read is a warning, not a crash', async (t) => {
  const { api, key, dir } = await setup(t)
  const r = await runCli(['whoami'], { env: { MAGINE_API_URL: api.url, MAGINE_API_KEY: key, MAGINE_CONFIG_DIR: dir, MAGINE_CA_CERTS: CA + '.missing' } })
  assert.equal(r.code, 8)
  assert.match(r.stderr, /warning: MAGINE_CA_CERTS is set to/)
})

test('HTTPS_PROXY: the request goes through a CONNECT tunnel; the proxy never sees the key', async (t) => {
  const { api, key, dir } = await setup(t)
  const proxy = await startProxy({ requireAuth: `Basic ${Buffer.from('alice:s3cret').toString('base64')}` })
  t.after(() => proxy.close())
  const env = {
    MAGINE_API_URL: `https://magine.test:${api.port}`,
    MAGINE_API_KEY: key,
    MAGINE_CONFIG_DIR: dir,
    MAGINE_CA_CERTS: CA,
    HTTPS_PROXY: `http://alice:s3cret@127.0.0.1:${proxy.port}`,
  }
  const r = await runCli(['agents', 'list', '--json'], { env })
  assert.equal(r.code, 0, r.stderr)
  assert.deepEqual(r.json.agents, [])
  assert.equal(proxy.seen.length, 1)
  assert.equal(proxy.seen[0].method, 'CONNECT')
  assert.equal(proxy.seen[0].url, `magine.test:${api.port}`)
  assert.equal(proxy.seen[0].headers.authorization, undefined)
  assert.equal(api.state.requests[0].headers.authorization, `Bearer ${key}`)
})

test('a proxy that refuses the tunnel is a network error (exit 8)', async (t) => {
  const { api, key, dir } = await setup(t)
  const proxy = await startProxy({ refuse: true })
  t.after(() => proxy.close())
  const r = await runCli(['whoami', '--json'], {
    env: { MAGINE_API_URL: `https://magine.test:${api.port}`, MAGINE_API_KEY: key, MAGINE_CONFIG_DIR: dir, MAGINE_CA_CERTS: CA, HTTPS_PROXY: `http://127.0.0.1:${proxy.port}` },
  })
  assert.equal(r.code, 8)
  assert.match(r.errorJson.error.message, /refused the tunnel \(HTTP 407\)/)
})

test('requests to this computer never go through the proxy', async (t) => {
  const { api, key, dir } = await setup(t)
  const proxy = await startProxy()
  t.after(() => proxy.close())
  const r = await runCli(['whoami', '--json'], {
    env: { MAGINE_API_URL: api.url, MAGINE_API_KEY: key, MAGINE_CONFIG_DIR: dir, MAGINE_CA_CERTS: CA, HTTPS_PROXY: `http://127.0.0.1:${proxy.port}` },
  })
  assert.equal(r.code, 0, r.stderr)
  assert.equal(proxy.seen.length, 0)
})
