import { defineSite } from 'docs-kit/site';
import { ogLogo } from './components/og-logo';
import { docsUrl, githubUrl, siteTitle, siteUrl } from './lib/layout.shared';
import { ogAlt } from './lib/og';

// What Reylo's docs are, for Docs Kit (docs-kit/README.md, "A site's config"). The site routes its
// pages itself (app/routes.ts), so this gives Docs Kit each page's address and the look of the link
// preview it draws for every page but the intro, which keeps the homepage's card
// (app/routes/opengraph-image.tsx): the site's dark page, Geist for the title and Geist Mono for the
// address, as the homepage's card sets them.
export default defineSite({
  name: 'Reylo',
  url: siteUrl,
  repo: githubUrl,
  title: siteTitle,
  pageUrl: (slug) => `${siteUrl}${docsUrl(slug ? `/${slug}` : '/')}`,
  ogAlt,
  og: {
    logo: ogLogo,
    fonts: [
      { name: 'Geist', file: 'assets/fonts/Geist-600.ttf', weight: 600 },
      { name: 'Geist Mono', file: 'assets/fonts/GeistMono-400.ttf', weight: 400 },
    ],
    titleFont: 'Geist',
    addressFont: 'Geist Mono',
    colors: { background: '#0e0e0e', title: '#fafafa', address: '#a1a1a1' },
  },
});
