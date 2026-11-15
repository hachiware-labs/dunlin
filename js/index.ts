import { addAbortListener } from 'node:events';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';

import { DunlinError } from './types.js';
import type {
  ChunkDocument, ChunkOptions, CorpusBackend, CorpusDocument, DeltaSyncOutcome, DocumentRef, DocumentSnapshot, DunlinErrorCode, EvaluationOptions, EvaluationReport, Evidence, ExploreOptions, ExploreOutcome, FilesystemSource, IndexStatus, McpHandle, McpHttpHandle,
  McpOptions, OpenOptions, OperationOptions, OptimizationReport, OptimizeOptions, ReadOutcome, RetrievalMethod,
  RetrievalPlan, SearchOptions, SearchOutcome, SourceDocument, SyncOutcome, TextChunk, Wiki, WikiBuildOptions,
  WikiImpact, WikiPage,
} from './types.js';

export * from './types.js';

interface NativeCancellation {
  abort(): void;
}

interface JapaneseToken {
  start: number;
  end: number;
  term: string | null;
  reading: string | null;
}

interface NativeDatabase {
  addSource(id: string, path: string, cancel: NativeCancellation): Promise<SyncOutcome>;
  replaceDocuments(id: string, request: string, cancel: NativeCancellation): Promise<SyncOutcome>;
  syncSource(id: string, path: string, cancel: NativeCancellation): Promise<DeltaSyncOutcome>;
  syncDocuments(id: string, request: string, cancel: NativeCancellation): Promise<DeltaSyncOutcome>;
  removeSource(id: string, cancel: NativeCancellation): Promise<void>;
  search(query: string, limit: number | undefined, budget: number | undefined, method: RetrievalMethod | undefined, sourceIds: readonly string[] | undefined, cancel: NativeCancellation): Promise<SearchOutcome>;
  explore(query: string, maxSteps: number, timeoutMs: number, seedLimit: number | undefined, sourceIds: readonly string[] | undefined, cancel: NativeCancellation): Promise<ExploreOutcome>;
  evaluate(options: string, cancel: NativeCancellation): Promise<EvaluationReport>;
  optimize(options: string, cancel: NativeCancellation): Promise<OptimizationReport>;
  getActivePlan(cancel: NativeCancellation): Promise<RetrievalPlan>;
  activatePlan(id: string, cancel: NativeCancellation): Promise<RetrievalPlan>;
  snapshotDocuments(cancel: NativeCancellation): Promise<DocumentSnapshot>;
  getIndexRevision(cancel: NativeCancellation): Promise<string>;
  getCorpusDocument(sourceId: string, objectId: string, version: string, cancel: NativeCancellation): Promise<CorpusDocument | null>;
  getDocument(sourceId: string, objectId: string, cancel: NativeCancellation): Promise<ReadOutcome | null>;
  read(evidence: string, cancel: NativeCancellation): Promise<ReadOutcome>;
  wikiBuild(options: string, cancel: NativeCancellation): Promise<WikiPage>;
  wikiGet(id: string, cancel: NativeCancellation): Promise<WikiPage | null>;
  wikiAffected(sourceId: string | undefined, cancel: NativeCancellation): Promise<WikiImpact[]>;
  wikiRegenerate(id: string, cancel: NativeCancellation): Promise<WikiPage>;
  close(): Promise<void>;
}

interface NativeBinding {
  NativeCancellation: new () => NativeCancellation;
  openNative(path: string, cancel: NativeCancellation, backend?: CorpusBackend, keywordNgrams?: boolean, keywordBigrams?: boolean,
    corpusPath?: string, keywordJapanese?: OpenOptions['keywordJapanese']): Promise<NativeDatabase>;
  inspectIndex(path: string, cancel: NativeCancellation): Promise<IndexStatus | null>;
  analyzeJapanese(texts: readonly string[], dictionary: string | undefined, cancel: NativeCancellation): Promise<JapaneseToken[][]>;
  mergeUserDictionary(existing: string | undefined, selected: string, cancel: NativeCancellation): Promise<string>;
  rankVectors(path: string, request: string, cancel: NativeCancellation): Promise<{ i: number; score: number }[]>;
  chunkDocuments(request: string): string;
  acquireExecutionLock(path: string): { release(): void };
}

