// A page's link preview, at `/og/<slug>.png` and `/og/index.png` for the first page (README, "Link
// previews"): the card `docs-kit/og` draws, from the page's title and the site's `og`.
import site from '@/docs.config';
import { ogCard } from '../og';
import { source } from '../page.server';
import { urlOf } from '../site';

export async function loader({ request }: { request: Request }) {
  const base = new URL(site.url).pathname.replace(/\/$/, '');
  const pathname = new URL(request.url).pathname;
  const name = (base && pathname.startsWith(`${base}/`) ? pathname.slice(base.length) : pathname).replace(/^\/og\/|\.png(?:\.data)?$/g, '');
  const slug = name === 'index' ? '' : name;
  const page = source.getPage(slug ? [slug] : []);
  if (!page) throw new Response('Not found', { status: 404 });
  return ogCard({ slug, title: page.data.title ?? '', url: urlOf(site, slug), name: site.name }, site.og);
}
