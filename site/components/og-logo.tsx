// The logo and mark on the link preview Docs Kit draws for each page (docs.config.ts). The logo is the
// wordmark as the sidebar sets it by night: all yellow, glowing (app/global.css, `.reylo-brand` and
// `--saber-glow-soft`), and Reylo Migrate's on Migrate's pages. The mark is drawn large and faded on
// the card's right.
import type { OgPage } from 'docs-kit/og';
import { migrateWidth, migrateWords } from './migrate-wordmark';
import { wordmark } from './wordmark';

const HEIGHT = 44;
const YELLOW = '#fbc40e';
/** View-box units per pixel at the logo's height. */
const UNIT = wordmark.height / HEIGHT;
/** Room around the wordmark for its glow, in view-box units: the far light's reach. */
const ROOM = 90;

/** Whether a page is one of Reylo Migrate's: its intro, migrate.mdx, and each migrate-<name>.mdx. */
export const migratePage = (slug: string) => slug === 'migrate' || slug.startsWith('migrate-');

/** The mark, as the card draws it large. */
export const ogMark = { viewBox: '0 0 400 235.47', d: wordmark.mark };

/**
 * The wordmark in yellow, with the soft glow the sidebar gives it at 24px scaled to 44: a near gold
 * light 2px out and a far amber one 7px out there, each a Gaussian blur of half that radius. The
 * glow needs room past the wordmark's box, which negative margins give back, so its ink sits where
 * the box would.
 */
export function ogLogo({ slug }: OgPage) {
  const [width, word] = migratePage(slug) ? [migrateWidth, migrateWords] : [wordmark.width, wordmark.word];
  const scale = HEIGHT / 24;
  const blur = (px: number) => (px * scale) / 2 * UNIT;
  return (
    <svg
      width={(width + 2 * ROOM) / UNIT}
      height={(wordmark.height + 2 * ROOM) / UNIT}
      viewBox={`${-ROOM} ${-ROOM} ${width + 2 * ROOM} ${wordmark.height + 2 * ROOM}`}
      style={{ margin: -ROOM / UNIT }}
    >
      <defs>
        <filter id="glow" x={-ROOM} y={-ROOM} width={width + 2 * ROOM} height={wordmark.height + 2 * ROOM} filterUnits="userSpaceOnUse">
          <feGaussianBlur in="SourceAlpha" stdDeviation={blur(2)} result="near" />
          <feFlood floodColor="rgb(255, 200, 31)" floodOpacity={0.55} />
          <feComposite in2="near" operator="in" result="nearLight" />
          <feGaussianBlur in="SourceAlpha" stdDeviation={blur(7)} result="far" />
          <feFlood floodColor="rgb(255, 184, 0)" floodOpacity={0.4} />
          <feComposite in2="far" operator="in" result="farLight" />
          <feMerge>
            <feMergeNode in="farLight" />
            <feMergeNode in="nearLight" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
      </defs>
      <g filter="url(#glow)">
        <path fill={YELLOW} d={wordmark.mark} />
        <path fill={YELLOW} transform={wordmark.wordTransform} d={word} />
      </g>
    </svg>
  );
}
