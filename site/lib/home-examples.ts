import { readFileSync } from 'node:fs';
import path from 'node:path';
import { createHighlighter } from 'shiki';
import type { Plugin } from 'vite';
import { pageFiles } from './page-files.ts';
import { declarations } from './table-hovers.ts';
import { collect, collectFiles, DIALECT_NAME, type Dialect, type MdNode, INFERENCE_CARD, isStatement, moduleTypes, type Modules, parsePage, readModules, SCHEMA_FILE } from './remark-mapper.ts';
import { formatType, resultType } from '../../src/type-format.ts';

/*
 * What the homepage shows (components/home-landing.tsx), read from the docs and the types
 * captured for them (lib/sql-modules.json), never typed by hand: the Inference section's cards
 * (content/sections/03-inference.mdx), each h2's first by its id and a later one by its id and place, each with its statement and types in
 * every dialect it has; the main page's schema in each dialect; and the intro's `get_author.sql`
 * with the type its import gives. Highlighted here, once, as lines of [text, colour] tokens, so the
 * page ships highlighted text and no highlighter. `virtual:home-examples` is all three.
 */

/** A line of code: its tokens, each its text and its colour by day and by night, as the docs' code has them (Shiki's github-light and github-dark), and for a table's name, the table, whose declaration a hover shows. */
export type Line = [text: string, light: string, dark: string, table?: string][];
/** A statement's parameters and result, as the docs' inference cards show them: `params` is undefined for none, `returns` is `void` for no rows. `tables` holds the declaration of each table it names. */
export type HomeVariant = { file: string; sql: Line[]; tables: Record<string, Line[]>; params?: Line[]; returns: Line[] };
export type HomeExample = { id: string; title: string; lede: string; variants: Partial<Record<Dialect, HomeVariant>> };
export type HomeData = {
  examples: HomeExample[];
  intro: { sql: Line[]; tables: Record<string, Line[]>; ts: Line[]; type: Line[] };
  /** The intro's statement with `u.email` added to its select list, and the type then, `email` beside `name`: the first line of each as it was and as it is, then the type whole. */
  edit: { before: Line; after: Line; rest: Line[]; type: Line[]; added: number };
  /** The editor section's misspelled statement (content/sections/01-setup.mdx), and the error the content mapper gives it (lib/sql-errors.json). */
  error: { file: string; sql: Line[]; line: number; start: number; end: number; message: string; source: string };
  /** One main.ts per driver, from the Usage section's driver tabs (content/sections/02-usage.mdx). */
  drivers: { name: string; dialect: Dialect; ts: Line[] }[];
};

const VIRTUAL = 'virtual:home-examples';
const SECTION = 'content/sections/03-inference.mdx';
const INTRO = 'content/docs/index.mdx';
const SETUP = 'content/sections/01-setup.mdx';
const USAGE = 'content/sections/02-usage.mdx';
/** The drivers the homepage shows, by tab and dialect, in the order of the docs' feature card that names them. */
const DRIVERS: [name: string, dialect: Dialect][] = [
  ['pg', 'postgres'],
  ['postgres.js', 'postgres'],
  ['Bun.sql', 'postgres'],
  ['Neon', 'postgres'],
  ['mysql2', 'mysql'],
  ['PlanetScale', 'mysql'],
  ['better-sqlite3', 'sqlite'],
  ['node:sqlite', 'sqlite'],
  ['libSQL', 'sqlite'],
  ['Cloudflare D1', 'sqlite'],
  ['Drizzle', 'postgres'],
  ['Knex', 'postgres'],
];
/** How many characters of a type fit on a line before it breaks: the docs' cards' width (lib/remark-mapper.ts). */
const WIDTH = 64;
const THEMES = { light: 'github-light', dark: 'github-dark' };

type Token = { content: string; htmlStyle?: Record<string, string> };

/** A heading's or a paragraph's text, inline code in backticks; a heading's `[#id]` comes off. */
function textOf(node: MdNode): string {
  if (node.type === 'text') return String(node.value ?? '');
  if (node.type === 'inlineCode') return `\`${String(node.value ?? '')}\``;
  return (node.children ?? []).map(textOf).join('');
}

