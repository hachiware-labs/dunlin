import { Dunlin, DunlinError } from 'dunlin';
import type { DeltaSyncOutcome, DocumentSnapshot, EvaluationReport, Evidence, ExploreOutcome, OptimizationReport, ReadOutcome, RetrievalPlan, SearchOutcome, SourceDocument, SyncOutcome } from 'dunlin';

async function publicContract(signal: AbortSignal, evidence: Evidence): Promise<void> {
  const database: Dunlin = await Dunlin.open('./knowledge', { signal });
  try {
    await database.addSource({ id: 'docs', kind: 'filesystem', path: './docs' }, { signal });
    const documents: readonly SourceDocument[] = [
      { objectId: 'intro', url: 'https://example.invalid/intro', format: 'markdown', content: '# Intro' },
    ];
    const imported: SyncOutcome = await database.replaceDocuments('web', documents, { signal });
    void imported;
    const delta: DeltaSyncOutcome = await database.syncDocuments('web', documents, { signal });
    const files: DeltaSyncOutcome = await database.syncSource({ id: 'docs', kind: 'filesystem', path: './docs' }, { signal });
    void delta;
    void files;
    const snapshot: DocumentSnapshot = await database.snapshotDocuments({ signal });
    const revision: string = await database.getIndexRevision({ signal });
    const document: ReadOutcome | null = await database.getDocument({ sourceId: 'docs', objectId: 'intro.md' }, { signal });
    void snapshot;
    void revision;
    void document;
    const result: SearchOutcome = await database.search('認証方式', { mode: 'keyword', limit: 5, signal });
    const source: ReadOutcome = await database.read(result.hits[0]?.evidence ?? evidence, { signal });
    const content: string = source.content;
    void content;
    const evaluation: EvaluationReport = await database.evaluate({ evaluationSet: './eval.jsonl', signal });
    const optimized: OptimizationReport = await database.optimize({
      evaluationSet: './eval.jsonl', limit: evaluation.limit, p95LatencyBudgetMs: 10, signal,
    });
    const plan: RetrievalPlan = optimized.selectedPlanId
      ? await database.activatePlan(optimized.selectedPlanId, { signal })
      : await database.getActivePlan({ signal });
    await database.search('OAuth', { mode: plan.method });
    const exploration: ExploreOutcome = await database.explore('OAuth', {
      sourceIds: ['docs'], maxSteps: 5, timeoutMs: 100, signal,
    });
    const linked = exploration.evidence.find(({ via }) => via.kind === 'link');
    if (linked?.via.kind === 'link') {
      const line: number = linked.via.line;
      void line;
    }
    // @ts-expect-error Vector search is outside the current public contract.
    await database.search('OAuth', { mode: 'vector' });
  } catch (error) {
    if (error instanceof DunlinError && error.code === 'ABORTED') return;
    throw error;
  } finally {
    await database.close();
  }
}

void publicContract;
