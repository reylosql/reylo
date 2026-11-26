import { markdownOf } from '@/lib/markdown';

// One page as Markdown, at /docs.md for the intro, /docs/migrate/<name>.md for Reylo Migrate's
// sub-pages, and /docs/<slug>.md for each other page of content/docs (lib/markdown.ts), written to
// build/client at build time. vercel.json answers a request for the page's own address that asks
// for Markdown (`Accept: text/markdown`, as Claude Code, Cursor and OpenCode send) with this file,
// as docs-kit's README describes.
export async function loader({ request }: { request: Request }) {
  // The address is /docs.md, /docs/<slug>.md or /docs/migrate/<name>.md (with `.data` when the build asks for the route's data).
  const path = new URL(request.url).pathname.replace(/\.md(?:\.data)?$/, '');
  const slug = path === '/docs' ? '' : path.startsWith('/docs/migrate/') ? `migrate-${path.slice('/docs/migrate/'.length)}` : path.replace(/^\/docs\//, '');
  return new Response(await markdownOf(slug), {
    headers: { 'Content-Type': 'text/markdown; charset=utf-8' },
  });
}
