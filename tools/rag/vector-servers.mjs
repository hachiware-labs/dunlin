export function chromaServer(url, apiKey) {
  const tenant = process.env.CHROMA_TENANT ?? 'default_tenant';
  const database = process.env.CHROMA_DATABASE ?? 'default_database';
  const path = `/api/v2/tenants/${encodeURIComponent(tenant)}/databases/${encodeURIComponent(database)}/collections`;
  const server = connection('Chroma', url ?? process.env.CHROMA_URL ?? 'http://127.0.0.1:8000',
    'x-chroma-token', apiKey ?? process.env.CHROMA_API_KEY);

  return {
    identity: ['chroma', server.base, tenant, database],
    async create(name) {
      const result = await server.request('POST', path, { name, configuration: { hnsw: { space: 'cosine' } } });
      if (typeof result?.id !== 'string' || !result.id) throw new Error('Chroma did not return a collection ID');
      return result.id;
    },
    async upload(collection, points) {
      await server.request('POST', `${path}/${encodeURIComponent(collection)}/upsert`, {
        ids: points.map(({ id }) => String(id)),
        embeddings: points.map(({ vector }) => vector),
        metadatas: points.map(({ sourceId }) => ({ sourceId })),
      });
    },
    async search(collection, vector, sourceIds, limit) {
      const result = await server.request('POST', `${path}/${encodeURIComponent(collection)}/query`, {
        query_embeddings: [vector], n_results: limit, include: [],
        ...(sourceIds && { where: { sourceId: { $in: sourceIds } } }),
      });
      return result?.ids?.[0];
    },
    remove: (name) => server.request('DELETE', `${path}/${encodeURIComponent(name)}`),
  };
}

export function qdrantServer(url, apiKey) {
  const server = connection('Qdrant', url ?? process.env.QDRANT_URL ?? 'http://127.0.0.1:6333',
    'api-key', apiKey ?? process.env.QDRANT_API_KEY);
  const path = (name) => `/collections/${encodeURIComponent(name)}`;

  return {
    identity: ['qdrant', server.base],
    async create(name, dimensions) {
      await server.request('PUT', path(name), { vectors: { size: dimensions, distance: 'Cosine' } });
      return name;
    },
    async upload(collection, points) {
      await server.request('PUT', `${path(collection)}/points?wait=true`, {
        points: points.map(({ id, vector, sourceId }) => ({ id, vector, payload: { sourceId } })),
      });
    },
    async search(collection, vector, sourceIds, limit) {
      const result = await server.request('POST', `${path(collection)}/points/query`, {
        query: vector, limit, with_payload: false, with_vector: false,
        ...(sourceIds && { filter: { must: [{ key: 'sourceId', match: { any: sourceIds } }] } }),
      });
      return result?.result?.points?.map(({ id }) => id);
    },
    remove: (name) => server.request('DELETE', path(name)),
  };
}

function connection(name, value, keyHeader, key) {
  const url = new URL(value);
  if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new Error(`Expected an HTTP(S) ${name} base URL without credentials, query, or fragment`);
  }
  const base = url.href.replace(/\/+$/u, '');
  return {
    base,
    async request(method, path, body) {
      const response = await fetch(base + path, {
        method, headers: { 'content-type': 'application/json', ...(key && { [keyHeader]: key }) },
        body: body === undefined ? undefined : JSON.stringify(body),
        redirect: 'error', signal: AbortSignal.timeout(30_000),
      });
      if (!response.ok) {
        await response.body?.cancel();
        throw new Error(`${name} ${method} failed (HTTP ${response.status})`);
      }
      const text = await response.text();
      return text ? JSON.parse(text) : undefined;
    },
  };
}
