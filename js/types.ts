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

/** A file for Dunlin.loadCorpus. */
export interface LoadCorpusFile {
  id: string;
  file: string;
  objectId: string;
  /** text, html, office, pdf, ocr or command. Only text is read in Rust; the rest go to the host. */
  kind: string;
  /** The parser module instance, reported in warnings. */
  route: string;
  /** For command files, the identity that keys the extraction cache. */
  command?: unknown;
  /** Set by Rust for host calls: the file name without directories. */
  name?: string;
}

export interface LoadCorpusRequest {
  /** The project's state.sqlite, which maps cached files to corpus versions. */
  state: string;
  /** The shared corpus directory, which holds the extraction results in corpus.sqlite. */
  corpus: string;
  /** The shared originals directory. */
  originals: string;
  force?: boolean;
  /** The OCR tool version, part of the cache key for OCR files. */
  ocrVersion: string;
  /** `oar-ocr`, run in Rust, or `lightonocr` (the default), run as a separate process. */
  ocrTool?: 'oar-ocr' | 'lightonocr';
  /** The folder `dunlin add oar-ocr` prepared. */
  ocrHome?: string;
  /** The runtime `dunlin add lightonocr` prepared. */
  lighton?: LightonRuntime;
  /** The project root, for `{root}` in loader.command arguments. */
  root: string;
  files: LoadCorpusFile[];
}

export interface LoadCorpusHost {
  parse(file: LoadCorpusFile, bytes: Buffer): Promise<unknown> | unknown;
  progress?(completed: number): void;
}

export interface LoadedCorpus {
  documents: Record<string, unknown>[];
  warnings: { code: string; section: string; module: string; target: string; message: string }[];
}

/** The managed LightOnOCR runtime: Python, the server script, the model folder and its name. */
export interface LightonRuntime {
  python: string;
  server: string;
  modelDir: string;
  model: string;
}

/** A parser route for `Dunlin.routeFiles` and `Dunlin.runSource`. */
export interface ParserRoute {
  name: string;
  category: string;
  match?: readonly string[];
  ocr?: boolean;
}

/** The `source` stage over scanned sources (spec 7). */
export interface SourceStageRequest {
  state: string;
  /** The shared corpus directory. */
  corpus: string;
  originals: string;
  root: string;
  force?: boolean;
  skipUnsupported?: boolean;
  /** A parser module sets `unsupported: error`. */
  reject?: boolean;
  ocrVersion: string;
  ocrTool?: 'oar-ocr' | 'lightonocr';
  ocrHome?: string;
  lighton?: LightonRuntime;
  /** Scanned sources in order, each file as `{ file, objectId }`. */
  sources: { id: string; files: { file: string; objectId: string }[] }[];
  routes: ParserRoute[];
  /** `{ module, args }` of each loader.command instance. */
  commands?: { module: string; args: unknown }[];
  /** The stage's module settings, hashed into its configuration ID. */
  modules: unknown;
}

/** The recorded `source` stage, or the unsupported files that stopped it. */
export type SourceStageOutcome =
  | { status: 'done'; output: Record<string, unknown> & { id: string; config_id: string }; reused: boolean; warnings: LoadedCorpus['warnings'] }
  | { status: 'unsupported'; stats: { files: number; skipped: { source_id: string; path: string; reason: string }[] } };

/** An embedding model for `chunk.semantic`, resolved from `models`: the full endpoint URL and key. */
export interface EmbedderSettings {
  provider: 'ollama' | 'openai';
  model: string;
  url: string;
  apiKey?: string;
  maxInputBytes?: number;
}

/** The `transform` stage of a recorded `source` stage (spec 28.9 step 2b). */
export interface TransformStageRequest {
  state: string;
  /** The shared corpus directory, which holds the chunk sets. */
  corpus: string;
  sourceId: string;
  force?: boolean;
  /** `moduleSettings(plan)` for the transform stage. */
  modules: unknown[];
  embedder?: EmbedderSettings | null;
}

export type TransformStageOutcome =
  | { status: 'done'; output: Record<string, unknown> & { id: string; key: string; config_id: string; source_id: string; chunks: string[][] }; reused: boolean }
  | { status: 'failed'; error: { code?: string; message: string; exitCode?: number } };

