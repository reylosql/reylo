import results from '@/lib/compare-results.json';
import migrateResultsJson from '@/lib/migrate-compare-results.json';

/**
 * The docs' comparison, /docs/compare: which tools and rows it shows. Every mark comes from
 * lib/compare-results.json, which lab/compare writes from real runs of each tool.
 */
/** A tool's mark in a row; `pro` is a yes only in the tool's paid build, which `note` says. */
export type CompareCell = { state: 'y' | 'p' | 'n'; note?: string; pro?: boolean };

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

/** The ORMs and query builders, in the first table. */
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

/**
 * The migration tools beside Reylo Migrate, on /docs/migrate/compare and in its launch post, in an order
 * that keeps the longer subtitles from sitting side by side. Every mark
 * comes from lib/migrate-compare-results.json, written from real runs of each tool on Postgres 18,
 * MySQL 8.4 and SQLite, but Atlas's, which are Atlas Pro's documented behavior, since its free
 * builds leave out views, functions, triggers and policies.
 */
export const migrateTools = [
  { id: 'prisma', name: 'Prisma', sub: 'TypeScript', href: 'https://www.prisma.io/orm' },
  { id: 'atlas', name: 'Atlas Pro', sub: 'Go', href: 'https://atlasgo.io' },
  { id: 'drizzle', name: 'Drizzle Kit', sub: 'TypeScript', href: 'https://orm.drizzle.team/docs/kit-overview' },
  { id: 'flyway', name: 'Flyway', sub: 'Java', href: 'https://github.com/flyway/flyway' },
  { id: 'pgdelta', name: 'pg-delta', sub: 'Supabase', href: 'https://github.com/supabase/pg-toolbelt/tree/main/packages/pg-delta' },
  { id: 'liquibase', name: 'Liquibase', sub: 'Java', href: 'https://www.liquibase.com' },
];

/** The rows of the migration table, each with what it asks of a tool. */
export const migrateRows: { id: string; label: string; asks?: string }[] = [
  { id: 'sql-schema', label: 'Schema in SQL', asks: 'Your schema is plain SQL files, and a change to them becomes a migration.' },
  { id: 'check', label: 'Check constraints', asks: 'A new or changed check constraint becomes a migration.' },
  { id: 'expression-indexes', label: 'Expression indexes', asks: 'An index on an expression, such as `lower(email)`, becomes a migration.' },
  { id: 'partial-indexes', label: 'Partial indexes', asks: 'An index with a `where` clause becomes a migration.' },
  { id: 'generated-columns', label: 'Computed columns', asks: 'A generated column, or a change to its expression, becomes a migration.' },
  { id: 'domains', label: 'Domains', asks: 'A domain, and a column of it, becomes a migration.' },
  { id: 'views', label: 'Views', asks: "A new view, or a change to its query, becomes a migration." },
  { id: 'functions', label: 'Functions', asks: "A new function, or a change to its body, becomes a migration." },
  { id: 'triggers', label: 'Triggers', asks: 'A new trigger, or a change to when it fires, becomes a migration.' },
  { id: 'rls', label: 'Row level security', asks: "A new policy, or a change to its expression, becomes a migration." },
  { id: 'extensions', label: 'Extensions', asks: 'An extension your schema creates becomes a migration.' },
  { id: 'grants', label: 'Grants', asks: 'A new grant becomes a migration.' },
  { id: 'three-dialects', label: 'Postgres, MySQL, SQLite', asks: 'It writes migrations from your schema on all three.' },
  { id: 'renames', label: 'Renames', asks: 'A renamed column keeps its rows, rather than being dropped and added again.' },
  { id: 'data-loss', label: 'Data-loss guard', asks: 'Dropping a column that holds rows waits for you to confirm it.' },
  { id: 'view-dependencies', label: 'Dependent views', asks: 'Retyping a column that a view reads takes the view down and puts it back around the change.' },
  { id: 'non-blocking', label: 'Non-blocking indexes', asks: 'A new index on a Postgres table is built without blocking writes.' },
  { id: 'shadow', label: 'Shadow database', asks: 'Each migration is tried on a throwaway database before it runs.' },
  { id: 'drift', label: 'Drift detection', asks: 'A deploy onto a database someone changed by hand is refused.' },
  { id: 'edited', label: 'Edited migrations', asks: 'A deploy onto a database that ran a migration since edited is refused.' },
  { id: 'out-of-order', label: 'Out-of-order migrations', asks: 'A migration that sorts before one a database already ran is caught.' },
  { id: 'branch-switch', label: 'Branch switching', asks: "After a branch switch, the development database rewinds the other branch's migrations, keeping the rest." },
  { id: 'lockfile', label: 'Strict order in git', asks: 'Two branches that each add a migration conflict in git.' },
  { id: 'open-source', label: 'Free and open source', asks: 'Everything above is in a release under an open source license.' },
];

export const migrateResults = migrateResultsJson as unknown as typeof compareResults;

export type CompareSetName = 'orm' | 'migrate';

/** Which tools, rows and results a `<CompareTable />` shows: the SQL tools, `set="orm"` or `set="migrate"`, and what Reylo's column is called under its name. */
export const compareSet = (set?: CompareSetName) =>
  set === 'orm'
    ? { tools: compareOrms, rows: compareOrmRows, results: compareResults, sub: 'TypeScript', name: 'Reylo' }
    : set === 'migrate'
      ? { tools: migrateTools, rows: migrateRows, results: migrateResults, sub: 'Migrate', name: 'Reylo Migrate' }
      : { tools: compareTools, rows: compareRows, results: compareResults, sub: 'TypeScript', name: 'Reylo' };

/** A tool's mark on a row. A migration table's cell no run has measured yet is missing, and shown empty. */
export function compareCell(tool: string, row: string, set?: CompareSetName): CompareCell | undefined {
  const cell = compareSet(set).results.tools[tool]?.cells[row];
  return cell ?? (set === 'migrate' ? undefined : { state: 'n' });
}

/** The page's `<CompareTable />`s as Markdown, for /docs/compare.md, /docs/migrate/compare.md and /llms.txt. */
export function expandCompare(text: string): string {
  const word = { y: 'yes', p: 'partly', n: 'no' };
  const said = (cell?: CompareCell) => (cell ? word[cell.state] : '');
  return text.replace(/<CompareTable(?: set="(orm|migrate)")? \/>/g, (_, set?: CompareSetName) => {
    const { tools, rows, name } = compareSet(set);
    const head = `| | ${name} | ${tools.map((t) => `[${t.id === 'postgrestypedsql' ? 'Postgres Typed SQL' : t.name}](${t.href})`).join(' | ')} |`;
    const rule = `| --- | --- | ${tools.map(() => '---').join(' | ')} |`;
    const lines = rows.map((r) => `| ${r.label} | ${said(compareCell('reylo', r.id, set))} | ${tools.map((t) => said(compareCell(t.id, r.id, set))).join(' | ')} |`);
    return [head, rule, ...lines].join('\n');
  });
}
