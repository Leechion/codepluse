'use strict';

// Shared reference adapter used by POST /api/agents for HTTP(S) endpoints.
// POST /messages receives a v1.0 envelope and returns
// { ok: true, output: 'optional reply' }. Failures must not return ok: true.
//
// const HttpAdapter = require('./adapters/http-adapter.example');
// const adapter = new HttpAdapter({
//   endpoint: 'http://127.0.0.1:9001', token: process.env.BRIDGE_TOKEN
// });
// await adapter.send(envelope);
//
// HTTP connections support messaging and do not join simulation scheduling.
module.exports = require('../core/http-adapter');
