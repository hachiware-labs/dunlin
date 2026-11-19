import { createHash } from 'node:crypto';
import { isIP } from 'node:net';

import { Dunlin } from 'dunlin';

// Serving runs in Rust (spec 28.10 step 3c and 28.12 step 5b). JavaScript checks the serve section,
// applies the command line overrides and reads the access token.

export const isSearchProject = (type) => type === 'mcp' || type === 'rag';

const limits = { max_request_bytes: 1048576, request_timeout_seconds: 120, max_concurrent: 8 };
const access = { allowed_hosts: ['localhost', '127.0.0.1'], allowed_origins: [], auth: { type: 'none', token_env: null } };
const endpoints = { rag: ['responses', 'search', 'mcp'], wiki: ['web'], para: ['web'] };

export function serveTemplate(type) {
  return type === 'mcp' ? { transport: 'stdio' } : {
    transport: 'http', host: '127.0.0.1', port: type === 'rag' ? 4318 : 4319,
    endpoints: [...endpoints[type]], access: structuredClone(access), limits: { ...limits },
    ...(isSearchProject(type) ? {} : { pages: './site' }),
  };
}

/** Validates a serve section in Rust and reports each problem through `issue`. */
export function validateServe(type, input, issue) {
  const { settings, issues } = Dunlin.validateServe(type, input);
  for (const { code, path, expected, hint } of issues) issue(code, path, expected, hint);
  return settings;
}

export function withServeOverrides(type, settings, values, issue) {
  if (values.transport !== undefined && !['http', 'stdio'].includes(values.transport)) {
    issue('ARG_RANGE', ['serve', 'transport'], 'http or stdio', 'Use --transport http or --transport stdio.');
  }
  if (values.transport === 'stdio' && (values.host !== undefined || values.port !== undefined)) {
    issue('ARG_CONFLICT', ['serve', 'transport'], 'stdio without --host or --port', 'Use HTTP to override the host or port.');
  }
  const transport = values.transport ?? settings.transport;
  const next = transport === settings.transport ? { ...settings } : transport === 'stdio' ? { transport } : { ...serveTemplate(type), transport };
  if (values.host !== undefined) next.host = values.host;
  if (values.port !== undefined) next.port = /^\d+$/u.test(values.port) ? Number(values.port) : values.port;
  return validateServe(type, next, issue);
}

export function serveAddress(host, port) {
  return 'http://' + (isIP(host) === 6 ? '[' + host + ']' : host) + ':' + port;
}

export function serveCredential(settings) {
  const { auth } = settings.access;
  const token = auth.type === 'token' ? process.env[auth.token_env] : null;
  if (auth.type === 'token' && (!token || !/^[!-~]+$/u.test(token))) {
    throw Object.assign(new Error('Set a nonempty token in the configured environment variable.'), { code: 'AUTH_TOKEN_REQUIRED', exitCode: 2 });
  }
  return token && createHash('sha256').update(token).digest();
}
