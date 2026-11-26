import { readdirSync } from 'node:fs';
import { index, route, type RouteConfig } from '@react-router/dev/routes';
import { docsRoutes } from 'docs-kit/routes';
import { builtPage } from '../lib/rust-content.ts';

// The homepage at /, all docs pages under /docs (index.mdx at /docs, migrate-<name>.mdx at
// /docs/migrate/<name>, every other file at /docs/<name>), each page's Markdown at <address>.md,
// the page a missing address gets, and the files beside them, each rendered to a file at build
// time (react-router.config.ts). The Rust page is one too, unlisted while the docs leave Rust out
// (lib/rust-content.ts).
const docFile = (name: string) => (name === 'index' ? 'docs.md' : name.startsWith('migrate-') ? `docs/migrate/${name.slice('migrate-'.length)}.md` : `docs/${name}.md`);
const markdown = readdirSync('content/docs')
  .filter(builtPage)
  .map((name) => docFile(name.slice(0, -'.mdx'.length)));
const blogMarkdown = readdirSync('content/blog')
  .filter((name) => name.endsWith('.mdx'))
  .map((name) => `blog/${name.slice(0, -'.mdx'.length)}.md`);

export default [
  // Reylo's homepage (components/home-landing.tsx), and all docs pages under /docs.
  index('routes/home.tsx'),
  route('docs', 'routes/page.tsx', { id: 'docs' }),
  route('docs/:slug', 'routes/page.tsx'),
  route('docs/migrate/usage', 'routes/page.tsx', { id: 'docs-migrate-usage' }),
  route('docs/migrate/cli', 'routes/page.tsx', { id: 'docs-migrate-cli' }),
  route('docs/migrate/compare', 'routes/page.tsx', { id: 'docs-migrate-compare' }),
  // Reylo Migrate's landing page stays at /migrate; its docs live under /docs/migrate.
  route('migrate', 'routes/migrate.tsx'),
  // The blog: its list of posts, and each post (content/blog, lib/blog.ts), plus Markdown mirrors.
  route('blog', 'routes/blog.tsx'),
  route('blog/:slug', 'routes/blog-post.tsx'),
  route('blog.md', 'routes/blog-markdown.ts', { id: 'markdown/blog.md' }),
  ...blogMarkdown.map((file) => route(file, 'routes/blog-markdown.ts', { id: `markdown/${file}` })),
  ...markdown.map((file) => route(file, 'routes/markdown.ts', { id: `markdown/${file}` })),
  // The playground, full screen (components/play.tsx).
  route('play', 'routes/play.tsx'),
  // Its link preview, the card of the query it opens with (a shared query's card is drawn on request).
  route('play/og.png', 'routes/play-og.tsx'),
  // Its API, as Markdown for an agent.
  route('play/api.md', 'routes/play-api-md.ts'),
  // The local playground (app/routes/playground.tsx), on the dev server only: it needs scratch databases.
  ...(process.env.NODE_ENV === 'production' ? [] : [route('playground', 'routes/playground.tsx')]),
  // docs-kit's page a missing address gets, robots.txt and the manifest, beside the site's own
  // /llms.txt and sitemap; the pages and their Markdown are routed above, at their own addresses.
  ...docsRoutes({ page: false, markdown: false, llms: 'routes/llms.ts', sitemap: 'routes/sitemap.ts' }),
  route('opengraph-image.png', 'routes/opengraph-image.tsx'),
  route('migrate/opengraph-image.png', 'routes/migrate-opengraph-image.tsx'),
] satisfies RouteConfig;