/** The `index` stage of a project build: indexes a recorded transform stage and records the build. */
export interface IndexStageRequest {
  /** state.sqlite of the project. */
  state: string;
  /** `.dunlin/artifacts`: build directories and dictionary snapshots. */
  artifacts: string;
  /** The shared corpus directory. */
  corpus: string;
  /** The project root. A failed build removes its directory only inside it. */
  root: string;
  runId: string;
  transformId: string;
  /** The `index.keyword` arguments. */
  keyword?: Record<string, unknown> | null;
  /** `{ backend, settings }` of `index.vector` or `hannoy.index`. */
  vector?: { backend: 'exact' | 'hannoy'; settings: Record<string, unknown> } | null;
  embedder?: EmbedderSettings | null;
  /** `moduleSettings(plan.search)`, saved with the build. */
  search: unknown;
  /** The model settings of rerank modules, part of the configuration ID. */
  rerank: unknown[];
  /** `moduleSettings(plan.index)`. */
  construction: unknown;
  /** The configuration saved with the build. */
  configuration: unknown;
  force?: boolean;
  /** Make the build the serving build. */
  activate?: boolean;
  tag?: string;
  /** `max(adoption_history.id)` when the build started. */
  generation?: number | null;
}

/** Answers the `index` stage's questions. Throw to stop the stage. */
export interface IndexStageHost {
  /** Whether the configuration file changed since the build started. */
  configChanged?(): boolean | Promise<boolean>;
  /** The path of the installed hannoy tool, after its integrity check. */
  indexTool?(name: string): string | Promise<string>;
}

export type IndexStageOutcome =
  | { status: 'done'; output: Record<string, unknown> & { build_id: string; config_id: string; serving_changed: boolean; reused: boolean };
    warnings: { code: string; message: string }[] }
  | { status: 'failed'; error: { code?: string; message: string; exitCode?: number; data?: unknown } };

/** A build command (`build`, `source`, `transform` or `index`) of a project, run by the engine. */
export interface BuildCommandRequest {
  command: 'build' | 'source' | 'transform' | 'index';
  /** Absent for dry runs. */
  runId?: string;
  /** Records the run as a trial within its limits (`build --tag` and `--config`). */
  trial?: TrialRequest;
  from?: string;
  until?: string;
  /** A build ID or tag whose saved stages feed `from`. */
  input?: string;
  tag?: string;
  /** The settings come from a candidate configuration (`--config`). */
  candidate?: boolean;
  force?: boolean;
  skipUnsupported?: boolean;
  dryRun?: boolean;
  /** Milliseconds already spent on the run before this call, such as scanning. */
  elapsedMs?: number;
  state: string;
  artifacts: string;
  corpus: string;
  root: string;
  /** The folders to scan (`{ path, exclude, name }`), routes and the other settings of the source stage. */
  source: Record<string, unknown> & { modules: unknown; scans?: { path: string; exclude?: readonly string[]; name: string }[] };
  transform: { modules: unknown; embedder?: EmbedderSettings | null };
  /** The `index` stage settings of IndexStageRequest, without the run, stage and serving fields. */
  index: Record<string, unknown> & { construction: unknown };
  /** The `index` section of the configuration, compared by dry runs. */
  configIndex?: unknown;
}

export type BuildCommandHost = LoadCorpusHost & IndexStageHost;

export type BuildCommandOutcome =
  | { status: 'done'; output: Record<string, unknown>; warnings: { code: string; message: string }[] }
  | { status: 'failed'; error: { code?: string; message: string; exitCode?: number; data?: unknown } };

/** `dunlin tag`: names a build (the serving build when `reference` is absent). */
export interface TagBuildRequest {
  state: string;
  name: string;
  reference?: string;
  move?: boolean;
  /** `moduleSettings(plan.search)`, for the serving build. */
  search: unknown;
  /** The `search` and `models` sections saved with a tag of the serving build. */
  configSearch?: unknown;
  configModels?: unknown;
}

export interface TagBuildHost {
  /** The search configuration ID of a build, with its saved search settings. */
  searchConfigId(build: Record<string, unknown>): string | Promise<string>;
}

