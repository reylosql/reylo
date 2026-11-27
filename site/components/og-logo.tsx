// The logo on the link preview Docs Kit draws for each page (docs.config.ts): Reylo's wordmark, or
// Reylo Migrate's on Migrate's pages, the mark in the site's yellow and the words in the headings'
// white, as the site's own Migrate card sets them (app/routes/migrate-opengraph-image.tsx). The mark
// is the one thinned for a light shape on a dark ground.
import type { OgPage } from 'docs-kit/og';
import { migrateWidth, migrateWords } from './migrate-wordmark';
import { wordmark } from './wordmark';

const HEIGHT = 44;

/** Whether a page is one of Reylo Migrate's: its intro, migrate.mdx, and each migrate-<name>.mdx. */
export const migratePage = (slug: string) => slug === 'migrate' || slug.startsWith('migrate-');

export function ogLogo({ slug }: OgPage) {
  const [width, word] = migratePage(slug) ? [migrateWidth, migrateWords] : [wordmark.width, wordmark.word];
  return (
    <svg width={(HEIGHT * width) / wordmark.height} height={HEIGHT} viewBox={`0 0 ${width} ${wordmark.height}`}>
      <path fill="#fbc40e" d={wordmark.markOnDark} />
      <path fill="#fafafa" transform={wordmark.wordTransform} d={word} />
    </svg>
  );
}
