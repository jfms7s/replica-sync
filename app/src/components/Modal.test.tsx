import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import Modal from './Modal';

describe('Modal', () => {
  const open = (onClose?: () => void) => render(
    <Modal title="Sure?" onClose={onClose} actions={<><button>No</button><button>Yes</button></>}>Body</Modal>,
  );

  it('starts with focus on its first button and keeps Tab inside', async () => {
    open();
    expect(screen.getByRole('button', { name: 'No' })).toHaveFocus();
    await userEvent.tab();
    expect(screen.getByRole('button', { name: 'Yes' })).toHaveFocus();
    await userEvent.tab();
    expect(screen.getByRole('button', { name: 'No' })).toHaveFocus();
    await userEvent.tab({ shift: true });
    expect(screen.getByRole('button', { name: 'Yes' })).toHaveFocus();
  });

  it('closes on Escape', async () => {
    const onClose = vi.fn();
    open(onClose);
    await userEvent.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