const require = createRequire(import.meta.url);
let binding: NativeBinding | undefined;

function native(): NativeBinding {
  try {
    return binding ??= require('./native.cjs') as NativeBinding;
  } catch {
    throw new DunlinError(
      'NATIVE_UNAVAILABLE',
      `Could not load the Dunlin addon for ${process.platform}/${process.arch}, Node ${process.versions.node}. Install a package built for this platform.`,
    );
  }
}

async function cancellable<T>(
  signal: AbortSignal | undefined,
  run: (cancel: NativeCancellation) => Promise<T>,
  returnPartial = false,
): Promise<T> {
  if (signal?.aborted && !returnPartial) {
    throw new DunlinError('ABORTED', 'Operation aborted');
  }
  const cancel = new (native().NativeCancellation)();
  if (signal?.aborted) {
    cancel.abort();
  }
  const listener = signal && !signal.aborted ? addAbortListener(signal, () => cancel.abort()) : undefined;
  try {
    return await run(cancel);
  } catch (error) {
    throw translateNativeError(error);
  } finally {
    listener?.[Symbol.dispose]();
  }
}

function translateNativeError(error: unknown): DunlinError {
  if (error instanceof DunlinError) return error;
  const match = error instanceof Error && /^(INVALID_ARGUMENT|IO_ERROR|BUSY|ABORTED|TIMEOUT|NOT_FOUND|STALE_EVIDENCE|INDEX_ERROR|PLAN_NOT_FOUND|PLAN_ERROR|WIKI_NOT_FOUND|WIKI_ERROR)\|(.+)$/u.exec(error.message);
  return match ? new DunlinError(match[1] as DunlinErrorCode, match[2]) : new DunlinError('INDEX_ERROR', 'The native operation failed');
}

function absolutePath(path: string): string {
  if (typeof path !== 'string' || path.length === 0 || path.includes('\0')) {
    throw new DunlinError('INVALID_ARGUMENT', 'Expected a nonempty filesystem path');
  }
  return resolve(path);
}

function filesystemPath(source: FilesystemSource): string {
  if (source?.kind !== 'filesystem' || typeof source.id !== 'string') {
    throw new DunlinError('INVALID_ARGUMENT', 'Expected a filesystem source with a source ID');
  }
  return absolutePath(source.path);
}

function documentsRequest(sourceId: string, documents: readonly SourceDocument[]): string {
  if (typeof sourceId !== 'string' || !Array.isArray(documents)) {
    throw new DunlinError('INVALID_ARGUMENT', 'Expected a source ID and an array of documents');
  }
  return JSON.stringify(Array.from(documents, (document: SourceDocument) => {
    const input: Partial<SourceDocument> = document ?? {};
    const { objectId, url, format, content } = input;
    if (typeof objectId !== 'string' || typeof url !== 'string'
      || typeof format !== 'string' || typeof content !== 'string') {
      throw new DunlinError('INVALID_ARGUMENT', 'Expected document objectId, url, format, and content strings');
    }
    return { objectId, url, format, content, corpus: input.corpus, modifiedAtMs: input.modifiedAtMs, chunks: input.chunks };
  }));
}

function integer(value: number, min: number, max: number, name: string): void {
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    throw new DunlinError('INVALID_ARGUMENT', `${name} must be an integer between ${min} and ${max}`);
  }
}

function validateQuery(query: string): void {
  if (typeof query !== 'string' || Buffer.byteLength(query) > 4096) {
    throw new DunlinError('INVALID_ARGUMENT', 'Expected a query of at most 4096 UTF-8 bytes');
  }
}

function validateScope(ids: readonly string[] | undefined): void {
  if (ids !== undefined && (!Array.isArray(ids) || ids.length < 1 || ids.length > 100
    || Array.from(ids).some((id) => typeof id !== 'string' || !/^[a-zA-Z0-9_.-]{1,64}$/u.test(id)))) {
    throw new DunlinError('INVALID_ARGUMENT', 'Expected 1–100 valid source IDs');
  }
}

