// The text `reylo analyze --format typescript` prints for a statement, written by the content
// mapper's renderer, which the binary's port is held to: `Params` where the statement takes any, and
// what `.run()` resolves to, laid out in 80 columns as `reylo play` lays them out (src/play-result.ts).
import type { Codecs } from "../src/codecs.ts";
import type { Analysis, Dialect } from "../src/engine-core.ts";
import { moduleTypes } from "../src/module-types.ts";
import { formatType, resultType } from "../src/type-format.ts";

const WIDTH = 80;

/** A type laid out, or as it is where the layout reads no form in it (a codec's `import("…")` type). */
function laidOut(type: string): string {
  try {
    return formatType(type, WIDTH);
  } catch {
    return type;
  }
}

/**
 * The declarations of a statement's Params, Row and cardinality, as its module declares them. A
 * statement the engine could not analyze has Params of `unknown`, which no analyzed one has, and
 * resolves to `unknown`; an analyzed one's Row is the element of its cardinality, `unknown` too.
 */
export function typescriptText(params: string, row: string, cardinality: string): string {
  const returns = laidOut(params === "unknown" ? "unknown" : resultType(row, cardinality));
  return `${params === "Record<never, never>" ? "" : `type Params = ${laidOut(params)};\n`}type Returns = ${returns};\n`;
}

/** The text for a statement's analysis, or for one the engine could not analyze. */
export function typescriptOf(sql: string, analysis: Analysis | undefined, dialect: Dialect, codecs: Codecs, allowNestedArrays: boolean): string {
  if (analysis === undefined) return typescriptText("unknown", "unknown", "many");
  const types = moduleTypes(sql, analysis, dialect, undefined, false, undefined, codecs, allowNestedArrays && dialect === "postgres");
  return typescriptText(types.params, types.row, types.cardinality);
}

/** The text for the module the content mapper wrote for a statement, read off its declarations; undefined for a module of no statement. */
export function typescriptOfModule(text: string): string | undefined {
  const types = /^export type Params = ([\s\S]*?);\nexport type Row = ([\s\S]*?);\nexport type QueryError = /m.exec(text);
  const cardinality = /declare const query: Query<[\s\S]*, "(one|maybeOne|many|none)", "(?:postgres|mysql|sqlite|clickhouse)"/.exec(text);
  if (!types || !cardinality) return undefined;
  return typescriptText(types[1]!, types[2]!, cardinality[1]!);
}
