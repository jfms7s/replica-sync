import { act, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';

const handlers: Record<string, (p: unknown) => void> = {};
vi.mock('../api', () => ({
  api: { startScan: vi.fn().mockResolvedValue(undefined), cancelScan: vi.fn(), listPairs: vi.fn() },
  onEvent: vi.fn(async (name: string, cb: (p: unknown) => void) => { handlers[name] = cb; return () => {}; }),
}));
import { api, onEvent } from '../api';
import Scanning from './Scanning';

describe('Scanning', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    for (const k of Object.keys(handlers)) delete handlers[k]; // no stale listeners from a previous test
    // A test below swaps in listeners that never resolve; put the default back.
    vi.mocked(onEvent).mockImplementation((async (name: string, cb: (p: unknown) => void) => { handlers[name] = cb; return () => {}; }) as never);
    vi.mocked(api.startScan).mockResolvedValue(undefined);
  });

  it('listens before starting, shows progress, then goes to the preview', async () => {
    const navigate = vi.fn();
    render(<I18nProvider lang="en"><Scanning pairId="p1" pairName="Photos" navigate={navigate} /></I18nProvider>);
    await vi.waitFor(() => expect(api.startScan).toHaveBeenCalledWith('p1', undefined));
    expect(handlers['scan-done']).toBeDefined(); // registered before startScan resolved
    act(() => handlers['scan-progress']({
      source: { files: 1200, bytes: 2048, current: 'Photos/2026' },
      replica: { files: 3, bytes: 0, current: '' }, approxFiles: 2400,
    }));
    expect(screen.getByText(/1,200 files/)).toBeInTheDocument();
    const summary = { pairId: 'p1' };
    act(() => handlers['scan-done']({ ok: summary, error: null }));
    expect(navigate).toHaveBeenCalledWith({ name: 'preview', summary });
  });

  it('scans against the backup disk it was given', async () => {
    render(<I18nProvider lang="en"><Scanning pairId="p1" pairName="Photos" replica={1} navigate={vi.fn()} /></I18nProvider>);
    await vi.waitFor(() => expect(api.startScan).toHaveBeenCalledWith('p1', 1));
  });

  it('shows a translated error with Edit pair for nested folders', async () => {
    render(<I18nProvider lang="en"><Scanning pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await vi.waitFor(() => expect(handlers['scan-done']).toBeDefined());
    act(() => handlers['scan-done']({ ok: null, error: { code: 'scan.nested', params: {} } }));
    expect(screen.getByRole('alert')).toHaveTextContent('one is inside the other');
    expect(screen.getByRole('button', { name: 'Edit pair' })).toBeInTheDocument();
  });

  it('unmounting_before_listeners_resolve_never_starts_a_scan', async () => {
    const unlisten = [vi.fn(), vi.fn()];
    const resolvers: (() => void)[] = [];
    vi.mocked(onEvent).mockImplementation((() => {
      const off = unlisten[resolvers.length];
      return new Promise((res) => { resolvers.push(() => res(off)); });
    }) as never);
    const { unmount } = render(<I18nProvider lang="en"><Scanning pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    unmount();
    resolvers[0]();
    await vi.waitFor(() => expect(unlisten[0]).toHaveBeenCalled());
    expect(resolvers.length).toBe(1);
    expect(api.startScan).not.toHaveBeenCalled();
  });

  it('goes back to the pairs list when the start itself is cancelled', async () => {
    vi.mocked(api.startScan).mockRejectedValueOnce({ code: 'scan.cancelled', params: {} });
    const navigate = vi.fn();
    render(<I18nProvider lang="en"><Scanning pairId="p1" pairName="Photos" navigate={navigate} /></I18nProvider>);
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith({ name: 'pairs' }));
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('shows an error when Edit pair cannot load the pair', async () => {
    vi.mocked(api.startScan).mockRejectedValueOnce({ code: 'scan.nested', params: {} });
    vi.mocked(api.listPairs).mockRejectedValueOnce({ code: 'io', params: { detail: 'store gone' } });
    render(<I18nProvider lang="en"><Scanning pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    const edit = await screen.findByRole('button', { name: 'Edit pair' });
    edit.click();
    expect(await screen.findByText(/store gone/)).toBeInTheDocument();
  });
});
