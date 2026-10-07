import type { ReactNode } from 'react';

export default function Modal({ title, children, actions }: { title: string; children: ReactNode; actions: ReactNode }) {
  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" aria-label={title}>
        <h2>{title}</h2>
        <div>{children}</div>
        <div className="row" style={{ justifyContent: 'flex-end' }}>{actions}</div>
      </div>
    </div>
  );
}