function evaluationRequest(options: EvaluationOptions, p95LatencyBudgetMs?: number): string {
  const { evaluationSet, limit = 10, repetitions = 3 } = options;
  integer(limit, 1, 100, 'limit');
  integer(repetitions, 1, 20, 'repetitions');
  const input = typeof evaluationSet === 'string' ? absolutePath(evaluationSet) : evaluationSet;
  if (typeof input !== 'string' && (!Array.isArray(input) || input.length < 1 || input.length > 1000)) {
    throw new DunlinError('INVALID_ARGUMENT', 'Expected a JSONL path or 1–1000 evaluation cases');
  }
  let serialized: string;
  try {
    serialized = JSON.stringify({ evaluationSet: input, limit, repetitions, p95LatencyBudgetMs });
  } catch {
    throw new DunlinError('INVALID_ARGUMENT', 'Evaluation options must be JSON serializable');
  }
  if (Buffer.byteLength(serialized) > 4 * 1024 * 1024) {
    throw new DunlinError('INVALID_ARGUMENT', 'Evaluation options exceed 4 MiB');
  }
  return serialized;
}

function evidenceInput(evidence: Evidence): Evidence {
  const source = evidence?.source;
  const range = evidence?.range;
  if (!source || !range
    || typeof source.sourceId !== 'string' || typeof source.objectId !== 'string'
    || typeof source.url !== 'string' || typeof source.version !== 'string'
    || !(source.modifiedAtMs === null || Number.isSafeInteger(source.modifiedAtMs))) {
    throw new DunlinError('INVALID_ARGUMENT', 'Expected source evidence from search');
  }
  integer(range.startLine, 1, 2 ** 20, 'startLine');
  integer(range.endLine, range.startLine, 2 ** 20, 'endLine');
  const bytes = evidence.byteRange;
  if (bytes !== undefined) {
    integer(bytes?.start, 0, 2 ** 20 - 1, 'byteRange.start');
    integer(bytes?.end, bytes.start + 1, 2 ** 20, 'byteRange.end');
  }
  integer(evidence.acquiredAtMs, 0, Number.MAX_SAFE_INTEGER, 'acquiredAtMs');
  return {
    source: {
      sourceId: source.sourceId, objectId: source.objectId, url: source.url,
      version: source.version, modifiedAtMs: source.modifiedAtMs,
    },
    range: { startLine: range.startLine, endLine: range.endLine },
    acquiredAtMs: evidence.acquiredAtMs,
    ...(bytes && { byteRange: { start: bytes.start, end: bytes.end } }),
  };
}

function validateWikiId(id: string): void {
  if (typeof id !== 'string' || id.length === 0 || id.length > 128) {
    throw new DunlinError('INVALID_ARGUMENT', 'Expected a Wiki page ID');
  }
}

function wikiRequest({ id, title, documents }: WikiBuildOptions): string {
  validateWikiId(id);
  if (typeof title !== 'string' || Buffer.byteLength(title) > 512) {
    throw new DunlinError('INVALID_ARGUMENT', 'Expected a Wiki title of at most 512 UTF-8 bytes');
  }
  if (!Array.isArray(documents) || documents.length < 2 || documents.length > 32) {
    throw new DunlinError('INVALID_ARGUMENT', 'A Wiki page requires 2–32 distinct documents');
  }
  const references = Array.from(documents, (document) => {
    const { sourceId, objectId } = document ?? {};
    if (typeof sourceId !== 'string' || sourceId.length > 64
      || typeof objectId !== 'string' || Buffer.byteLength(objectId) > 128 * 1024) {
      throw new DunlinError('INVALID_ARGUMENT', 'Expected sourceId and objectId for each Wiki document');
    }
    return { sourceId, objectId };
  });
  return JSON.stringify({ id, title, documents: references });
}

export class Dunlin {
  #database: NativeDatabase | undefined;
  #lifetime = new AbortController();
  #pending = new Set<Promise<unknown>>();
  #closing: Promise<void> | undefined;

  readonly wiki: Wiki = {
    build: async (options) => {
      const request = wikiRequest(options);
      return this.#run(options.signal, (database, cancel) => database.wikiBuild(request, cancel));
    },
    get: async (id, { signal } = {}) => {
      validateWikiId(id);
      return this.#run(signal, (database, cancel) => database.wikiGet(id, cancel));
    },
    affected: async ({ sourceId, signal } = {}) => {
      if (sourceId !== undefined && typeof sourceId !== 'string') {
        throw new DunlinError('INVALID_ARGUMENT', 'Expected a source ID');
      }
      return this.#run(signal, (database, cancel) => database.wikiAffected(sourceId, cancel));
    },
    regenerate: async (id, { signal } = {}) => {
      validateWikiId(id);
      return this.#run(signal, (database, cancel) => database.wikiRegenerate(id, cancel));
    },
  };

