// A page as Markdown, at its address with `.md` after it (`/postgres.md`, and `/index.md` for the
// first page), on a site of several pages (README, "Markdown for agents").
import site from '@/docs.config';
import { markdownOf } from '../page.server';

export async function loader({ request }: { request: Request }) {
  const base = new URL(site.url).pathname.replace(/\/$/, '');
  const pathname = new URL(request.url).pathname;
  const name = (base && pathname.startsWith(`${base}/`) ? pathname.slice(base.length) : pathname).replace(/^\/|\.md(?:\.data)?$/g, '');
  return new Response(await markdownOf(name === 'index' ? '' : name), { headers: { 'Content-Type': 'text/markdown; charset=utf-8' } });
}