/** A project search (spec 28.10 step 3a): the search plan of a build and the request's overrides. */
export interface ProjectSearchRequest {
  /** `moduleSettings(plan)` of the search stage. */
  plan: unknown[];
  query: string;
  limit?: number;
  budgetChars?: number;
  sourceIds?: readonly string[];
  /** The build manifest. */
  build: Record<string, unknown>;
  /** The build's index directory. */
  directory: string;
  /** Embedding models by name: resolved settings, or `{ error: { code, message, exitCode } }`. */
  embedders?: Record<string, EmbedderSettings | { error: { code?: string; message: string; exitCode?: number } }>;
  /** The resolved model of each `rerank.model`, in plan order, for the search configuration ID. */
  rerankModels?: unknown[];
  /** The chat models of `rerank.model` by name: resolved settings, or `{ error }`. */
  rerankers?: Record<string, GeneratorSettings | { error: { code?: string; message: string; exitCode?: number } }>;
}

/** A generation model resolved from `models`: the full endpoint URL and the key to send. */
export interface GeneratorSettings {
  provider: 'openai' | 'ollama' | 'strata';
  model: string;
  url: string;
  apiKey?: string;
  maxInputBytes?: number;
}

export interface ProjectSearchHost {
  /** The path of the installed hannoy tool, after its integrity check. */
  indexTool?(name: string): string | Promise<string>;
}

/** An answer from a search result (spec 28.10 step 3b): the ask plan, the search output and the model. */
export interface AnswerRequest {
  /** `moduleSettings(plan)` of the ask stage. */
  plan: unknown[];
  /** The search result in the format of spec 27.1. */
  search: Record<string, unknown>;
  query: string;
  model: GeneratorSettings;
}

export type AnswerOutcome =
  | { status: 'done'; output: Record<string, unknown> }
  | { status: 'failed'; error: { code?: string; message: string; exitCode?: number; module?: string } };

export type ProjectSearchOutcome =
  | { status: 'done'; output: Record<string, unknown>; context: Evidence[]; contextStop: string }
  | { status: 'failed'; error: { code?: string; message: string; exitCode?: number; module?: string } };

/** An `eval` run (spec 28.11 step 4a): the search of each question and the evaluation settings. */
export interface EvaluateProjectRequest {
  /** The search plan and models; `query` is replaced by each question. */
  search: ProjectSearchRequest;
  /** The path of `state.sqlite`. */
  state: string;
  /** The build manifest the report is for. */
  build: Record<string, unknown>;
  /** `eval-data.use` of the configuration. */
  dataset?: string;
  /** The validated `eval` settings with `contextBudget`, `construction` and `searchModules`. */
  settings: Record<string, unknown>;
  split?: string;
  tag?: string;
  runId: string;
  /** Records the run as a trial within its limits (`eval --build` and `--config`). */
  trial?: TrialRequest;
}

export type EvaluateProjectOutcome =
  | { status: 'done'; report: Record<string, unknown> }
  | { status: 'failed'; error: { code?: string; message: string; exitCode?: number; module?: string } };

/** An `evidence-map` command (spec 28.11 step 4a). */
export interface EvidenceMapRequest {
  state: string;
  action: 'list' | 'show' | 'create' | 'import' | 'confirm';
  /** The map ID, or the file to import. */
  reference?: string;
  /** `eval-data.use` of the configuration, for `create`. */
  dataset?: string;
  /** The build manifest, for `create`, `import` and `confirm`. */
  build?: Record<string, unknown>;
  key?: string;
  start?: string;
  end?: string;
  reviewer?: string;
}

export type EvidenceMapOutcome =
  | { status: 'done'; output: unknown }
  | { status: 'failed'; error: { code?: string; message: string; exitCode?: number } };

/** An `eval-data` command (spec 28.11 step 4e). */
export interface EvalDataRequest {
  state: string;
  action: string;
  positional?: string;
  other?: string;
  /** `eval-data.use` of the configuration. */
  selected?: string;
  /** The text of the file `import` reads. */
  input?: string;
  /** The command options. */
  values?: Record<string, unknown>;
}

export type EvalDataOutcome =
  | { status: 'done'; output: unknown; select?: string }
  | { status: 'failed'; error: { code?: string; message: string; exitCode?: number } };

