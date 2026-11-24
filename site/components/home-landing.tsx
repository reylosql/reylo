import { createContext, useContext, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import data from 'virtual:home-examples';
import { DATABASES, HOSTS } from './host-logos';
import { Foot, Install, Nav } from './home-chrome';
import { CompareTable } from './compare-table';
import { HowItWorksDiagram } from './how-it-works-diagram';
import { FileIcon, MigrateRows, Row } from './migrate-landing';
import type { HomeExample, HomeVariant, Line } from '@/lib/home-examples';
import './home-landing.css';

/*
 * The homepage, at / (app/routes/home.tsx), for Reylo and Reylo Migrate together, in the Migrate
 * landing's language (components/migrate-landing.tsx, the `.ml` styles): a nav whose GitHub pill
 * carries a handwritten "Leave a star!"; a hero split in half, the words on the left and on the right
 * a query over the main.ts that imports it; the databases and hosts both work with; then one part per
 * tool, each opening on a centred head with wide room around it, a chip naming what the tool is over
 * its own headline, and going on in rows whose sides alternate. Reylo's head holds the Inference
 * section's examples, each in the docs' inference card (components/inference-card.tsx), each table's
 * name showing its declaration on hover; its rows are its features, each beside what it does. Reylo
 * Migrate's rows are its landing's, then its comparison. Every statement, type and error is the
 * docs' own, captured (lib/home-examples.ts), but for the agent row's edit (see there). Nothing reads
 * the scroll position.
 */

type Dialect = 'postgres' | 'mysql' | 'sqlite';

/** The examples the homepage shows, by the Inference section's anchors, in its order. Named parameters are left out: `:name` is Reylo's addition to SQL, not what Reylo infers. */
const SHOWN = [
  'left-joins',
  'left-joins:2',
  'computed-columns',
  'nullable-parameters',
  'filtering-by-a-list',
  'unique-keys',
  'aggregates',
  'aggregates:2',
  'window-functions',
  'returning-inserts',
  'returning-inserts:2',
  'returning-inserts:4',
  'unions',
  'json',
  'json-schema',
  // The longest queries last, so that clicking through in order meets the short ones first.
  'oneof',
  'subqueries:2',
  'bulk-inserts',
  'nested-fetching',
  'typed-json:2',
];
/** A tab's own name and caption where the card is a later one in its section, whose heading and lede lean on the card before it. */
const OWN: Record<string, { title?: string; lede?: string }> = {
  'left-joins:2': { title: 'Narrowing in where' },
  'aggregates:2': { title: 'Rollups' },
  'returning-inserts:2': { title: 'Upserts', lede: 'An upsert returns one row, whether it inserts the row or updates it.' },
  'returning-inserts:4': { title: 'Old and new' },
};
const EXAMPLES = SHOWN.map((id) => data.examples.find((e) => e.id === id))
  .filter((e): e is HomeExample => !!e)
  .map((e) => ({ ...e, ...OWN[e.id] }));


const DIALECT_NAMES: Record<Dialect, string> = { postgres: 'Postgres', mysql: 'MySQL', sqlite: 'SQLite' };

export function HomeLanding({ stars, agentPrompt }: { stars?: number; agentPrompt?: string }) {
  return (
    <TableHover>
      <div className="ml rh rh-home">
        <Nav stars={stars} />
        <Hero agentPrompt={agentPrompt} />
        <Logos />
        <section id="reylo" className="rh-part">
          <PartHead chip="Inference engine" title="Import SQL into TypeScript, fully typed">
            Reylo infers type information from SQL queries, against a live database.
          </PartHead>
          <Examples />
          <ReyloRows />
        </section>
        <section id="migrate" className="rh-part">
          <PartHead chip="Migration engine" title="SQL-first schema and migrations">
            Model your schema in <code>schema.sql</code>. Generate migrations, or write your own. Works on Postgres, MySQL and SQLite.
          </PartHead>
          <MigrateRows hosts={false} />
          <section className="ml-table">
            <h2>Compared with other migration tools</h2>
            <p>Hover a cross to see why.</p>
            <CompareTable set="migrate" />
          </section>
        </section>
        <span className="rh-blade" aria-hidden="true" />
        <End />
        <Foot />
      </div>
    </TableHover>
  );
}

/* ── The nav ─────────────────────────────────────────────────────────────── */

/* ── The install ────────────────────────────────────────────────────────── */

/* ── Code ───────────────────────────────────────────────────────────────── */

/**
 * Highlighted lines, as lib/home-examples.ts tokenized them, each token in its colour by day and by
 * night (`.rh-tok` picks one; a window, dark on either theme, takes the night's). A table's name,
 * where `tables` holds its declaration, is underlined, and shows the declaration on hover or focus.
 */
function Code({ lines, tables }: { lines: Line[]; tables?: Record<string, Line[]> }) {
  const hover = useContext(HoverContext);
  return lines.map((line, i) => (
    <span key={i} className="rh-line">
      {line.map(([text, light, dark, table], j) => {
        const style = { '--l': light, '--d': dark } as CSSProperties;
        const declaration = table ? tables?.[table] : undefined;
        if (!declaration) {
          return (
            <span key={j} className="rh-tok" style={style}>
              {text}
            </span>
          );
        }
        return (
          <span
            key={j}
            className="rh-tok rh-ref"
            style={style}
            tabIndex={0}
            aria-label={`${text}: show its declaration`}
            onMouseEnter={(e) => hover.show(e.currentTarget, declaration)}
            onMouseLeave={hover.hide}
            onFocus={(e) => hover.show(e.currentTarget, declaration)}
            onBlur={hover.hide}
          >
            {text}
          </span>
        );
      })}
      {'\n'}
    </span>
  ));
}

/* ── A table's declaration, on hover ────────────────────────────────────── */

const HoverContext = createContext<{ show: (at: HTMLElement, lines: Line[]) => void; hide: () => void }>({ show: () => {}, hide: () => {} });

/**
 * One popover for the page, as Twoslash shows a type: under the table's name, holding its `create
 * table`. It sits on the page's body, since the card clips what overflows it; it stays while the
 * pointer moves from the name into it, so a long declaration can be scrolled.
 */
function TableHover({ children }: { children: ReactNode }) {
  const [shown, setShown] = useState<{ lines: Line[]; left: number; top: number } | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const keep = () => clearTimeout(timer.current);
  const hide = () => {
    keep();
    timer.current = setTimeout(() => setShown(null), 120);
  };
  const show = (at: HTMLElement, lines: Line[]) => {
    keep();
    const box = at.getBoundingClientRect();
    // As wide as the popover may be (its max-width), kept a gutter in from the window's right edge.
    const width = Math.min(576, window.innerWidth - 32);
    setShown({ lines, left: Math.max(16, Math.min(box.left - 14, window.innerWidth - width - 16)) + window.scrollX, top: box.bottom + 6 + window.scrollY });
  };
  return (
    <HoverContext.Provider value={{ show, hide }}>
      {children}
      {shown &&
        createPortal(
          <div className="rh rh-decl" role="tooltip" style={{ left: shown.left, top: shown.top }} onMouseEnter={keep} onMouseLeave={hide}>
            <pre className="rh-code">
              <Code lines={shown.lines} />
            </pre>
          </div>,
          document.body,
        )}
    </HoverContext.Provider>
  );
}

/* ── The inference card ─────────────────────────────────────────────────── */

/**
 * The docs' inference card (components/inference-card.tsx, `.reylo-inference-card` in
 * app/global.css), whose styles are scoped to the docs' page: Query, with the database switch at the
 * right of its label's line where the example has more than one database, then Params and Returns in
 * a band under it.
 */
function Card({ variant, dialects, dialect, onDialect, tabs }: { variant: HomeVariant; dialects: Dialect[]; dialect: Dialect; onDialect: (d: Dialect) => void; tabs?: ReactNode }) {
  return (
    <div className="rh-card">
      {tabs}
      <div className="rh-card-row">
        <div className="rh-card-label">Query</div>
        {dialects.length > 1 && (
          <div className="rh-switch" role="tablist" aria-label="Database">
            {dialects.map((d) => (
              <button key={d} type="button" role="tab" aria-selected={d === dialect} data-state={d === dialect ? 'active' : undefined} onClick={() => onDialect(d)}>
                {DIALECT_NAMES[d]}
              </button>
            ))}
          </div>
        )}
        <pre className="rh-code">
          <Code lines={variant.sql} tables={variant.tables} />
        </pre>
      </div>
      <div className="rh-card-types">
        <div className="rh-card-row">
          <div className="rh-card-label">Params</div>
          <pre className="rh-code">
            <Code lines={variant.params ?? [[['void', '#D73A49', '#F97583']]]} />
          </pre>
        </div>
        <div className="rh-card-row">
          <div className="rh-card-label">Returns</div>
          <pre className="rh-code">
            <Code lines={variant.returns} />
          </pre>
        </div>
      </div>
    </div>
  );
}

/* ── The examples ───────────────────────────────────────────────────────── */

/** A line of prose with its inline code, as the docs write it in backticks. */
function Prose({ text }: { text: string }) {
  return text.split(/(`[^`]+`)/).map((part, i) => (part.startsWith('`') ? <code key={i}>{part.slice(1, -1)}</code> : part));
}

/**
 * The Inference section's examples, under Reylo's part's head, in one inference card: their names as tabs across its top, the
 * rows balanced so no name sits alone on one, and the picked example's lede as a caption under it.
 * The card is as tall as its example needs. The reader picks one; none turns by itself.
 */
function Examples() {
  const [at, setAt] = useState(0);
  const [dialect, setDialect] = useState<Dialect>('postgres');
  const example = EXAMPLES[at]!;
  const shown: Dialect = example.variants[dialect] ? dialect : (Object.keys(example.variants)[0] as Dialect);
  const tabs = (
    <div className="rh-tabs" role="tablist" aria-label="Examples">
      {EXAMPLES.map((e, i) => (
        <button key={e.id} type="button" role="tab" aria-selected={i === at} className={i === at ? 'on' : undefined} onClick={() => setAt(i)}>
          {e.title}
        </button>
      ))}
    </div>
  );
  return (
    <section className="rh-examples">
      <div className="rh-ex">
        <Card variant={example.variants[shown]!} dialects={Object.keys(example.variants) as Dialect[]} dialect={shown} onDialect={setDialect} tabs={tabs} />
        <p className="rh-ex-caption">
          <Prose text={example.lede} />
        </p>
      </div>
    </section>
  );
}

/* ── The hero ───────────────────────────────────────────────────────────── */

/** The blog post (content/blog) the pill above the headline links: the latest launch, which a patch release never replaces. */
const NEWS = { href: '/blog/reylo-migrate', title: 'Introducing Reylo Migrate — SQL-first schema and migrations' };

/** Above the headline, a pill to the latest launch's post: a "New" badge, its title, which a narrow window ellipsizes, and an arrow. */
function NewsPill() {
  return (
    <a className="rh-news" href={NEWS.href}>
      <span className="rh-news-badge">New</span>
      <span className="rh-news-title">{NEWS.title}</span>
      <span className="rh-news-arrow" aria-hidden="true">
        →
      </span>
    </a>
  );
}

/**
 * The words on the left; on the right the intro's statement and, under it, indented, the main.ts
 * that imports it, with the type of `author` in a hover, as the Migrate landing lays its editor over
 * its terminal.
 */
function Hero({ agentPrompt }: { agentPrompt?: string }) {
  return (
    <header className="ml-hero rh-hero-split">
      <div className="ml-hero-words">
        <NewsPill />
        <h1 className="ml-h1 rh-h1 rh-h1-both">
          <span>Type-safe SQL tooling</span> <em>for the modern era</em>
        </h1>
        <p className="ml-lede">Typed queries, schema modeling and migrations, all in plain SQL.</p>
        <Install agentPrompt={agentPrompt} />
      </div>
      <div className="ml-hero-visual rh-import">
        <div className="ml-pair">
          <div className="ml-win ml-editor">
            <div className="ml-bar">
              <FileIcon />
              get_author.sql
            </div>
            <pre className="rh-code">
              <Code lines={data.intro.sql} tables={data.intro.tables} />
            </pre>
          </div>
          <div className="ml-win ml-under rh-import-ts" aria-hidden="true">
            <div className="ml-bar">
              <FileIcon />
              main.ts
            </div>
            <pre className="rh-code">
              <Code lines={data.intro.ts} />
            </pre>
            <div className="rh-hover">
              <pre className="rh-code">
                <Code lines={data.intro.type} />
              </pre>
            </div>
          </div>
        </div>
      </div>
    </header>
  );
}

/**
 * The head of a tool's part of the page, centred, with room above and below it to mark a new part:
 * a chip naming what the tool is, its headline and a sentence.
 */
function PartHead({ chip, title, children }: { chip: string; title: string; children: ReactNode }) {
  return (
    <header className="rh-part-head">
      <div className="rh-section-head">
        <span className="rh-chip">{chip}</span>
        <h2>{title}</h2>
        <p>{children}</p>
      </div>
    </header>
  );
}

/* ── Reylo's rows ───────────────────────────────────────────────────────── */

/** One run of tokens, as `Code` draws a line, without the line's break. */
function Tokens({ line }: { line: Line }) {
  return line.map(([text, light, dark], j) => (
    <span key={j} className="rh-tok" style={{ '--l': light, '--d': dark } as CSSProperties}>
      {text}
    </span>
  ));
}

/** Reylo's features, each a row beside what it does, the sides alternating as Reylo Migrate's do. */
function ReyloRows() {
  const rows: { title: string; visual: ReactNode; body: ReactNode }[] = [
    {
      title: 'SQL-first',
      visual: <SqlFirst />,
      body: <p>No lossy DSL and no CRUD-limited ORM. Reylo gives you (and your agents) back the freedom to write complex queries without sacrificing DRY.</p>,
    },
    {
      title: 'No hand-written types',
      visual: <HowItWorksDiagram stacked />,
      body: <p>Parameters, columns, nullability and cardinality come from the database's own analysis of each query.</p>,
    },
    {
      title: 'Checked against your database',
      visual: <ErrorWindow />,
      body: (
        <p>
          A misspelled column is a type error at its line and column, and so is a wrong argument to <code>.run()</code>.
        </p>
      ),
    },
    {
      title: 'Agent-friendly',
      visual: <AgentEdit />,
      body: <p>A query and its types are one file. An agent edits the SQL, and the types follow, with nothing else to update.</p>,
    },
    {
      title: 'Postgres, MySQL and SQLite',
      visual: <Dialects />,
      body: <p>Each in its own dialect, on Neon, Supabase, PlanetScale, Cloudflare D1, Turso and more.</p>,
    },
    {
      title: 'Your own driver',
      visual: <Drivers />,
      body: (
        <p>
          Runs on <code>pg</code>, postgres.js, Bun.sql, Neon, <code>mysql2</code>, <code>mariadb</code>, PlanetScale, <code>better-sqlite3</code>, <code>node:sqlite</code>, libSQL, Cloudflare D1 and more, or through Drizzle and Knex.
        </p>
      ),
    },
  ];
  return rows.map((r, i) => (
    <Row key={r.title} n={i + 1} title={r.title} visual={r.visual} flip={i % 2 === 1}>
      {r.body}
    </Row>
  ));
}

/** The Subqueries section's lateral join, a query an ORM can't write, over the type it returns. */
function SqlFirst() {
  const variant = EXAMPLES.find((e) => e.id === 'subqueries:2')?.variants.postgres;
  if (!variant) return null;
  return (
    <div className="ml-pair rh-import rh-sqlfirst" aria-hidden="true">
      <div className="ml-win ml-editor">
        <div className="ml-bar">
          <FileIcon />
          {variant.file}
        </div>
        <pre className="rh-code">
          <Code lines={variant.sql} tables={variant.tables} />
        </pre>
      </div>
      <div className="ml-win ml-under rh-returns">
        <div className="ml-bar">Returns</div>
        <pre className="rh-code">
          <Code lines={variant.returns} />
        </pre>
      </div>
    </div>
  );
}

/** The editor section's misspelled statement, the misspelling underlined, and the error an editor shows under it. */
function ErrorWindow() {
  const { file, sql, line, start, end, message, source } = data.error;
  return (
    <div className="ml-win rh-error" aria-hidden="true">
      <div className="ml-bar">
        <FileIcon />
        {file}
      </div>
      <pre className="rh-code">
        {sql.map((tokens, i) => {
          if (i !== line) {
            return (
              <span key={i} className="rh-line">
                <Tokens line={tokens} />
                {'\n'}
              </span>
            );
          }
          // The line split at the error's columns (1-based, the end exclusive), the middle underlined.
          const text = tokens.map((t) => t[0]).join('');
          const cut = (from: number, to: number) => {
            const out: Line = [];
            let at = 0;
            for (const t of tokens) {
              const s = Math.max(from, at);
              const e = Math.min(to, at + t[0].length);
              if (s < e) out.push([t[0].slice(s - at, e - at), t[1], t[2]]);
              at += t[0].length;
            }
            return out;
          };
          return (
            <span key={i} className="rh-line">
              <Tokens line={cut(0, start - 1)} />
              <span className="rh-squiggle">
                <Tokens line={cut(start - 1, end - 1)} />
              </span>
              <Tokens line={cut(end - 1, text.length)} />
              {'\n'}
            </span>
          );
        })}
      </pre>
      <div className="rh-diag" style={{ '--rh-at': line, '--rh-col': start - 1 } as CSSProperties}>
        <span>{message}</span> <span className="rh-diag-src">{source}</span>
      </div>
    </div>
  );
}

/**
 * An agent's edit: `u.email` added to get_author.sql's select list, and under it the main.ts that
 * imports it, the type of `author` now holding `email`. The type is the intro's captured one with
 * `email` written in from the schema's note on its column (lib/home-examples.ts).
 */
function AgentEdit() {
  const { before, after, rest, type, added } = data.edit;
  return (
    <div className="ml-pair rh-import rh-agent">
      <div className="ml-win ml-editor">
        <div className="ml-bar">
          <FileIcon />
          get_author.sql
        </div>
        <pre className="ml-code">
          <span className="ml-l ml-del" data-g="-">
            <Tokens line={before} />
          </span>
          <span className="ml-l ml-add" data-g="+">
            <Tokens line={after} />
          </span>
          {rest.map((l, i) => (
            <span key={i} className="ml-l" data-g=" ">
              <Tokens line={l} />
            </span>
          ))}
        </pre>
      </div>
      <div className="ml-win ml-under rh-import-ts" aria-hidden="true">
        <div className="ml-bar">
          <FileIcon />
          main.ts
        </div>
        <pre className="rh-code">
          <Code lines={data.intro.ts} />
        </pre>
        <div className="rh-hover">
          <pre className="rh-code">
            {type.map((l, i) => (
              <span key={i} className={i === added ? 'rh-tline rh-added' : 'rh-tline'}>
                <Tokens line={l} />
              </span>
            ))}
          </pre>
        </div>
      </div>
    </div>
  );
}

/** One example in the three dialects, behind the docs' database switch: JSON aggregation, which each writes its own way. */
function Dialects() {
  const example = EXAMPLES.find((e) => e.id === 'json' && Object.keys(e.variants).length === 3) ?? EXAMPLES.find((e) => Object.keys(e.variants).length === 3)!;
  const [dialect, setDialect] = useState<Dialect>('postgres');
  return <Card variant={example.variants[dialect]!} dialects={Object.keys(example.variants) as Dialect[]} dialect={dialect} onDialect={setDialect} />;
}

/** The Usage section's main.ts for each driver, as tabs over an editor; none turns by itself. */
function Drivers() {
  const [at, setAt] = useState(0);
  return (
    <div className="ml-win ml-kinds rh-drivers">
      <div className="ml-bar">
        <FileIcon />
        main.ts
      </div>
      <div className="ml-tabs" role="tablist">
        {data.drivers.map((d, i) => (
          <button key={`${d.name}:${d.dialect}`} type="button" role="tab" aria-selected={i === at} className={i === at ? 'on' : undefined} onClick={() => setAt(i)}>
            {d.name}
          </button>
        ))}
      </div>
      <div className="ml-stack">
        {data.drivers.map((d, i) => (
          <pre key={`${d.name}:${d.dialect}`} className="ml-sql rh-code" data-on={i === at || undefined}>
            <Code lines={d.ts} />
          </pre>
        ))}
      </div>
    </div>
  );
}

/* ── The parts between sections, the call to action and the foot ────────── */

/** The databases and hosts in two rows, as even as their count allows, each centred. */
const LOGOS = [...DATABASES, ...HOSTS];
const LOGO_ROWS = [LOGOS.slice(0, Math.ceil(LOGOS.length / 2)), LOGOS.slice(Math.ceil(LOGOS.length / 2))];

/** Under the hero, the databases and hosts Reylo works with, under a small title that says so. */
function Logos() {
  return (
    <section className="rh-logos" aria-labelledby="rh-works-with">
      <h2 id="rh-works-with" className="rh-logos-title">
        Works with
      </h2>
      {LOGO_ROWS.map((row, i) => (
        <div key={i} className="rh-logos-row">
          {row.map((t) => (
            <span key={t.name} title={t.name}>
              <svg viewBox={t.viewBox} fill="currentColor" aria-hidden="true">
                {t.paths.map((d) => (
                  <path key={d} d={d} />
                ))}
              </svg>
              {t.name.replace(/ and Aurora$/, '')}
            </span>
          ))}
        </div>
      ))}
    </section>
  );
}

function End() {
  return (
    <section className="ml-end rh-end">
      <h2>Ship faster with Reylo</h2>
      <Install centered />
    </section>
  );
}

