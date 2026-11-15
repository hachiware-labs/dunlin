export interface OperationOptions {
  signal?: AbortSignal | undefined;
}

export type CorpusBackend = 'sqlite' | 'json';

export interface OpenOptions extends OperationOptions {
  corpusBackend?: CorpusBackend;
  corpusDirectory?: string;
  keywordNgrams?: boolean;
  keywordBigrams?: boolean;
  keywordJapanese?: { userDictionary?: string };
}

export type RetrievalMethod = 'keyword' | 'coverage';

export interface SearchOptions extends OperationOptions {
  /** Omit or use auto to execute the active plan. */
  mode?: 'auto' | RetrievalMethod;
  limit?: number | undefined;
  latencyBudgetMs?: number | undefined;
  sourceIds?: readonly string[] | undefined;
}

export interface FilesystemSource {
  id: string;
  kind: 'filesystem';
  path: string;
}

/** A document whose text has already been retrieved and decoded. */
export interface SourceDocument {
  /** A stable, nonempty identifier unique within its source. */
  objectId: string;
  /** The original absolute URL, returned as supplied in search evidence. */
  url: string;
  format: 'text' | 'markdown';
  content: string;
  modifiedAtMs?: number;
  corpus?: CorpusMetadata;
  /** Precomputed, consecutive ranges covering the document, including optional headings. */
  chunks?: readonly DocumentChunk[];
}

export interface DocumentChunk {
  heading: string;
  range: LineRange;
  byteRange?: { start: number; end: number };
  searchText?: string;
}

export interface SourceLocation {
  range: LineRange;
  page?: number;
  symbol?: string;
  bbox?: [number, number, number, number];
}

export interface CorpusMetadata {
  locations?: readonly SourceLocation[];
  parser?: { tool: string; model?: string; lang?: string; module?: string; version?: string };
  originalSha256?: string;
}

export interface CorpusDocument extends ReadOutcome, CorpusMetadata {
  format: SourceDocument['format'];
  parsedVersion: string;
}

export interface SourceRef {
  sourceId: string;
  objectId: string;
  url: string;
  version: string;
  modifiedAtMs: number | null;
}

/** One-based inclusive lines in an indexed source object. */
export interface LineRange {
  startLine: number;
  endLine: number;
}

/** A document for Dunlin.chunkDocuments. */
export interface ChunkDocument {
  content: string;
  /** Use Markdown headings as chunk titles. */
  markdown?: boolean;
  /** One-based lines that start a page. Used with structure. */
  boundaries?: readonly number[];
}

export interface ChunkOptions {
  /** Maximum characters per chunk. A grapheme is never split. */
  size: number;
  /** Characters repeated from the end of the previous chunk. */
  overlap: number;
  /** Split at headings and page boundaries instead of preferring sentence ends. */
  structure?: boolean;
  /** Add the NFKC form of each chunk as searchText. */
  nfkc?: boolean;
}

export interface TextChunk {
  heading: string;
  range: LineRange;
  byteRange: { start: number; end: number };
  searchText?: string;
}

export interface Evidence {
  source: SourceRef;
  range: LineRange;
  acquiredAtMs: number;
  byteRange?: { start: number; end: number } | undefined;
}

export interface SearchHit {
  title: string;
  score: number;
  evidence: Evidence;
}

export interface SearchOutcome {
  hits: SearchHit[];
  totalHits: number;
  status: 'complete' | 'fewer_matches';
  engine: 'tantivy-bm25' | 'tantivy-bm25-coverage';
  method: RetrievalMethod;
  planId: string;
  /** Rust execution time in milliseconds, excluding the worker queue and JS conversion. */
  elapsedMs: number;
}

export interface ExploreOptions extends OperationOptions {
  /** The first search and each subsequent document inspection or link visit count as steps. */
  maxSteps?: number | undefined;
  /** Cooperative time limit, including time spent waiting for a native worker. */
  timeoutMs?: number | undefined;
  seedLimit?: number | undefined;
  sourceIds?: readonly string[] | undefined;
}

export interface ExploreEvidence {
  evidence: Evidence;
  via: { kind: 'search' } | { kind: 'link'; from: SourceRef; line: number; destination: string };
}

export interface ExploreIssue {
  code: 'unresolved_link' | 'empty_target' | 'read_failed' | 'link_limit';
  from: SourceRef;
  line: number | null;
  destination: string | null;
}

export interface ExploreOutcome {
  evidence: ExploreEvidence[];
  issues: ExploreIssue[];
  steps: number;
  planId: string;
  status: 'complete' | 'partial' | 'no_matches';
  stopReason: 'exhausted' | 'step_limit' | 'timeout' | 'cancelled' | 'search_failed';
  /** Rust execution time in milliseconds, excluding the worker queue and JS conversion. */
  elapsedMs: number;
}

export interface DocumentRef {
  sourceId: string;
  objectId: string;
}

