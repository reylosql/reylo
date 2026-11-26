import { useState, type ReactNode } from 'react';
import { StarPill } from './star-pill';
import { Wordmark } from './wordmark';
import { VariadicMark } from './migrate-landing';
import { githubUrl, introUrl } from '@/lib/layout.shared';
import './home-landing.css';

/*
 * The homepage's bar, install row and foot (components/home-landing.tsx), which the blog's index
 * (components/blog-index.tsx) and the Migrate landing (components/migrate-landing.tsx) share, in the styles of home-landing.css. Their own module, so the
 * blog's page doesn't load the homepage's examples.
 */

/** The homepage's links: the docs, the playground, Migrate and the blog. */
const LINKS: [label: string, href: string][] = [
  ['Docs', introUrl],
  ['Playground', '/play'],
  ['Migrate', '/#migrate'],
  ['Blog', '/blog'],
];

/** The bar at the top of the homepage, the blog's index and the Migrate landing: the wordmark, the links and the star pill with its note. */
export function Nav({ stars, brand, links = LINKS }: { stars?: number; brand?: { href: string; label: string; mark: ReactNode }; links?: [label: string, href: string][] }) {
  return (
    <nav className="ml-nav rh-nav">
      <a className="ml-brand ml-wordmark" href={brand?.href ?? '/'} aria-label={brand?.label ?? 'Reylo'}>
        {brand?.mark ?? <Wordmark />}
      </a>
      <div className="ml-links rh-links">
        {links.map(([label, href]) => (
          <a key={label} href={href}>
            {label}
          </a>
        ))}
        <span className="rh-star-wrap">
          <StarPill stars={stars} />
          <StarNote />
        </span>
      </div>
    </nav>
  );
}

const INSTALL = 'npm i reylo';

function CopyIcon({ done }: { done: boolean }) {
  return done ? (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path d="M3 8.5l3.2 3.2L13 4.8" />
    </svg>
  ) : (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <rect x="5.25" y="5.25" width="8.5" height="8.5" rx="1.75" />
      <path d="M10.75 5.25v-1.5a1.5 1.5 0 0 0-1.5-1.5h-5.5a1.5 1.5 0 0 0-1.5 1.5v5.5a1.5 1.5 0 0 0 1.5 1.5h1.5" />
    </svg>
  );
}

/**
 * The install command, with "Read the docs", which opens the docs (`docs`, the intro by default) at their top: side by side where the width allows. `centered` centres the two.
 * Given an `agentPrompt`, a quiet "Copy agent prompt" sits on its own line under them, the next step for a reader who sets projects up with a coding agent.
 */
export function Install({ centered, docs = introUrl, agentPrompt }: { centered?: boolean; docs?: string; agentPrompt?: string }) {
  const [copied, setCopied] = useState(false);
  const command = INSTALL;
  return (
    <div className={`ml-install rh-install${centered ? ' ml-centered' : ''}`}>
      <div className="ml-big">
        <div className="ml-box">
          <div className="ml-cmd">
            <span className="ml-dollar">$</span>
            <span>{command}</span>
            <button
              type="button"
              className="ml-copybtn"
              aria-label="Copy the command"
              onClick={() => {
                void navigator.clipboard.writeText(command);
                setCopied(true);
                setTimeout(() => setCopied(false), 1400);
              }}
            >
              <CopyIcon done={copied} />
            </button>
          </div>
        </div>
        <a className="ml-docs ml-docs-primary" href={docs}>
          Read the docs
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M2.5 8h11M9 3.5 13.5 8 9 12.5" />
          </svg>
        </a>
      </div>
      {agentPrompt !== undefined && <CopyAgentPromptLink text={agentPrompt} />}
    </div>
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
        <a href="/docs/inference">Inference</a>
        <a href="/play">Playground</a>
        <a href="/#migrate">Migrate</a>
        <a href="/blog">Blog</a>
        <a href={githubUrl}>GitHub</a>
        <a href="https://www.npmjs.com/package/reylo">npm</a>
      </div>
      <span className="ml-license">Released under the MIT License.</span>
    </footer>
  );
}

/** Lucide's sparkles, the mark of docs-kit's "Copy agent prompt": the prompt is for a coding agent. */
function SparklesIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z" />
      <path d="M20 3v4" />
      <path d="M22 5h-4" />
      <path d="M4 17v2" />
      <path d="M5 18H3" />
    </svg>
  );
}

/** "Copy agent prompt" under the install row, as a link: it copies the setup prompt the docs' agent card holds, and says "Copied" in the same place. */
function CopyAgentPromptLink({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="ml-agent"
      onClick={() => {
        void navigator.clipboard.writeText(text);
        setCopied(true);
        setTimeout(() => setCopied(false), 1600);
      }}
    >
      <SparklesIcon />
      <span className="ml-agent-label">
        <span data-shown={!copied || undefined}>Copy agent prompt</span>
        <span data-shown={copied || undefined} aria-live="polite">
          Copied
        </span>
      </span>
    </button>
  );
}
