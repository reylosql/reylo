import { ogSize } from '@/lib/og';
import { siteUrl } from '@/lib/layout.shared';

/** The blog's and each post's metadata: the title, the description, and the link previews, whose image and its text the route gives (the homepage's card for the blog, each post's own for a post). */
export function blogMeta({ title, description, path, image, alt, type }: { title: string; description: string; path: string; image: string; alt: string; type: 'website' | 'article' }) {
  const url = `${siteUrl}${path}`;
  return [
    { title },
    { name: 'description', content: description },
    { name: 'application-name', content: 'Reylo' },
    { name: 'author', content: 'Colin McDonnell' },
    { name: 'robots', content: 'index, follow' },
    { tagName: 'link', rel: 'canonical', href: url },
    { tagName: 'link', rel: 'alternate', type: 'text/markdown', href: `${url}.md` },
    { property: 'og:title', content: title },
    { property: 'og:description', content: description },
    { property: 'og:url', content: url },
    { property: 'og:site_name', content: 'Reylo' },
    { property: 'og:type', content: type },
    { property: 'og:image', content: image },
    { property: 'og:image:width', content: String(ogSize.width) },
    { property: 'og:image:height', content: String(ogSize.height) },
    { property: 'og:image:alt', content: alt },
    { name: 'twitter:card', content: 'summary_large_image' },
    { name: 'twitter:creator', content: '@colinhacks' },
    { name: 'twitter:title', content: title },
    { name: 'twitter:description', content: description },
    { name: 'twitter:image', content: image },
    { name: 'twitter:image:alt', content: alt },
  ];
}
