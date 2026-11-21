import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { ogAlt, ogSize } from '@/lib/og';
import { siteUrl } from '@/lib/layout.shared';

/** The hash of the link preview's route, which its URL carries, so that a change to the card changes the URL and the caches of X and Slack fetch the new card. Read by a loader, at build time. */
export async function ogHash(): Promise<string> {
  return createHash('sha256').update(await readFile('app/routes/opengraph-image.tsx')).digest('hex').slice(0, 16);
}

/** The blog's and each post's metadata: the title, the description, and the link previews, the site's own card. */
export function blogMeta({ title, description, path, og, type }: { title: string; description: string; path: string; og: string; type: 'website' | 'article' }) {
  const url = `${siteUrl}${path}`;
  const image = `${siteUrl}/opengraph-image.png?${og}`;
  return [
    { title },
    { name: 'description', content: description },
    { name: 'application-name', content: 'Reylo' },
    { name: 'author', content: 'Colin McDonnell' },
    { name: 'robots', content: 'index, follow' },
    { tagName: 'link', rel: 'canonical', href: url },
    { property: 'og:title', content: title },
    { property: 'og:description', content: description },
    { property: 'og:url', content: url },
    { property: 'og:site_name', content: 'Reylo' },
    { property: 'og:type', content: type },
    { property: 'og:image', content: image },
    { property: 'og:image:width', content: String(ogSize.width) },
    { property: 'og:image:height', content: String(ogSize.height) },
    { property: 'og:image:alt', content: ogAlt },
    { name: 'twitter:card', content: 'summary_large_image' },
    { name: 'twitter:creator', content: '@colinhacks' },
    { name: 'twitter:title', content: title },
    { name: 'twitter:description', content: description },
    { name: 'twitter:image', content: image },
    { name: 'twitter:image:alt', content: ogAlt },
  ];
}
