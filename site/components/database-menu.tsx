'use client';

import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type MouseEvent, type PointerEvent, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { DIALECT_LOGOS } from './dialect-logos';
import type { Logo } from './host-logos';

/*
 * The database switch: the chosen database's logo and name in quiet grey, and a chevron, on the label
 * line of what it switches, with a menu of every database under it. It opens when a mouse rests on it,
 * on a click or a tap, and from the keyboard; it shuts 120ms after the pointer leaves it and its menu,
 * on a pick, on a press anywhere else and on Escape. The menu sits on the page's body, as a table's
 * declaration does on the homepage, since an inference card and a code-tab group clip what overflows
 * them. The docs' switch (`DialectSwitch`, ./dialect-tabs.tsx) and the homepage's (./home-landing.tsx)
 * both draw it; its styles are `.reylo-db-*` in app/global.css.
 */

/** A database's logo, in the colour of the text beside it. */
export function DatabaseLogo({ name }: { name: string }) {
  const logo = (DIALECT_LOGOS as Record<string, Logo | undefined>)[name];
  if (!logo) return null;
  return (
    <svg className="reylo-db-logo" viewBox={logo.viewBox} aria-hidden="true">
      {logo.paths.map((d, i) => (
        <path key={i} d={d} />
      ))}
    </svg>
  );
}

export function Chevron() {
  return (
    <svg className="reylo-db-chevron" viewBox="0 0 10 10" aria-hidden="true">
      <path d="M2.5 3.75 5 6.25l2.5-2.5" />
    </svg>
  );
}

/** The mark beside the chosen database in the menu. */
export function Check() {
  return (
    <svg className="reylo-db-check" viewBox="0 0 12 12" aria-hidden="true">
      <path d="M2.5 6.25 5 8.5l4.5-5" />
    </svg>
  );
}

/**
 * The switch that shows in place of `button`: an inference card holds a copy of its rows for each
 * database, each in that database's panel with a switch of its own, so a pick hides the switch it was
 * made on and shows its twin, in the same place, in the chosen database's panel.
 */
function showing(button: HTMLButtonElement | null): HTMLButtonElement | null {
  if (!button || button.getClientRects().length) return button;
  const panels = button.closest('[role="tabpanel"][data-state="inactive"]')?.parentElement;
  return [...(panels?.querySelectorAll<HTMLButtonElement>('.reylo-db-hit') ?? [])].find((twin) => twin.getClientRects().length) ?? null;
}

/**
 * The switch's open state and behaviour. `trigger` goes on the switch's button; `menu(items)` draws
 * the menu on the body while it is open; `shut(event)` closes it after a pick, handing the focus back
 * to the switch when the pick came from the keyboard.
 */
export function useDatabaseMenu() {
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  // Opened by a click, a tap or a key: the pointer leaving does not shut it.
  const pinned = useRef(false);
  // Opened from the keyboard: the focus moves into the menu.
  const fromKeys = useRef(false);

  const keep = () => clearTimeout(timer.current);
  const close = (refocus: boolean) => {
    keep();
    pinned.current = false;
    setOpen(false);
    if (refocus) showing(button.current)?.focus();
  };
  const enter = (e: PointerEvent) => {
    if (e.pointerType !== 'mouse') return;
    keep();
    setOpen(true);
  };
  const leave = (e: PointerEvent) => {
    if (e.pointerType !== 'mouse' || pinned.current) return;
    keep();
    timer.current = setTimeout(() => setOpen(false), 120);
  };
  const click = (e: MouseEvent) => {
    fromKeys.current = e.detail === 0;
    // A click on a menu the pointer opened keeps it open; another click shuts it.
    if (open && !pinned.current && !fromKeys.current) {
      pinned.current = true;
      return;
    }
    if (open) return close(false);
    pinned.current = true;
    setOpen(true);
  };

  // The menu hangs under the switch, its right edge on the switch's. It is placed again on every
  // render while open, since a pick switches the code above the switch and moves it.
  useLayoutEffect(() => {
    const menu = menuRef.current;
    const at = showing(button.current);
    if (!open || !menu || !at) return;
    const box = at.getBoundingClientRect();
    menu.style.top = `${box.bottom + window.scrollY + 6}px`;
    menu.style.right = `${Math.max(8, document.documentElement.clientWidth - box.right) - window.scrollX}px`;
  });

  // A press elsewhere, Escape or a resize shuts it. Tab from inside the menu shuts it with the focus
  // back on the switch, so the Tab moves on from there.
  useEffect(() => {
    if (!open) return;
    const press = (e: globalThis.PointerEvent) => {
      const target = e.target as Node;
      if (!button.current?.contains(target) && !menuRef.current?.contains(target)) close(false);
    };
    const key = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape') close(true);
      else if (e.key === 'Tab' && menuRef.current?.contains(document.activeElement)) close(true);
    };
    const resize = () => close(false);
    addEventListener('pointerdown', press, true);
    addEventListener('keydown', key);
    addEventListener('resize', resize);
    if (fromKeys.current) (menuRef.current?.querySelector<HTMLElement>('[data-state="active"]') ?? menuRef.current?.querySelector<HTMLElement>('button'))?.focus();
    return () => {
      removeEventListener('pointerdown', press, true);
      removeEventListener('keydown', key);
      removeEventListener('resize', resize);
    };
  }, [open]);

  useEffect(() => () => clearTimeout(timer.current), []);

  // Up and Down move between a menu's options; a menu of tabs moves itself (Radix's roving focus).
  const arrows = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    const options = [...e.currentTarget.querySelectorAll<HTMLElement>('[role="option"]')];
    if (!options.length) return;
    e.preventDefault();
    const at = options.indexOf(document.activeElement as HTMLElement);
    options[(at + (e.key === 'ArrowDown' ? 1 : -1) + options.length) % options.length]?.focus();
  };

  return {
    open,
    shut: (e: MouseEvent) => close(e.detail === 0),
    trigger: { ref: button, 'aria-haspopup': true, 'aria-expanded': open, onPointerEnter: enter, onPointerLeave: leave, onClick: click },
    menu: (items: ReactNode) =>
      open &&
      createPortal(
        <div ref={menuRef} className="reylo-db-menu" onPointerEnter={enter} onPointerLeave={leave} onKeyDown={arrows}>
          {items}
        </div>,
        document.body,
      ),
  };
}
