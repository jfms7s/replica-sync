import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';

const handlers: Record<string, (p: unknown) => void> = {};
vi.mock('../api', () => ({
  api: { stopAndClose: vi.fn().mockResolvedValue(undefined) },
  onEvent: vi.fn(async (name: string, cb: (p: unknown) => void) => { handlers[name] = cb; return () => {}; }),
}));
import { api } from '../api';
import CloseGuard from './CloseGuard';

describe('CloseGuard', () => {
  it('asks when the window is closed mid-sync and stops on Yes', async () => {
    render(<I18nProvider lang="en"><CloseGuard /></I18nProvider>);
    await vi.waitFor(() => expect(handlers['close-requested']).toBeDefined());
    act(() => handlers['close-requested'](null));
    expect(screen.getByRole('dialog', { name: 'A sync is running' })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Keep syncing' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    act(() => handlers['close-requested'](null));
    await userEvent.click(screen.getByRole('button', { name: 'Stop and close' }));
    expect(api.stopAndClose).toHaveBeenCalled();
  });

  it('stop_failure_is_shown_in_the_modal', async () => {
    vi.mocked(api.stopAndClose).mockRejectedValueOnce({ code: 'drive.notConnected', params: { label: 'USB' } });
    render(<I18nProvider lang="en"><CloseGuard /></I18nProvider>);
    await vi.waitFor(() => expect(handlers['close-requested']).toBeDefined());
    act(() => handlers['close-requested'](null));
    await userEvent.click(screen.getByRole('button', { name: 'Stop and close' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('USB is not connected.');
    expect(screen.getByRole('button', { name: 'Stop and close' })).toBeEnabled();
  });

  it('after Stop it keeps both buttons disabled and says it is stopping', async () => {
    vi.mocked(api.stopAndClose).mockResolvedValueOnce(undefined);
    render(<I18nProvider lang="en"><CloseGuard /></I18nProvider>);
    await vi.waitFor(() => expect(handlers['close-requested']).toBeDefined());
    act(() => handlers['close-requested'](null));
    await userEvent.click(screen.getByRole('button', { name: 'Stop and close' }));
    expect(await screen.findByText('Stopping after the current file…')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Stop and close' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Keep syncing' })).toBeDisabled();
  });
});