const slug = (s: string) => s.toLowerCase().replace(/`/g, '').replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');

/** A paragraph's first sentence, without the colon that hands off to its example. */
const firstSentence = (text: string) => (/^(.*?[.:])(\s|$)/.exec(text)?.[1] ?? text).replace(/:$/, '.');

/** Where a statement names one of the tables, as [start, end, table]: an identifier outside strings and comments, not after a dot. */
function tableNames(sql: string, tables: Map<string, string>): [number, number, string][] {
  const out: [number, number, string][] = [];
  for (let i = 0; i < sql.length; i++) {
    const c = sql[i];
    if (c === "'") i = sql.indexOf("'", i + 1) < 0 ? sql.length : sql.indexOf("'", i + 1);
    else if (c === '-' && sql[i + 1] === '-') i = sql.indexOf('\n', i) < 0 ? sql.length : sql.indexOf('\n', i);
    else if (/[A-Za-z_]/.test(c)) {
      let end = i;
      while (end < sql.length && /\w/.test(sql[end])) end++;
      const word = sql.slice(i, end).toLowerCase();
      if (tables.has(word) && !/\.\s*$/.test(sql.slice(0, i)) && sql[i - 1] !== ':') out.push([i, end, word]);
      i = end - 1;
    }
  }
  return out;
}

/** Highlighted lines with each table's name a token of its own, marked with the table. */
function markTables(code: string, highlighted: Line[], tables: Map<string, string>): Line[] {
  const spans = tableNames(code, tables);
  if (!spans.length) return highlighted;
  let offset = 0;
  return highlighted.map((line) => {
    const out: Line = [];
    for (const [text, light, dark] of line) {
      const start = offset;
      offset += text.length;
      let at = start;
      for (const [s, e, table] of spans) {
        if (e <= at || s >= offset) continue;
        if (s > at) out.push([text.slice(at - start, s - start), light, dark]);
        out.push([text.slice(Math.max(s, at) - start, Math.min(e, offset) - start), light, dark, table]);
        at = Math.min(e, offset);
      }
      if (at < offset) out.push([text.slice(at - start), light, dark]);
    }
    offset += 1;
    return out;
  });
}

export async function buildHomeData(site = process.cwd(), modules: Modules = readModules(site)): Promise<HomeData> {
  const highlighter = await createHighlighter({ themes: Object.values(THEMES), langs: ['sql', 'ts'] });
  const lines = (code: string, lang: 'sql' | 'ts'): Line[] =>
    (highlighter.codeToTokens(code.replace(/\n$/, ''), { lang, themes: THEMES, defaultColor: false }).tokens as Token[][]).map((line) =>
      line.map((t): Line[number] => [t.content, t.htmlStyle?.['--shiki-light'] ?? '#24292e', t.htmlStyle?.['--shiki-dark'] ?? '#e1e4e8']),
    );

  // The Inference section's cards, by the h2 each sits under, and the paragraph before it.
  const tree = parsePage(path.join(site, SECTION));
  const cards: { card: MdNode; title: string; id: string; lede: string }[] = [];
  let title = '';
  let id = '';
  let lede = '';
  const walk = (node: MdNode) => {
    for (const child of node.children ?? []) {
      if (child.type === 'heading' && (child as MdNode & { depth?: number }).depth === 2) {
        const text = textOf(child);
        title = text.replace(/\s*\[#[\w-]+\]\s*$/, '');
        id = /\[#([\w-]+)\]\s*$/.exec(text)?.[1] ?? slug(title);
        lede = '';
      } else if (child.type === 'paragraph' && node.type === 'root') {
        lede = firstSentence(textOf(child));
      } else if (child.type === 'mdxJsxFlowElement' && child.name === INFERENCE_CARD) {
        // A section's first card is its id; a later one, its id and its place, as `subqueries:2`.
        const n = cards.filter((c) => c.id === id || c.id.startsWith(`${id}:`)).length;
        cards.push({ card: child, title, id: n ? `${id}:${n + 1}` : id, lede });
      } else walk(child);
    }
  };
  walk(tree);
  const blocks = collect(tree);

  // The schema every card without its own is analyzed against: the main page's, per dialect.
  const page = collectFiles(pageFiles(site), site);
  const pageTables: Partial<Record<Dialect, Map<string, string>>> = {};
  for (const dialect of Object.keys(DIALECT_NAME) as Dialect[]) {
    const parts = page.filter((b) => b.name === SCHEMA_FILE && b.dialect === dialect).map((b) => b.node.value.replace(/\n$/, ''));
    if (parts.length) pageTables[dialect] = declarations(parts.join('\n\n'));
  }
  /** A statement highlighted, its tables marked, and the declarations of those it names. */
  const statement = (sql: string, tables: Map<string, string>) => {
    const marked = markTables(sql.replace(/\n$/, ''), lines(sql, 'sql'), tables);
    const named = new Set(marked.flat().flatMap((t) => (t[3] ? [t[3]] : [])));
    return { sql: marked, tables: Object.fromEntries([...named].map((t) => [t, lines(tables.get(t)!, 'sql')])) };
  };

  const examples: HomeExample[] = [];
  for (const { card, title, id, lede } of cards) {
    const statements = blocks.filter((b) => b.parent === card && isStatement(b));
    const variants: HomeExample['variants'] = {};
    for (const dialect of Object.keys(DIALECT_NAME) as Dialect[]) {
      const block = statements.find((b) => b.dialect === dialect);
      const module = block && modules[dialect]?.[block.name!]?.[block.hash];
      if (!block || module === undefined) continue;
      const { params, row, cardinality } = moduleTypes(module);
      const types = (width: number) => ({
        ...(params && { params: lines(formatType(params, width), 'ts') }),
        returns: lines(formatType(resultType(row, cardinality), width), 'ts'),
      });
      const tables = block.schema !== undefined ? declarations(block.schema) : (pageTables[dialect] ?? new Map<string, string>());
      variants[dialect] = {
        file: block.name!.replace(/^inferred_/, ''),
        ...statement(block.node.value, tables),
        ...types(WIDTH),
      };
    }
    if (Object.keys(variants).length) examples.push({ id, title, lede, variants });
  }

  // The intro's statement, and the type of what its import's `.run()` resolves to.
  const intro = collect(parsePage(path.join(site, INTRO))).find((b) => b.name === 'get_author.sql' && b.dialect === 'postgres');
  const introModule = intro && modules.postgres?.['get_author.sql']?.[intro.hash];
  if (!intro || introModule === undefined) throw new Error(`lib/home-examples.ts: no captured module for get_author.sql in ${INTRO}; run \`nub run capture\` in site/.`);
  const { row, cardinality } = moduleTypes(introModule);
  // The intro's main.ts, without its Twoslash directives and the completion it opens.
  const main = collect(parsePage(path.join(site, INTRO))).find((b) => b.name === 'main.ts')?.node.value;
  if (main === undefined) throw new Error(`lib/home-examples.ts: no main.ts block in ${INTRO}.`);
  const ts = main.split('\n').filter((l) => !l.startsWith('// @')).join('\n').replace(/\n+author\?\.[\s\S]*$/, '');
  // The edit an agent makes: `u.email` added to the select list, and the type it then gives, from
  // the schema's own note on the column (`email text unique -- email: string | null`).
  const introSql = intro.node.value.replace(/^(?:--[^\n]*\n)+/, '').replace(/\n$/, '');
  const [first, ...rest] = introSql.split('\n');
  const edited = first.replace('u.name,', 'u.name, u.email,');
  if (edited === first) throw new Error(`lib/home-examples.ts: the intro's get_author.sql no longer selects u.name; update the homepage's edit.`);
  const typeText = `const author: ${formatType(resultType(row, cardinality), 44)}`;
  const typeLines = typeText.split('\n');
  const nameAt = typeLines.findIndex((l) => /^\s+name: string;$/.test(l));
  if (nameAt < 0) throw new Error(`lib/home-examples.ts: the intro's type has no \`name: string\`; update the homepage's edit.`);
  typeLines.splice(nameAt + 1, 0, typeLines[nameAt].replace('name: string', 'email: string | null'));
  const edit = { before: lines(first, 'sql')[0]!, after: lines(edited, 'sql')[0]!, rest: lines(rest.join('\n'), 'sql'), type: lines(typeLines.join('\n'), 'ts'), added: nameAt + 1 };

  // The editor section's misspelled statement, and its captured error.
  const errorBlock = collect(parsePage(path.join(site, SETUP))).find((b) => b.name === 'get_user.sql' && b.dialect === 'postgres' && /\berrors\b/.test(b.node.meta ?? ''));
  const errors = JSON.parse(readFileSync(path.join(site, 'lib/sql-errors.json'), 'utf8')) as Record<string, Record<string, Record<string, { start: { line: number; column: number }; end: { line: number; column: number }; message: string; source: string }[]>>>;
  const captured = errorBlock && errors.postgres?.['get_user.sql']?.[errorBlock.hash]?.[0];
  if (!errorBlock || !captured) throw new Error(`lib/home-examples.ts: no captured error for get_user.sql in ${SETUP}; run \`nub run capture\` in site/.`);
  const error = { file: 'get_user.sql', sql: lines(errorBlock.node.value, 'sql'), line: captured.start.line - 1, start: captured.start.column, end: captured.end.column, message: captured.message, source: captured.source };

  // Each driver's main.ts, without Twoslash's lines.
  const usage = collect(parsePage(path.join(site, USAGE)));
  const drivers = DRIVERS.map(([name, dialect]) => {
    const block = usage.find((b) => b.name === name && b.dialect === dialect);
    if (!block) throw new Error(`lib/home-examples.ts: no ${name} tab for ${dialect} in ${USAGE}.`);
    const code = block.node.value.split('\n').filter((l) => !/^\/\/ (@|---cut---)|^\/\/\/ <reference/.test(l)).join('\n');
    return { name, dialect, ts: lines(code, 'ts') };
  });

  const data: HomeData = {
    examples,
    edit,
    error,
    drivers,
    // The hero shows the statement without the docs' leading comment, which points at the hovers.
    intro: { ...statement(intro.node.value.replace(/^(?:--[^\n]*\n)+/, ''), pageTables.postgres ?? new Map()), ts: lines(ts, 'ts'), type: lines(`const author: ${formatType(resultType(row, cardinality), 44)}`, 'ts') },
  };
  highlighter.dispose();
  return data;
}

/** The Vite plugin behind `virtual:home-examples`. */
export function homeExamples(): Plugin {
  return {
    name: 'reylo-home-examples',
    resolveId: (id) => (id === VIRTUAL ? `\0${VIRTUAL}` : undefined),
    async load(id) {
      if (id !== `\0${VIRTUAL}`) return;
      const site = process.cwd();
      for (const file of [SECTION, INTRO, SETUP, USAGE, 'lib/sql-modules.json', 'lib/sql-errors.json']) this.addWatchFile(path.join(site, file));
      return `export default ${JSON.stringify(await buildHomeData(site))};`;
    },
  };
}
