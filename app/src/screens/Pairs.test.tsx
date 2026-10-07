// app/src/screens/Pairs.test.tsx
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';
import type { PairView } from '../api';

vi.mock('../api', () => ({
  api: { listPairs: vi.fn(), relink: vi.fn() },
  pickFolder: vi.fn(),
}));
import { api, pickFolder } from '../api';
import Pairs from './Pairs';

const view = (over: Partial<PairView> = {}): PairView => ({
  pair: {
    id: 'p1', name: 'Photos',
    source: { volume_id: 'a', rel_path: 'Photos', label: 'Internal' },
    replica: { volume_id: 'b', rel_path: 'Backup', label: 'USB' },
    user_rules: [], trash_days: 30, last_sync: null, last_scan_files: null,
  },
  sourceConnected: true, replicaConnected: true, ...over,
});

describe('Pairs', () => {
  beforeEach(() => vi.clearAllMocks());

  it('shows the empty state', async () => {
    vi.mocked(api.listPairs).mockResolvedValue([]);
    render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
    expect(await screen.findByText(/Add your first pair/)).toBeInTheDocument();
  });

  it('disables Sync and offers Re-link when a drive is missing', async () => {
    vi.mocked(api.listPairs).mockResolvedValue([view({ replicaConnected: false })]);
    vi.mocked(pickFolder).mockResolvedValue('/media/usb/Backup');
    vi.mocked(api.relink).mockResolvedValue(view().pair);
    render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
    const card = (await screen.findByText('Photos')).closest('article')!;
    expect(within(card).getByText('⚠ USB not connected')).toBeInTheDocument();
    expect(within(card).getByRole('button', { name: 'Sync' })).toBeDisabled();
    await userEvent.click(within(card).getByRole('button', { name: 'Re-link drive' }));
    expect(api.relink).toHaveBeenCalledWith('p1', 'Replica', '/media/usb/Backup');
  });

  it('starts a scan from Sync', async () => {
    vi.mocked(api.listPairs).mockResolvedValue([view()]);
    const navigate = vi.fn();
    render(<I18nProvider lang="pt-PT"><Pairs navigate={navigate} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: 'Sincronizar' }));
    expect(navigate).toHaveBeenCalledWith({ name: 'scanning', pairId: 'p1', pairName: 'Photos' });
  });

  it('marks a last sync that stopped early as incomplete', async () => {
    const v = view();
    v.pair.last_sync = { at: '2026-10-07T10:00:00Z', applied: 3, failed: 0, stopped: true };
    vi.mocked(api.listPairs).mockResolvedValue([v]);
    render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
    expect(await screen.findByText(/3 applied, 0 failed · incomplete$/)).toBeInTheDocument();
  });
});