/** A build to adopt with `use` (spec 28.11 step 4c). */
export interface AdoptBuildRequest {
  state: string;
  buildId: string;
  /** The search configuration ID of the adopted configuration. */
  searchId: string;
  /** The configuration before and after, without secrets, as JSON text. */
  previous: string;
  adopted: string;
  /** `eval-data.use` and the `eval` settings of the adopted configuration. */
  dataset?: string;
  settings?: Record<string, unknown> | null;
}

/** Writes the configuration file during adoption, and restores it after a failure. Throw to stop. */
export interface AdoptBuildHost {
  writeConfig(): void | Promise<void>;
  restoreConfig(): void | Promise<void>;
}

/** The trials a run belongs to, which Rust records and limits (spec 28.11 step 4c). */
export interface TrialRequest {
  state: string;
  id: string;
  operation: string;
  limits?: Record<string, number>;
}

/** `report show`, `list` or `compare` (spec 28.11 step 4b). */
export interface EvaluationReportsRequest {
  state: string;
  action: 'show' | 'list' | 'compare';
  refs?: readonly string[];
  split?: string;
}

/** The evaluation status of a build and search configuration (spec 28.11 step 4b). */
export interface EvaluationStatusRequest {
  state: string;
  buildId: string;
  searchId: string;
  /** `eval-data.use` of the configuration. */
  dataset?: string;
  /** The validated `eval` settings, or `null` without an `eval` section. */
  settings?: Record<string, unknown> | null;
}

/** The report that dictionary suggestions use: `report`, or the latest development report. */
export interface DictionaryEvaluationRequest {
  state: string;
  report?: string;
  dataset?: string;
  includeGenerated?: boolean;
}

/** The evidence a development report missed (spec 28.11 step 4b). */
export interface DictionaryMissesRequest {
  state: string;
  report: Record<string, unknown>;
  data: Record<string, unknown>;
}

export type ReportOutcome<T> =
  | { status: 'done'; output: T }
  | { status: 'failed'; error: { code?: string; message: string; exitCode?: number } };

export type TagBuildOutcome =
  | { status: 'done'; output: { tag: string; build_id: string; search_config_id: string } }
  | { status: 'failed'; error: { code?: string; message: string; exitCode?: number } };

/** A configuration diagnostic (spec 26.8). */
export interface ConfigDiagnostic {
  code: string;
  file: string;
  line: number | null;
  section: string | null;
  module: string | null;
  arg: string | null;
  received: unknown;
  expected: unknown;
  suggestions: string[];
  hint: string;
}

/** A configuration problem reported without its file. */
export interface ConfigIssue {
  code: string;
  path: (string | number)[];
  expected: unknown;
  hint: string;
}

/** Dunlin.validateConfig: diagnostics, and the module plan for each stage. */
export interface ConfigValidation {
  errors: ConfigDiagnostic[];
  warnings: ConfigDiagnostic[];
  plan: Record<string, { use: string; name: unknown; args: Record<string, unknown> }[]>;
  serveSettings: unknown;
  evalSettings: unknown;
}

/** Dunlin.parseConfig: the configuration and value lines, or a syntax error. */
export type ParsedConfig =
  | { config: unknown; lines: [(string | number)[], number][]; syntaxError?: undefined }
  | { syntaxError: { line: number | null; message: string }; config?: undefined; lines?: undefined };

/** How dunlin modules describe prints one module argument. */
export interface ModuleArgInfo {
  type?: string;
  default?: unknown;
  choices?: string[];
  min?: number;
  max?: number;
  optional?: true;
  effect: 'output' | 'speed';
}

/** A registered module (spec 26.1). */
export interface ModuleDescription {
  name: string;
  version: number;
  stage: string;
  role: string;
  input: string;
  output: string;
  description: string;
  deterministic: boolean;
  external: boolean;
  unit: 'document' | 'collection';
  requires: string[];
  tool?: string;
  args: Record<string, ModuleArgInfo>;
}