  private constructor(database: NativeDatabase) {
    this.#database = database;
  }

  static async open(path: string, {
    signal, corpusBackend, corpusDirectory, keywordNgrams, keywordBigrams, keywordJapanese,
  }: OpenOptions = {}): Promise<Dunlin> {
    const directory = absolutePath(path);
    if (corpusBackend !== undefined && corpusBackend !== 'sqlite' && corpusBackend !== 'json') {
      throw new DunlinError('INVALID_ARGUMENT', 'Expected corpus backend sqlite or json');
    }
    const corpusPath = corpusDirectory === undefined ? undefined : absolutePath(corpusDirectory);
    const dictionary = keywordJapanese?.userDictionary;
    const japanese = keywordJapanese === undefined ? undefined : dictionary === undefined ? {} : { userDictionary: absolutePath(dictionary) };
    const database = await cancellable(signal, (cancel) =>
      native().openNative(directory, cancel, corpusBackend, keywordNgrams, keywordBigrams, corpusPath, japanese));
    return new Dunlin(database);
  }

  /** Inspects an existing keyword index without creating it. */
  static async inspectIndex(path: string, { signal }: OperationOptions = {}): Promise<IndexStatus | null> {
    const directory = absolutePath(path);
    return cancellable(signal, (cancel) => native().inspectIndex(directory, cancel));
  }

  /** Inspect search terms and their UTF-8 byte ranges using the Lindera keyword tokenizer. */
  static async analyzeJapanese(texts: readonly string[], dictionary?: string,
    { signal }: OperationOptions = {}): Promise<JapaneseToken[][]> {
    const path = dictionary === undefined ? undefined : absolutePath(dictionary);
    return cancellable(signal, (cancel) => native().analyzeJapanese(texts, path, cancel));
  }

  /** Validate and combine two Lindera CSVs while keeping the input files intact. */
  static async mergeUserDictionary(existing: string | undefined, selected: string,
    { signal }: OperationOptions = {}): Promise<string> {
    const path = existing === undefined ? undefined : absolutePath(existing);
    return cancellable(signal, (cancel) => native().mergeUserDictionary(path, absolutePath(selected), cancel));
  }

  /** Score a completed immutable float32 vector artifact. Omit positions to search all rows. */
  static async rankVectors(path: string, options: { needle: readonly number[]; positions?: readonly number[]; rows: number; limit: number },
    { signal }: OperationOptions = {}): Promise<{ i: number; score: number }[]> {
    return cancellable(signal, (cancel) => native().rankVectors(resolve(path), JSON.stringify(options), cancel));
  }

  /**
   * Split documents into character windows with the chunk.length and chunk.structure rules.
   * Runs synchronously on the calling thread; pass a few documents at a time to stay responsive.
   */
  static chunkDocuments(documents: readonly ChunkDocument[], options: ChunkOptions): TextChunk[][] {
    try {
      return JSON.parse(native().chunkDocuments(JSON.stringify({ documents, ...options }))) as TextChunk[][];
    } catch (error) {
      throw translateNativeError(error);
    }
  }

  /** Acquire a nonblocking process lock. Closing it releases ownership. */
  static acquireExecutionLock(path: string): { release(): void } {
    try {
      return native().acquireExecutionLock(absolutePath(path));
    } catch (error) {
      throw translateNativeError(error);
    }
  }

  /** Registers or atomically replaces the indexed snapshot for this source ID. */
  async addSource(source: FilesystemSource, { signal }: OperationOptions = {}): Promise<SyncOutcome> {
    const path = filesystemPath(source);
    return this.#run(signal, (database, cancel) => database.addSource(source.id, path, cancel));
  }

