// One statement as the playground shows it (src/play-result.ts), analyzed in the reader's tab by the
// worker (lib/live/worker.ts), at build time by the page's loader (lib/live/play-data.ts) for the
// examples, and on the server by the playground's API (lib/live/api.ts), so one function answers all three.
import type { AnswerCache, EngineCore, EngineIo } from '../../../src/engine-core.ts';
import { playResult, type PlayResult } from '../../../src/play-result.ts';

export type LiveResult = PlayResult;
export type LiveDiagnostic = PlayResult['diagnostics'][number];

/** The width a type is broken at: what the playground's column of types holds. */
export const TYPE_WIDTH = 46;

/**
 * The analysis with each enum value an object. The playground's engine is the latest release's
 * (scripts/playground-engine.ts), which may write an enum's values as plain strings, as the
 * format did before each value became an object: each such string is read as `{ value }`.
 */
function enumValuesAsObjects<T>(v: T): T {
  if (Array.isArray(v)) {
    v.forEach(enumValuesAsObjects);
  } else if (v !== null && typeof v === 'object') {
    const o = v as Record<string, unknown>;
    if (o.kind === 'enum' && Array.isArray(o.values)) o.values = o.values.map((x) => (typeof x === 'string' ? { value: x } : x));
    Object.values(o).forEach(enumValuesAsObjects);
  }
  return v;
}

export async function analyzeStatement(engine: EngineCore, io: EngineIo, cache: AnswerCache, sql: string, width = TYPE_WIDTH): Promise<LiveResult> {
  const t = performance.now();
  let lexed;
  try {
    lexed = engine.lex(sql, 'postgres');
  } catch {}
  const options = { width };
  try {
    const analysis = enumValuesAsObjects(await engine.analyze(sql, 'postgres', io, cache));
    return playResult(sql, 'postgres', { analysis, lexed }, { ...options, ms: performance.now() - t });
  } catch (e) {
    return playResult(sql, 'postgres', { error: e as Error }, { ...options, ms: performance.now() - t });
  }
}