/** One argument read by Dunlin.readModuleArgs. */
export interface ModuleArgResult {
  key: string;
  /** Absent for an optional argument that was not given, and for invalid values. */
  value?: unknown;
  defaulted?: boolean;
  code?: 'ARG_MISSING' | 'ARG_TYPE' | 'ARG_RANGE';
  /** The argument description used in diagnostics. */
  expected: ModuleArgInfo;
}

/** A file read by Dunlin.readSourceFiles: its bytes, or why it could not be read. */
export interface SourceFileRead {
  bytes?: Buffer | null;
  modifiedAtMs?: number | null;
  /** not_file, too_large, changed (while it was read) or io. */
  error?: 'not_file' | 'too_large' | 'changed' | 'io' | null;
  message?: string | null;
}

/** An evidence range read from its saved document revision. */
export interface EvidenceExcerpt {
  text: string;
  /** Characters (Unicode code points) before the excerpt in the document. */
  start: number;
  /** start plus the characters in text. */
  end: number;
  /** Source locations, such as pages, that overlap the evidence lines. */
  locations: SourceLocation[];
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

/** A quote that answers a question, in a document `sourceId:objectId`, with the hints of `eval-data`. */
export interface EvidenceQuote {
  doc: string;
  quote: string;
  prefix?: string;
  suffix?: string;
  page?: number;
  heading?: string;
}

/** A question and its evidence quotes, or one quote given directly as in `eval-data import`. */
export type EvaluationCase =
  | { question: string; evidence: readonly EvidenceQuote[] }
  | ({ question: string } & EvidenceQuote);

export interface EvaluationOptions extends OperationOptions {
  /** A UTF-8 JSONL file path or an array of questions with evidence quotes. */
  evaluationSet: string | readonly EvaluationCase[];
  limit?: number;
  repetitions?: number;
  /** A quote of at most this many characters must be covered completely. Default 40. */
  shortEvidenceChars?: number;
  /** The share of a longer quote's characters the results must cover. Default 0.8. */
  minCoverage?: number;
}

export interface OptimizeOptions extends EvaluationOptions {
  objective?: 'maximize-relevance';
  p95LatencyBudgetMs: number;
}

export interface CandidateMetrics {
  planId: string;
  method: RetrievalMethod;
  /** The share of evidence quotes the top `limit` results cover, averaged over questions. */
  evidenceRecallAtK: number;
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
  /** Quotes found once in their documents, and the others, which count as missed. */
  resolvedEvidence: number;
  unresolvedEvidence: number;
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
  /** Plans saved before evidence-level scoring keep their document-level report. */
  evaluation: { p95LatencyBudgetMs: number; report: EvaluationReport | Record<string, unknown> } | null;
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
  /** Settles when the client closes the connection. */
  closed?: Promise<void>;
}

/** The `knowledge.search` arguments a project answers: one spelling of each option. */
export interface McpSearchArguments {
  query: string;
  limit?: number;
  sourceIds?: string[];
  budgetChars?: number;
}

/** What a Rust MCP server asks of a project's JavaScript. */
export interface McpHost {
  mcpSearch(arguments_: McpSearchArguments): Promise<Record<string, unknown>>;
  mcpRead(evidence: Evidence): Promise<ReadOutcome>;
}

/** A project to serve (spec 28.10 step 3c): the validated serve section, paths, plans and answer model. */
export interface ServeProjectRequest {
  projectType: 'mcp' | 'rag';
  settings: Record<string, unknown>;
  /** The token of `auth: token`, read from its environment variable. */
  token?: string | null;
  paths: { state: string; artifacts: string; corpus: string };
  /** `moduleSettings(plan.search)` and `moduleSettings(plan.ask)`. */
  search: unknown[];
  ask?: unknown[];
  model?: GeneratorSettings | null;
}

/** What serving asks of a project's JavaScript. */
export interface ServeProjectHost {
  indexTool?(name: string): string | Promise<string>;
  /** `{ embedders, rerankers, rerankModels }` for a build, or `{ error }`. */
  searchModels(build: Record<string, unknown>): Record<string, unknown>;
  /** The server listens: the port for HTTP, and the serving build. */
  listening?(address: { port?: number; buildId: string }): void;
  /** The server opened the index directory of a build. */
  opened?(directory: string): void;
}

export type DunlinErrorCode =
  | 'LOAD_FAILED'
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