  /** Atomically replaces all indexed documents for this source ID. */
  async replaceDocuments(
    sourceId: string,
    documents: readonly SourceDocument[],
    { signal }: OperationOptions = {},
  ): Promise<SyncOutcome> {
    const request = documentsRequest(sourceId, documents);
    return this.#run(signal, (database, cancel) => database.replaceDocuments(sourceId, request, cancel));
  }

  /** Applies filesystem changes and regenerates the Wiki pages affected by this source. */
  async syncSource(source: FilesystemSource, { signal }: OperationOptions = {}): Promise<DeltaSyncOutcome> {
    const path = filesystemPath(source);
    return this.#run(signal, (database, cancel) => database.syncSource(source.id, path, cancel));
  }

  /** Applies differences against the full set of current documents for this source. */
  async syncDocuments(
    sourceId: string,
    documents: readonly SourceDocument[],
    { signal }: OperationOptions = {},
  ): Promise<DeltaSyncOutcome> {
    const request = documentsRequest(sourceId, documents);
    return this.#run(signal, (database, cancel) => database.syncDocuments(sourceId, request, cancel));
  }

  async removeSource(id: string, { signal }: OperationOptions = {}): Promise<void> {
    if (typeof id !== 'string') {
      throw new DunlinError('INVALID_ARGUMENT', 'Expected a source ID');
    }
    return this.#run(signal, (database, cancel) => database.removeSource(id, cancel));
  }

  async search(query: string, options: SearchOptions = {}): Promise<SearchOutcome> {
    const { mode = 'auto', limit, latencyBudgetMs, sourceIds, signal } = options;
    validateQuery(query);
    validateScope(sourceIds);
    if (mode !== 'auto' && mode !== 'keyword' && mode !== 'coverage') {
      throw new DunlinError('INVALID_ARGUMENT', 'Unknown retrieval mode');
    }
    if (limit !== undefined) {
      integer(limit, 1, 100, 'limit');
    }
    if (latencyBudgetMs !== undefined) {
      integer(latencyBudgetMs, 1, 2 ** 32 - 1, 'latencyBudgetMs');
    }
    const method = mode === 'auto' ? undefined : mode;
    return this.#run(signal, (database, cancel) => database.search(query, limit, latencyBudgetMs, method, sourceIds, cancel));
  }

  /** Searches the selected sources and follows indexed Markdown links. */
  async explore(query: string, options: ExploreOptions = {}): Promise<ExploreOutcome> {
    const { maxSteps = 12, timeoutMs = 1000, seedLimit, sourceIds, signal } = options;
    validateQuery(query);
    validateScope(sourceIds);
    integer(maxSteps, 1, 100, 'maxSteps');
    integer(timeoutMs, 1, 60_000, 'timeoutMs');
    if (seedLimit !== undefined) {
      integer(seedLimit, 1, 100, 'seedLimit');
    }
    return this.#run(signal, (database, cancel) => database.explore(query, maxSteps, timeoutMs, seedLimit, sourceIds, cancel), true);
  }

  /** Compares built-in plans against the same immutable index snapshot. */
  async evaluate(options: EvaluationOptions): Promise<EvaluationReport> {
    const request = evaluationRequest(options);
    return this.#run(options.signal, (database, cancel) => database.evaluate(request, cancel));
  }

  /** Evaluates and saves the highest-quality plan within the budget. */
  async optimize(options: OptimizeOptions): Promise<OptimizationReport> {
    const { objective, p95LatencyBudgetMs } = options;
    if (objective !== undefined && objective !== 'maximize-relevance') {
      throw new DunlinError('INVALID_ARGUMENT', 'Unknown optimization objective');
    }
    if (!Number.isFinite(p95LatencyBudgetMs) || p95LatencyBudgetMs <= 0) {
      throw new DunlinError('INVALID_ARGUMENT', 'P95 latency budget must be finite and positive');
    }
    const request = evaluationRequest(options, p95LatencyBudgetMs);
    return this.#run(options.signal, (database, cancel) => database.optimize(request, cancel));
  }

  async getActivePlan({ signal }: OperationOptions = {}): Promise<RetrievalPlan> {
    return this.#run(signal, (database, cancel) => database.getActivePlan(cancel));
  }

