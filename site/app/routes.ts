import { readdirSync } from 'node:fs';
import { index, route, type RouteConfig } from '@react-router/dev/routes';
import { builtPage } from '../lib/rust-content.ts';

// The homepage at /, the pages at /docs (the intro) and /<name> (content/docs), each page's Markdown,
// at /index.md and /<name>.md, the page a missing address gets, and the files beside them, each
// rendered to a file at build time (react-router.config.ts). The Rust page is one too, unlisted while
// the docs leave Rust out (lib/rust-content.ts).
const markdown = readdirSync('content/docs')
  .filter(builtPage)
  .map((name) => `${name.slice(0, -'.mdx'.length)}.md`);
const blogMarkdown = readdirSync('content/blog')
  .filter((name) => name.endsWith('.mdx'))
  .map((name) => `blog/${name.slice(0, -'.mdx'.length)}.md`);

export default [
  // Reylo's homepage (components/home-landing.tsx), and the docs' intro (content/docs/index.mdx) under it.
  index('routes/home.tsx'),
  route('docs', 'routes/page.tsx', { id: 'docs' }),
  route(':slug', 'routes/page.tsx'),
  // Reylo Migrate: its landing page, and under it its docs, content/docs/migrate.mdx at /migrate/docs
  // and each content/docs/migrate-<name>.mdx at /migrate/<name>: usage, the CLI and the comparison
  // with other migration tools.
  route('migrate', 'routes/migrate.tsx'),
  route('migrate/docs', 'routes/page.tsx', { id: 'migrate-docs' }),
  route('migrate/usage', 'routes/page.tsx', { id: 'migrate-usage' }),
  route('migrate/cli', 'routes/page.tsx', { id: 'migrate-cli' }),
  route('migrate/compare', 'routes/page.tsx', { id: 'migrate-compare' }),
  // The blog: its list of posts, and each post (content/blog, lib/blog.ts), plus Markdown mirrors.
  route('blog', 'routes/blog.tsx'),
  route('blog/:slug', 'routes/blog-post.tsx'),
  route('blog.md', 'routes/blog-markdown.ts', { id: 'markdown/blog.md' }),
  ...blogMarkdown.map((file) => route(file, 'routes/blog-markdown.ts', { id: `markdown/${file}` })),
  ...markdown.map((file) => route(file, 'routes/markdown.ts', { id: `markdown/${file}` })),
  route('404', 'routes/not-found.tsx'),
  // The playground, full screen (components/play.tsx).
  route('play', 'routes/play.tsx'),
  // Its link preview, the card of the query it opens with (a shared query's card is drawn on request).
  route('play/og.png', 'routes/play-og.tsx'),
  // Its API, as Markdown for an agent.
  route('play/api.md', 'routes/play-api-md.ts'),
  // The local playground (app/routes/playground.tsx), on the dev server only: it needs scratch databases.
  ...(process.env.NODE_ENV === 'production' ? [] : [route('playground', 'routes/playground.tsx')]),
  route('llms.txt', 'routes/llms.ts'),
  route('robots.txt', 'routes/robots.ts'),
  route('sitemap.xml', 'routes/sitemap.ts'),
  route('manifest.webmanifest', 'routes/manifest.ts'),
  route('opengraph-image.png', 'routes/opengraph-image.tsx'),
  route('migrate/opengraph-image.png', 'routes/migrate-opengraph-image.tsx'),
] satisfies RouteConfig;
