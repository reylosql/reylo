// A page's link preview, drawn at build time (README, "Link previews"): the site's logo and the page's
// address in a row at the top, and the page's title at the foot, on the site's colours and in its
// fonts, 1200 by 630. Drawn by @vercel/og's ImageResponse (Satori), which lays out flex boxes only, so
// every element with more than one child is a flex box. Run in Node only, by Docs Kit's card route
// (`routes/og.ts`) or by a site's own route for pages outside `content/docs` (a blog post).
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import type { ReactNode } from 'react';

/** A font the card draws with: a TTF, OTF or WOFF file (Satori reads no WOFF2), by its path from the site's root. */
export type OgFont = { name: string; file: string; weight?: 400 | 500 | 600 | 700 };

/** How a site's cards look: `og` in its `docs.config.ts`. Every field is optional. */
export type OgOptions = {
  /** The logo at the top left, drawn 44px tall, which may differ by page. The site's name in the title's font when none. */
  logo?: (page: OgPage) => ReactNode;
  /** The fonts. With none, the card draws in @vercel/og's own Geist. */
  fonts?: OgFont[];
  /** The title's font, by name. */
  titleFont?: string;
  /** The title's weight: 600 when none. */
  titleWeight?: 400 | 500 | 600 | 700;
  /** The address's font, by name: the title's when none. */
  addressFont?: string;
  colors?: { background?: string; title?: string; address?: string };
};

/** What a card shows. */
export type OgPage = {
  /** The page's slug, for `logo`: `''` for the first page. */
  slug: string;
  title: string;
  /** The page's address, shown without its scheme. */
  url: string;
  /** The site's name, the logo when the site draws none. */
  name: string;
};

export const ogSize = { width: 1200, height: 630 };

// The ink of every edge sits 64px in: at the foot, the title's last baseline, which sits about 0.17em
// above the bottom of its line's box (measured in Geist), so the padding at the foot is that much short.
const PAD = 64;
const LOGO = 44;
const TITLE = 112;

/**
 * Text with each space a no-break space and a zero-width space after it: Satori measures a word and
 * the space after it apart, and in some fonts (Geist) that spreads the gap before some words to
 * nearly twice the rest, while a no-break space is measured with its word; the zero-width space is
 * where the line may still break.
 */
const evenSpaces = (text: string) => text.replace(/ /g, '\u00a0\u200b');

function Card({ page, options }: { page: OgPage; options: OgOptions }) {
  const { titleFont, titleWeight = 600, addressFont = titleFont, colors = {} } = options;
  const color = { background: '#0e0e0e', title: '#fafafa', address: '#a1a1a1', ...colors };
  // Two lines at the full size hold about 36 characters; a longer title is set smaller.
  const size = page.title.length <= 36 ? TITLE : TITLE * 0.72;
  return (
    <div style={{ width: '100%', height: '100%', display: 'flex', flexDirection: 'column', justifyContent: 'space-between', padding: `${PAD}px ${PAD}px ${PAD - size * 0.17}px`, background: color.background }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', height: LOGO }}>
        <div style={{ display: 'flex', alignItems: 'center' }}>
          {options.logo?.(page) ?? <div style={{ display: 'flex', fontFamily: titleFont, fontWeight: titleWeight, fontSize: LOGO * 0.8, color: color.title }}>{page.name}</div>}
        </div>
        {/* Lowercase ink sits below the middle of its line, so the address is lifted 2px to the logo's middle. */}
        <div style={{ display: 'flex', marginBottom: 4, fontFamily: addressFont, fontSize: 23, color: color.address }}>{page.url.replace(/^https?:\/\//, '')}</div>
      </div>
      {/* A capital's stem stands about 0.05em in from its box, so the title is pulled left by that to line its ink up with the logo's. */}
      <div style={{ display: 'block', lineClamp: 2, marginLeft: -size * 0.05, fontFamily: titleFont, fontWeight: titleWeight, fontSize: size, lineHeight: 1.05, letterSpacing: -size * 0.03, color: color.title }}>{evenSpaces(page.title)}</div>
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