export type RelevantObject = DocumentRef;

export interface EvaluationCase {
  query: string;
  relevant: readonly RelevantObject[];
}

export interface EvaluationOptions extends OperationOptions {
  /** A UTF-8 JSONL file path or an array of labeled queries. */
  evaluationSet: string | readonly EvaluationCase[];
  limit?: number;
  repetitions?: number;
}

export interface OptimizeOptions extends EvaluationOptions {
  objective?: 'maximize-relevance';
  p95LatencyBudgetMs: number;
}

export interface CandidateMetrics {
  planId: string;
  method: RetrievalMethod;
  recallAtK: number;
  ndcgAtK: number;
  /** Warm Rust search latency, including evidence lookup. */
  p95LatencyMs: number;
  latencySamples: number;
}

export interface EvaluationReport {
  indexRevision: string;
  evaluationSetHash: string;
  queryCount: number;
  limit: number;
  repetitions: number;
  candidates: CandidateMetrics[];
}

export interface OptimizationReport extends EvaluationReport {
  status: 'selected' | 'budget_unmet';
  p95LatencyBudgetMs: number;
  selectedPlanId: string | null;
}

export interface RetrievalPlan {
  id: string;
  version: 1;
  method: RetrievalMethod;
  defaultLimit: number;
  evaluation: { p95LatencyBudgetMs: number; report: EvaluationReport } | null;
}

export interface ReadOutcome {
  content: string;
  evidence: Evidence;
}

export interface DocumentSnapshot {
  revision: string;
  documents: ReadOutcome[];
}

export interface IndexStatus {
  revision: string;
  sources: Record<string, number>;
  searchableDocuments: number;
}

export interface WikiBuildOptions extends OperationOptions {
  id: string;
  title: string;
  documents: readonly DocumentRef[];
}

export interface WikiCitation {
  number: number;
  evidence: Evidence;
  truncated: boolean;
}

export interface WikiChange {
  document: DocumentRef;
  reason: 'updated' | 'deleted' | 'restored';
  previous: SourceRef | null;
  current: SourceRef | null;
}

export interface WikiPage {
  id: string;
  title: string;
  revision: number;
  generatedAtMs: number;
  status: 'current' | 'stale' | 'insufficient_evidence';
  markdown: string;
  citations: WikiCitation[];
  changes: WikiChange[];
  unavailable: { document: DocumentRef; reason: 'missing' | 'empty' }[];
}

export interface WikiImpact {
  id: string;
  title: string;
  revision: number;
  changes: WikiChange[];
}

export interface WikiAffectedOptions extends OperationOptions {
  sourceId?: string | undefined;
}

export interface Wiki {
  /** Creates or replaces a page recipe from the selected documents. */
  build(options: WikiBuildOptions): Promise<WikiPage>;
  /** Returns the page with citations validated against the current index. */
  get(id: string, options?: OperationOptions): Promise<WikiPage | null>;
  affected(options?: WikiAffectedOptions): Promise<WikiImpact[]>;
  /** Rebuilds the page using the saved document identities. */
  regenerate(id: string, options?: OperationOptions): Promise<WikiPage>;
}

export interface SyncOutcome {
  files: number;
  units: number;
}

export interface DeltaSyncOutcome {
  sourceId: string;
  added: string[];
  updated: string[];
  deleted: string[];
  unchanged: number;
  /** Search units written for the added and updated documents. */
  indexedUnits: number;
  wiki: {
    status: 'complete' | 'partial';
    regenerated: { id: string; revision: number; status: WikiPage['status'] }[];
    failures: { pageId: string | null; code: DunlinErrorCode; message: string }[];
  };
}

export interface McpOptions {
  transport: 'stdio';
  searchOnly?: boolean;
  search?: (query: string, options: SearchOptions & { budgetChars?: number | undefined }) => Promise<Record<string, unknown>>;
  read?: (evidence: Evidence, options: OperationOptions) => Promise<ReadOutcome>;
}

export interface McpHandle {
  close(): Promise<void>;
}

export interface McpHttpHandle extends McpHandle {
  fetch(request: Request): Promise<Response>;
}

export type DunlinErrorCode =
  | 'INVALID_ARGUMENT'
  | 'IO_ERROR'
  | 'BUSY'
  | 'ABORTED'
  | 'TIMEOUT'
  | 'NOT_FOUND'
  | 'STALE_EVIDENCE'
  | 'INDEX_ERROR'
  | 'PLAN_NOT_FOUND'
  | 'PLAN_ERROR'
  | 'WIKI_NOT_FOUND'
  | 'WIKI_ERROR'
  | 'CLOSED'
  | 'NATIVE_UNAVAILABLE'
  | 'MCP_UNAVAILABLE';

export class DunlinError extends Error {
  constructor(readonly code: DunlinErrorCode, message: string) {
    super(message);
    this.name = 'DunlinError';
  }
}
