import { defineSite } from 'docs-kit/site';
import { ogAlt } from './lib/og.ts';

/**
 * The site, as docs-kit reads it: the 404 page, robots.txt, the manifest and the structured data
 * (docs-kit/src/site.ts). The site's own routes read its address, repository and title from here too
 * (lib/layout.shared.tsx).
 */
export default defineSite({
  name: 'Reylo',
  /** reylo.dev, which every other Reylo domain redirects to (wiki/docs-site.md). */
  url: 'https://reylo.dev',
  repo: 'https://github.com/reylosql/reylo',
  /**
   * The title search results and link previews show: the name and what it does, in under 60
   * characters so that Google does not cut it. The page's h1 stays the name alone.
   */
  title: 'Reylo — Import .sql files into TypeScript, fully typed',
  description: 'Import .sql files into TypeScript with full type inference.',
  author: { name: 'Colin McDonnell', url: 'https://github.com/colinhacks' },
  x: { creator: '@colinhacks' },
  footer: 'Released under the MIT License. Copyright © 2026 Colin McDonnell.',
  /** The home-screen tile's background, the page's own (public/icon-*.png, written by brand/mark.ts). */
  color: '#0e0e0e',
  ogAlt,
  software: { language: ['TypeScript', 'SQL'], platform: 'Node.js', license: 'https://opensource.org/licenses/MIT' },
});
