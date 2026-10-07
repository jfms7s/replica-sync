import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';

const handlers: Record<string, (p: unknown) => void> = {};
vi.mock('../api', () => ({
  api: { apply: vi.fn().mockResolvedValue(undefined), retryFailed: vi.fn().mockResolvedValue(undefined),
         pause: vi.fn(), resume: vi.fn(), cancel: vi.fn() },
  onEvent: vi.fn(async (name: string, cb: (p: unknown) => void) => { handlers[name] = cb; return () => {}; }),
}));
import { api, onEvent } from '../api';
import Applying from './Applying';

describe('Applying', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    for (const k of Object.keys(handlers)) delete handlers[k];
    vi.mocked(onEvent).mockImplementation((async (name: string, cb: (p: unknown) => void) => { handlers[name] = cb; return () => {}; }) as never);
  });

  it('starts after listening, shows progress, pauses, then shows the result', async () => {
    const navigate = vi.fn();
    render(<I18nProvider lang="en"><Applying mode="apply" pairName="Photos" navigate={navigate} /></I18nProvider>);
    await vi.waitFor(() => expect(api.apply).toHaveBeenCalled());
    expect(handlers['apply-done']).toBeDefined();
    act(() => handlers['apply-progress']({ bytes_done: 512, bytes_total: 1024, changes_done: 1, changes_total: 4, current: 'a/b.jpg' }));
    expect(screen.getByText(/1 of 4 changes/)).toBeInTheDocument();
    expect(screen.getByText('Now: a/b.jpg')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Pause' }));
    expect(api.pause).toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Resume' })).toBeInTheDocument();
    const run = { pairId: 'p1', report: { results: [], stopped: null, trash_run: null } };
    act(() => handlers['apply-done']({ ok: run, error: null }));
    expect(navigate).toHaveBeenCalledWith({ name: 'result', run, pairName: 'Photos' });
  });

  it('retry mode calls retry_failed; a refused start shows the error', async () => {
    vi.mocked(api.retryFailed).mockRejectedValueOnce({ code: 'drive.notConnected', params: { label: 'USB' } });
    render(<I18nProvider lang="en"><Applying mode="retry" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    expect(await screen.findByRole('alert')).toHaveTextContent('USB is not connected.');
  });

  it('unmounting_before_listeners_resolve_never_starts_an_apply', async () => {
    const unlisten = [vi.fn(), vi.fn()];
    const resolvers: (() => void)[] = [];
    vi.mocked(onEvent).mockImplementation((() => {
      const off = unlisten[resolvers.length];
      return new Promise((res) => { resolvers.push(() => res(off)); });
    }) as never);
    const { unmount } = render(<I18nProvider lang="en"><Applying mode="apply" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    unmount();
    resolvers[0]();
    await vi.waitFor(() => expect(unlisten[0]).toHaveBeenCalled());
    expect(resolvers.length).toBe(1);
    expect(api.apply).not.toHaveBeenCalled();
    expect(api.retryFailed).not.toHaveBeenCalled();
  });

  it('pause_failure_keeps_the_button_and_shows_the_error', async () => {
    vi.mocked(api.pause).mockRejectedValueOnce({ code: 'drive.notConnected', params: { label: 'USB' } });
    render(<I18nProvider lang="en"><Applying mode="apply" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await vi.waitFor(() => expect(api.apply).toHaveBeenCalled());
    await userEvent.click(screen.getByRole('button', { name: 'Pause' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('USB is not connected.');
    expect(screen.getByRole('button', { name: 'Pause' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Resume' })).not.toBeInTheDocument();
  });

  it('cancel_failure_is_shown', async () => {
    vi.mocked(api.cancel).mockRejectedValueOnce({ code: 'drive.notConnected', params: { label: 'USB' } });
    render(<I18nProvider lang="en"><Applying mode="apply" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await vi.waitFor(() => expect(api.apply).toHaveBeenCalled());
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('USB is not connected.');
  });
});
