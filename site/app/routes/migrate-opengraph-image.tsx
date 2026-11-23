// Reylo Migrate's link preview, for /migrate, written to build/client/migrate/opengraph-image.png at
// build time: the page links it as og:image, and as X's image too, with a hash of this file's text in
// its URL (app/routes/migrate.tsx), so that a change to the card changes the URL and the caches of X
// and Slack fetch the new card. It is drawn as the homepage's card is (app/routes/opengraph-image.tsx).
//
// The lockup the Migrate launch video opens and closes on: the mark in yellow, over REYLO MIGRATE in
// Orbitron, over the tagline. Drawn by @vercel/og's ImageResponse (Satori), on the site's dark ground:
// Orbitron at weight 600, a static instance of assets/fonts/orbitron.woff2, since Satori reads no
// variable font, and Geist Mono (assets/fonts, SIL Open Font License). Satori lays out flex boxes only,
// so every element with more than one child is a flex box.
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import type { ReactNode } from 'react';
import { wordmark } from '@/components/wordmark';

import { ogSize } from '@/lib/og';

const color = {
  page: '#0f0f10',
  name: '#fafafa',
  tagline: '#a8a8a8',
  yellow: '#fbc40e',
};

// The mark is 400 × 235.47 units, at the left of the wordmark's view box.
const MARK = 96;
const NAME = 72;

function Card(): ReactNode {
  return (
    <div style={{ width: '100%', height: '100%', display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', paddingBottom: 20, background: color.page }}>
      <svg width={(MARK * 400) / 235.47} height={MARK} viewBox="0 0 400 235.47">
        <path fill={color.yellow} d={wordmark.mark} />
      </svg>
      {/* The letter spacing trails the last letter too, so one spacing of negative margin on the right centres the name's ink. */}
      <div style={{ display: 'flex', marginTop: 36, marginRight: -NAME * 0.12, fontFamily: 'Orbitron', fontWeight: 600, fontSize: NAME, lineHeight: 1, letterSpacing: NAME * 0.12, color: color.name }}>REYLO MIGRATE</div>
      {/* The closing period inks less than its cell, so the tagline is pulled right 3px to centre its ink. */}
      <div style={{ display: 'flex', marginTop: 30, marginLeft: 6, fontFamily: 'Geist Mono', fontSize: 32, lineHeight: 1, color: color.tagline }}>SQL-first schema and migrations.</div>
    </div>
  );
}

const font = async (file: string, name: string, weight: 400 | 600) => ({
  name,
  weight,
  style: 'normal' as const,
  data: await readFile(join(process.cwd(), 'assets/fonts', file)),
});

/** The preview as a PNG, built once, with the site. Its description is the page's og:image:alt (lib/og.ts). */
export async function loader() {
  const { ImageResponse } = await import('@vercel/og');
  const fonts = await Promise.all([font('Orbitron-600.ttf', 'Orbitron', 600), font('GeistMono-400.ttf', 'Geist Mono', 400)]);
  return new ImageResponse(<Card />, { ...ogSize, fonts });
}
