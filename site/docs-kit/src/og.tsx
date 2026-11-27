// A page's link preview, drawn at build time (README, "Link previews"): the site's logo at the top left
// and the page's title at the foot, beside the site's mark blown up and cropped by the card's edges in
// a faded colour, 1200 by 630, in the site's colours and fonts. Drawn by @vercel/og's ImageResponse
// (Satori), which lays out flex boxes only, so every element with more than one child is a flex box.
// Run in Node only, by Docs Kit's card route (`routes/og.ts`) or by a site's own route for pages
// outside `content/docs` (a blog post).
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import type { ReactNode } from 'react';

/** A font the card draws with: a TTF, OTF or WOFF file (Satori reads no WOFF2), by its path from the site's root. */
export type OgFont = { name: string; file: string; weight?: 400 | 500 | 600 | 700 };

/** How a site's cards look: `og` in its `docs.config.ts`. Every field is optional. */
export type OgOptions = {
  /** The logo at the top left, drawn 44px tall, which may differ by page. The site's name in the title's font when none. */
  logo?: (page: OgPage) => ReactNode;
  /** The mark drawn large on the right, as one SVG path in its view box; none when the site gives none. */
  mark?: { viewBox: string; d: string };
  /** The fonts. With none, the card draws in @vercel/og's own Geist. */
  fonts?: OgFont[];
  /** The title's font, by name. */
  titleFont?: string;
  /** The title's weight: 600 when none. */
  titleWeight?: 400 | 500 | 600 | 700;
  colors?: { background?: string; title?: string; mark?: string };
};

/** What a card shows. */
export type OgPage = {
  /** The page's slug, for `logo`: `''` for the first page. */
  slug: string;
  title: string;
  /** The page's address. */
  url: string;
  /** The site's name, the logo when the site draws none. */
  name: string;
};

export const ogSize = { width: 1200, height: 630 };

const PAD = 64;
const LOGO = 44;
// The mark is drawn 720px tall from 560px in, its middle on the card's, so its far side runs off the
// right edge and its top and bottom off the card's.
const MARK = 720;
const MARK_LEFT = 560;

/**
 * Text with each space a no-break space and a zero-width space after it: Satori measures a word and
 * the space after it apart, and in some fonts (Geist) that spreads the gap before some words to
 * nearly twice the rest, while a no-break space is measured with its word; the zero-width space is
 * where the line may still break.
 */
const evenSpaces = (text: string) => text.replace(/ /g, ' ​');

function Card({ page, options }: { page: OgPage; options: OgOptions }) {
  const { titleFont, titleWeight = 600, mark } = options;
  const color = { background: '#0e0e0e', title: '#fafafa', mark: 'rgba(251, 196, 14, 0.2)', ...options.colors };
  // A title past ten characters takes two lines in the left column, at a smaller size.
  const size = page.title.length > 10 ? 64 : 96;
  const [, , w = 1, h = 1] = mark ? mark.viewBox.split(/[\s,]+/).map(Number) : [];
  return (
    <div style={{ display: 'flex', position: 'relative', width: '100%', height: '100%', overflow: 'hidden', background: color.background }}>
      {mark ? (
        <svg style={{ position: 'absolute', left: MARK_LEFT, top: (ogSize.height - MARK) / 2 }} width={(MARK * w) / h} height={MARK} viewBox={mark.viewBox}>
          <path fill={color.mark} d={mark.d} />
        </svg>
      ) : null}
      <div style={{ display: 'flex', position: 'absolute', left: PAD, top: PAD, bottom: PAD - size * 0.17, width: 470, flexDirection: 'column', justifyContent: 'space-between' }}>
        <div style={{ display: 'flex', alignItems: 'center', height: LOGO }}>
          {options.logo?.(page) ?? <div style={{ display: 'flex', fontFamily: titleFont, fontWeight: titleWeight, fontSize: LOGO * 0.8, color: color.title }}>{page.name}</div>}
        </div>
        {/* A capital's stem stands about 0.05em in from its box, so the title is pulled left by that to line its ink up with the logo's. */}
        <div style={{ display: 'block', lineClamp: 2, marginLeft: -size * 0.05, fontFamily: titleFont, fontWeight: titleWeight, fontSize: size, lineHeight: 1.05, letterSpacing: -size * 0.03, color: color.title }}>{evenSpaces(page.title)}</div>
      </div>
    </div>
  );
}

/** A page's card as a PNG response, for a route's loader. */
export async function ogCard(page: OgPage, options: OgOptions = {}): Promise<Response> {
  const { ImageResponse } = await import('@vercel/og');
  const fonts = await Promise.all(
    (options.fonts ?? []).map(async (font) => ({ name: font.name, weight: font.weight ?? 400, style: 'normal' as const, data: await readFile(path.resolve(font.file)) })),
  );
  return new ImageResponse(<Card page={page} options={options} />, { ...ogSize, ...(fonts.length ? { fonts } : {}) });
}
