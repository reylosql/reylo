import results from '@/lib/compare-results.json';

/**
 * The docs' comparison, /compare: which tools and rows it shows. Every mark comes from
 * lib/compare-results.json, which lab/compare writes from real runs of each tool.
 */
export type CompareCell = { state: 'y' | 'p' | 'n'; note?: string };

export const compareResults = results as unknown as { at: string; tools: Record<string, { name: string; version: string; cells: Record<string, CompareCell> }> };

/** The tools beside Reylo, in an order that keeps the longer names from sitting side by side. */
export const compareTools = [
  { id: 'sqlc', name: 'sqlc', sub: 'Go', href: 'https://sqlc.dev' },
  { id: 'pgtyped', name: 'PgTyped', sub: 'TypeScript', href: 'https://pgtyped.dev' },
  { id: 'sqlx', name: 'sqlx', sub: 'Rust', href: 'https://github.com/transact-rs/sqlx' },
  { id: 'typedsql', name: 'TypedSQL', sub: 'Prisma', href: 'https://www.prisma.io/docs/orm/prisma-client/using-raw-sql/typedsql' },
  { id: 'safeql', name: 'SafeQL', sub: 'ESLint', href: 'https://safeql.dev' },
  { id: 'typesql', name: 'TypeSQL', sub: 'TypeScript', href: 'https://github.com/wsporto/typesql' },
  { id: 'postgrestypedsql', name: 'Typed SQL', sub: 'Postgres', href: 'https://github.com/fli/postgres-typed-sql' },
];

/** The ORMs and query builders, in the second table. */
export const compareOrms = [
  { id: 'prisma', name: 'Prisma', sub: 'ORM', href: 'https://www.prisma.io/orm' },
  { id: 'drizzle', name: 'Drizzle', sub: 'ORM', href: 'https://orm.drizzle.team' },
  { id: 'kysely', name: 'Kysely', sub: 'Query builder', href: 'https://kysely.dev' },
  { id: 'typeorm', name: 'TypeORM', sub: 'ORM', href: 'https://typeorm.io' },
];

/** The rows, in one table, each with what it asks of a tool, shown when its label is hovered. */
export const compareRows: { id: string; label: string; asks?: string }[] = [
  { id: 'no-codegen', label: 'No codegen', asks: 'Types appear with nothing to generate or commit.' },
  { id: 'from-the-database', label: 'Live database', asks: 'Types come from your running database, not from a copy of its schema.' },
  { id: 'outer-joins', label: 'Nullable left joins', asks: 'A column from the joined side of a `left join` is typed as possibly null, and the other side is not.' },
  { id: 'expression-nullability', label: 'Nullable expressions', asks: 'It knows that `count(*)` is never null and that `max(x)` can be.' },
  { id: 'parameter-types', label: 'Parameter types', asks: 'Each parameter is typed from the query, with no annotation.' },
  { id: 'parameter-nullability', label: 'Nullable parameters', asks: 'A parameter written to a nullable column takes `null`, and one compared with `=` does not.' },
  { id: 'cardinality', label: 'Cardinality', asks: 'It works out how many rows a query returns: at most one for a lookup by primary key, exactly one for `count(*)`.' },
  { id: 'json-shapes', label: 'Nested JSON', asks: 'The JSON a query builds, as with `json_agg(json_build_object(…))`, is typed by its shape.' },
  { id: 'json-schema', label: 'JSON Schema types', asks: 'A `jsonb` column is typed by the JSON Schema its `CHECK` holds it to.' },
  { id: 'postgres', label: 'Postgres' },
  { id: 'mysql', label: 'MySQL' },
  { id: 'sqlite', label: 'SQLite' },
];

/** The rows of the ORM table: what the SQL table asks that a query built in TypeScript can be asked, then each shape of query an ORM's API may or may not type. Every ORM runs on every database, so the table has no row for each. */
const row = (id: string) => compareRows.find((r) => r.id === id)!;
export const compareOrmRows: { id: string; label: string; asks?: string }[] = [
  row('from-the-database'),
  row('outer-joins'),
  row('expression-nullability'),
  { id: 'aggregates', label: 'Aggregates', asks: 'Each column of a `group by` query is typed, with the null each aggregate can return.' },
  { id: 'windows', label: 'Window functions', asks: 'A window function is typed, as `row_number() over (…)` and `lag(…)`.' },
  { id: 'subqueries', label: 'Subqueries', asks: 'A scalar subquery, an `exists` and a derived table are typed by what they return.' },
  { id: 'with-queries', label: 'WITH queries', asks: 'The columns of a `with` query are typed where the main query reads them, recursive ones included.' },
  { id: 'set-operations', label: 'Set operations', asks: 'A `union` or an `except` is typed by both its sides.' },
  { id: 'returning', label: 'Returning rows', asks: 'The row an `insert`, `update`, `delete` or upsert returns is typed.' },
  { id: 'enums', label: 'Enums', asks: 'An enum column is typed as the union of its values.' },
  row('parameter-types'),
  row('parameter-nullability'),
  row('cardinality'),
  { ...row('json-shapes'), asks: 'Rows nested in a row, as a user with their posts, are typed by their shape.' },
  row('json-schema'),
];

/** Which tools and rows a `<CompareTable />` shows: the SQL tools, or `set="orm"`. */
export const compareSet = (set?: 'orm') => (set === 'orm' ? { tools: compareOrms, rows: compareOrmRows } : { tools: compareTools, rows: compareRows });

export const compareCell = (tool: string, row: string): CompareCell => compareResults.tools[tool]?.cells[row] ?? { state: 'n' };

/** The page's `<CompareTable />`s as Markdown, for /compare.md and /llms.txt. */
export function expandCompare(text: string): string {
  const word = { y: 'yes', p: 'partly', n: 'no' };
  return text.replace(/<CompareTable(?: set="(orm)")? \/>/g, (_, set?: 'orm') => {
    const { tools, rows } = compareSet(set);
    const head = `| | Reylo | ${tools.map((t) => `[${t.id === 'postgrestypedsql' ? 'Postgres Typed SQL' : t.name}](${t.href})`).join(' | ')} |`;
    const rule = `| --- | --- | ${tools.map(() => '---').join(' | ')} |`;
    const lines = rows.map((r) => `| ${r.label} | ${word[compareCell('reylo', r.id).state]} | ${tools.map((t) => word[compareCell(t.id, r.id).state]).join(' | ')} |`);
    return [head, rule, ...lines].join('\n');
  });
}
