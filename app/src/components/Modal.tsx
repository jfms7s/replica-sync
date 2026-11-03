import { useEffect, useRef, type KeyboardEvent, type ReactNode } from 'react';

const FOCUSABLE = 'button:not([disabled]), input:not([disabled]), select, textarea, a[href], [tabindex]:not([tabindex="-1"])';

/**
 * A dialog: focus starts on its first button and Tab stays inside it. Escape
 * calls `onClose` (the call site's Cancel / No) when one is given.
 */
export default function Modal({ title, children, actions, onClose }: {
  title: string; children: ReactNode; actions: ReactNode; onClose?: () => void;
}) {
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const first = box.current?.querySelector<HTMLElement>('button:not([disabled])')
      ?? box.current?.querySelector<HTMLElement>(FOCUSABLE);
    (first ?? box.current)?.focus();
  }, []);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Escape' && onClose) {
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key !== 'Tab' || !box.current) return;
    const items = Array.from(box.current.querySelectorAll<HTMLElement>(FOCUSABLE));
    if (items.length === 0) {
      e.preventDefault();
      return;
    }
    const [first, last] = [items[0], items[items.length - 1]];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" aria-label={title} ref={box} tabIndex={-1} onKeyDown={onKeyDown}>
        <h2>{title}</h2>
        <div>{children}</div>
        <div className="row" style={{ justifyContent: 'flex-end' }}>{actions}</div>
      </div>
    </div>
  );
}
