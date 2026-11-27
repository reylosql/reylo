import { defineSite } from 'docs-kit/site';
import { ogLogo, ogMark } from './components/og-logo';
import { docsUrl, githubUrl, siteTitle, siteUrl } from './lib/layout.shared';
import { ogAlt } from './lib/og';

// What Reylo's docs are, for Docs Kit (docs-kit/README.md, "A site's config"): the 404 page,
// robots.txt, the manifest and the structured data come from it. The site routes its pages itself
// (app/routes.ts), so this gives Docs Kit each page's address and the look of the link preview it
// draws for every page but the intro, which keeps the homepage's card
// (app/routes/opengraph-image.tsx): the glowing yellow wordmark, the title in Geist, and the mark
// blown up and faded on the site's dark page (components/og-logo.tsx).
export default defineSite({
  name: 'Reylo',
  url: siteUrl,
  repo: githubUrl,
  title: siteTitle,
  description: 'Import .sql files into TypeScript with full type inference.',
  pageUrl: (slug) => `${siteUrl}${docsUrl(slug ? `/${slug}` : '/')}`,
  author: { name: 'Colin McDonnell', url: 'https://github.com/colinhacks' },
  x: { creator: '@colinhacks' },
  footer: 'Released under the MIT License. Copyright © 2026 Colin McDonnell.',
  /** The home-screen tile's background, the page's own (public/icon-*.png, written by brand/mark.ts). */
  color: '#0e0e0e',
  ogAlt,
  og: {
    logo: ogLogo,
    mark: ogMark,
    fonts: [{ name: 'Geist', file: 'assets/fonts/Geist-600.ttf', weight: 600 }],
    titleFont: 'Geist',
    colors: { background: '#0e0e0e', title: '#fafafa', mark: 'rgba(251, 196, 14, 0.2)' },
  },
  software: { language: ['TypeScript', 'SQL'], platform: 'Node.js', license: 'https://opensource.org/licenses/MIT' },
});
