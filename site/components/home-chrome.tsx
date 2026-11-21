import { StarPill } from './star-pill';
import { Mark, VariadicMark } from './migrate-landing';
import { githubUrl, introUrl } from '@/lib/layout.shared';
import './home-landing.css';

/*
 * The homepage's bar and foot (components/home-landing.tsx), which the blog's index
 * (components/blog-index.tsx) shares, in the styles of home-landing.css. Their own module, so the
 * blog's page doesn't load the homepage's examples.
 */

/** The bar at the top of the homepage and the blog's index. */
export function Nav({ stars }: { stars?: number }) {
  return (
    <nav className="ml-nav rh-nav">
      <a className="ml-brand" href="/">
        <Mark />
        Reylo
      </a>
      <div className="ml-links rh-links">
        <a href={introUrl}>Docs</a>
        <a href="/play">Playground</a>
        <a href="/migrate">Migrate</a>
        <a href="/blog">Blog</a>
        <span className="rh-star-wrap">
          <StarPill stars={stars} />
          <StarNote />
        </span>
      </div>
    </nav>
  );
}

/**
 * "Leave a star!" in a hand, under the pill, and an arrow drawn from the end of it, sweeping right and
 * then up into the middle of the pill's underside; on a wide window, mirrored onto the pill's right. Drawn from the pill's own box (`.rh-star-note`), so
 * it lands on the pill at every width; the nav scrolls away with the page, and the note with it.
 */
function StarNote() {
  return (
    <>
      <svg className="rh-star-note rh-star-note-left" viewBox="0 0 170 84" fill="none" aria-hidden="true">
        <text x="62" y="62" textAnchor="end" transform="rotate(-4 62 62)">
          Leave a star!
        </text>
        <path d="M 72 57 C 110 63, 144 36, 150 3" />
        <path d="M 142 15 L 150 3 L 158 14" />
      </svg>
      <svg className="rh-star-note rh-star-note-right" viewBox="0 0 170 84" fill="none" aria-hidden="true">
        <text x="66" y="62" transform="rotate(-4 66 62)">
          Leave a star!
        </text>
        <path d="M 58 57 C 34 60, 20 36, 20 3" />
        <path d="M 12 14 L 20 3 L 28 15" />
      </svg>
    </>
  );
}


/** The foot of the homepage and the blog's index. */
export function Foot() {
  return (
    <footer className="ml-foot">
      <a className="ml-variadic" href="https://variadic.sh">
        <VariadicMark />A Variadic project
      </a>
      <div className="ml-foot-links">
        <a href={introUrl}>Docs</a>
        <a href="/inference">Inference</a>
        <a href="/play">Playground</a>
        <a href="/migrate">Migrate</a>
        <a href="/blog">Blog</a>
        <a href={githubUrl}>GitHub</a>
        <a href="https://www.npmjs.com/package/reylo">npm</a>
      </div>
      <span className="ml-license">Released under the MIT License.</span>
    </footer>
  );
}