  /** Persists the plan ID and switches subsequent default searches on this handle. */
  async activatePlan(id: string, { signal }: OperationOptions = {}): Promise<RetrievalPlan> {
    if (typeof id !== 'string' || id.length === 0 || id.length > 100) {
      throw new DunlinError('INVALID_ARGUMENT', 'Expected a retrieval plan ID');
    }
    return this.#run(signal, (database, cancel) => database.activatePlan(id, cancel));
  }

  /** Exports stored documents and their evidence from one index revision. */
  async snapshotDocuments({ signal }: OperationOptions = {}): Promise<DocumentSnapshot> {
    return this.#run(signal, (database, cancel) => database.snapshotDocuments(cancel));
  }

  async getIndexRevision({ signal }: OperationOptions = {}): Promise<string> {
    return this.#run(signal, (database, cancel) => database.getIndexRevision(cancel));
  }

  async getDocument(document: DocumentRef, { signal }: OperationOptions = {}): Promise<ReadOutcome | null> {
    if (typeof document?.sourceId !== 'string' || typeof document.objectId !== 'string' || !document.objectId) {
      throw new DunlinError('INVALID_ARGUMENT', 'Expected source and object IDs');
    }
    return this.#run(signal, (database, cancel) => database.getDocument(document.sourceId, document.objectId, cancel));
  }

  /** Reads the full parsed text and metadata of an archived revision. */
  async getCorpusDocument(
    document: DocumentRef,
    version: string,
    { signal }: OperationOptions = {},
  ): Promise<CorpusDocument | null> {
    if (typeof document?.sourceId !== 'string' || typeof document.objectId !== 'string' || !document.objectId
      || typeof version !== 'string' || !/^[a-f0-9]{64}$/u.test(version)) {
      throw new DunlinError('INVALID_ARGUMENT', 'Expected source and object IDs and a document version');
    }
    return this.#run(signal, (database, cancel) => database.getCorpusDocument(document.sourceId, document.objectId, version, cancel));
  }

  /** Reads the indexed bytes from the requested version and line range. */
  async read(evidence: Evidence, { signal }: OperationOptions = {}): Promise<ReadOutcome> {
    const input = evidenceInput(evidence);
    return this.#run(signal, (database, cancel) => database.read(JSON.stringify(input), cancel));
  }

  async serveMcp({ transport, search, read, searchOnly }: McpOptions = { transport: 'stdio' }): Promise<McpHandle> {
    this.#requireDatabase();
    if (transport !== 'stdio') {
      throw new DunlinError('INVALID_ARGUMENT', 'The supported MCP transport is stdio');
    }
    const { serveStdioMcp } = await import('./mcp.js').catch((error: unknown) => {
      if (error instanceof Error && 'code' in error && error.code === 'ERR_MODULE_NOT_FOUND') {
        throw new DunlinError('MCP_UNAVAILABLE', 'Install @modelcontextprotocol/server and zod to enable MCP');
      }
      throw error;
    });
    this.#requireDatabase();
    return serveStdioMcp(this, search, read, searchOnly);
  }

  async createMcpHttpHandler(search: NonNullable<McpOptions['search']>, maxRequestBodySize: number): Promise<McpHttpHandle> {
    this.#requireDatabase();
    const { serveHttpMcp } = await import('./mcp.js');
    this.#requireDatabase();
    return serveHttpMcp(this, search, maxRequestBodySize);
  }

  close(): Promise<void> {
    if (!this.#closing) {
      const database = this.#database;
      this.#database = undefined;
      this.#lifetime.abort();
      this.#closing = Promise.allSettled(this.#pending).then(() => database?.close());
    }
    return this.#closing;
  }

  #requireDatabase(): NativeDatabase {
    if (!this.#database) {
      throw new DunlinError('CLOSED', 'Dunlin is closed');
    }
    return this.#database;
  }

  async #run<T>(
    signal: AbortSignal | undefined,
    run: (database: NativeDatabase, cancel: NativeCancellation) => Promise<T>,
    returnPartial = false,
  ): Promise<T> {
    const database = this.#requireDatabase();
    const lifetime = this.#lifetime.signal;
    const combined = signal ? AbortSignal.any([signal, lifetime]) : lifetime;
    const pending = cancellable(combined, (cancel) => run(database, cancel), returnPartial);
    this.#pending.add(pending);
    try {
      return await pending;
    } finally {
      this.#pending.delete(pending);
    }
  }
}
