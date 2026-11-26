import { SHOW_RUST } from './languages.ts';

export const githubUrl = 'https://github.com/reylosql/reylo';

/** Reylo Migrate's docs: content/docs/migrate.mdx, under /docs alongside the rest of the docs. */
export const migrateDocsUrl = '/docs/migrate';

/** The docs' intro, content/docs/index.mdx, at /docs under the homepage, / (components/home-landing.tsx). */
export const introUrl = '/docs';

/** Reylo Migrate's comparison with other migration tools: content/docs/migrate-compare.mdx, beside its docs. */
export const migrateCompareUrl = '/docs/migrate/compare';

/** A page's address: /docs for the intro, /docs/migrate/<name> for Reylo Migrate's sub-pages, and /docs/<name> for every other page. */
export const docsUrl = (url: string) => (url === '/' ? introUrl : url.startsWith('/migrate-') ? `/docs/migrate/${url.slice('/migrate-'.length)}` : `/docs${url}`);

/** The docs site's address: reylo.dev, which every other Reylo domain redirects to (wiki/docs-site.md). */
export const siteUrl = 'https://reylo.dev';

/**
 * The address of a page's Markdown, by its slug: /docs.md for the intro, /docs/<slug>.md for each
 * other page, and /docs/migrate/<name>.md for Reylo Migrate's sub-pages (lib/markdown.ts writes them).
 * Here, not there, since the page's `meta`, which links it, runs in the browser too, and lib/markdown.ts reads the disk.
 */
export const markdownPath = (slug: string) => `${docsUrl(slug ? `/${slug}` : '/')}.md`;

/**
 * The title search results and link previews show: the name and what it does, in under 60
 * characters so that Google does not cut it. The page's h1 stays the name alone.
 */
export const siteTitle = 'Reylo — Import .sql files into TypeScript, fully typed';

/**
 * A dialect's reference page (content/docs/<dialect>.mdx) in search results and link previews:
 * the name, the database and what the page holds, under 60 characters. The page's h1 stays the
 * database's name.
 */
export const pageTitle = (database: string) => `Reylo on ${database}: types, nulls, row counts, drivers`;

/** The comparison's title in search results and link previews. */
export const compareTitle = 'Reylo compared with Prisma, Drizzle, sqlc and others';

/** A page's title in search results and link previews, where the title is not a database's. */
export const docsTitle = (title: string) => `${title} — Reylo`;

/**
 * The list of pages in the left column, by group, and the links away from the docs at its foot:
 * first what holds for every language, then each language's own pages, then the reference. The Rust
 * page only while the docs show Rust (lib/languages.ts).
 */
export const pageGroups = [
  {
    label: 'Reylo',
    items: [
      { title: 'Intro', href: introUrl },
      { title: SHOW_RUST ? 'Usage with TypeScript' : 'Usage', href: '/docs/typescript' },
      ...(SHOW_RUST ? [{ title: 'Usage with Rust', href: '/docs/rust' }] : []),
      { title: 'Inference', href: '/docs/inference' },
      { title: 'Comparison', href: '/docs/compare' },
      { title: 'FAQ', href: '/docs/faq' },
    ],
  },
  {
    label: 'Reylo Migrate',
    items: [
      { title: 'Intro', href: migrateDocsUrl },
      { title: 'Usage', href: '/docs/migrate/usage' },
      { title: 'CLI', href: '/docs/migrate/cli' },
      { title: 'Comparison', href: migrateCompareUrl },
    ],
  },
  {
    label: 'Deployment',
    items: [
      // The biggest hosts first, then the rest by how many projects use them.
      { title: 'Neon', href: '/docs/neon' },
      { title: 'Supabase', href: '/docs/supabase' },
      { title: 'Durable Objects', href: '/docs/durable-objects' },
      { title: 'PlanetScale', href: '/docs/planetscale' },
      { title: 'Cloudflare D1', href: '/docs/d1' },
      { title: 'Turso', href: '/docs/turso' },
      { title: 'Cloudflare Hyperdrive', href: '/docs/hyperdrive' },
      { title: 'Vercel', href: '/docs/vercel' },
      { title: 'Render', href: '/docs/render' },
      { title: 'Kubernetes', href: '/docs/kubernetes' },
      { title: 'Railway', href: '/docs/railway' },
      { title: 'Fly.io', href: '/docs/fly' },
      { title: 'Next.js', href: '/docs/nextjs' },
      { title: 'CI', href: '/docs/deployment' },
    ],
  },
  {
    label: 'Reference',
    items: [
      { title: '.reylo.yaml', href: '/docs/configuration', code: true },
      { title: 'CLI', href: '/docs/cli' },
      { title: 'Postgres', href: '/docs/postgres' },
      { title: 'MySQL', href: '/docs/mysql' },
      { title: 'SQLite', href: '/docs/sqlite' },
      { title: 'ClickHouse', href: '/docs/clickhouse' },
    ],
  },
];

export const pageLinks = [
  { title: 'Playground', href: '/play' },
  { title: 'Blog', href: '/blog' },
  { title: 'npm', href: 'https://www.npmjs.com/package/reylo' },
];
