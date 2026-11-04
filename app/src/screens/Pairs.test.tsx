// app/src/screens/Pairs.test.tsx
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';
import type { PairView, ReplicaView } from '../api';

vi.mock('../api', () => ({
  api: { listPairs: vi.fn(), relink: vi.fn(), relinkReplica: vi.fn() },
  pickFolder: vi.fn(),
}));
import { api, pickFolder } from '../api';
import Pairs from './Pairs';

const disk = (index: number, over: Partial<ReplicaView> = {}): ReplicaView => ({
  index, side: { volume_id: 'b', rel_path: 'Backup', label: 'USB' }, connected: true, lastSync: null, lastScanFiles: null, ...over,
});
const view = (over: Partial<PairView> = {}, replicas: ReplicaView[] = [disk(0)]): PairView => ({
  pair: {
    id: 'p1', name: 'Photos',
    source: { volume_id: 'a', rel_path: 'Photos', label: 'Internal' },
    replicas: replicas.map((r) => ({ side: r.side, last_sync: r.lastSync, last_scan_files: r.lastScanFiles })),
    user_rules: [], trash_days: 30,
  },
  sourceConnected: true, replicas, dueReplica: 0, ...over,
});
const home = disk(0, { side: { volume_id: 'b', rel_path: 'Backup', label: 'HOME' } });
const relative = disk(1, { side: { volume_id: 'c', rel_path: 'Backup', label: 'RELATIVE' } });

describe('Pairs', () => {
  beforeEach(() => vi.clearAllMocks());

  it('shows the empty state', async () => {
    vi.mocked(api.listPairs).mockResolvedValue([]);
    render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
    expect(await screen.findByText(/Add your first pair/)).toBeInTheDocument();
  });

  it('disables Sync and offers Re-link when a drive is missing', async () => {
    vi.mocked(api.listPairs).mockResolvedValue([view({}, [disk(0, { connected: false })])]);
    vi.mocked(pickFolder).mockResolvedValue('/media/usb/Backup');
    vi.mocked(api.relink).mockResolvedValue(view().pair);
    render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
    const card = (await screen.findByText('Photos')).closest('article')!;
    expect(within(card).getByText('⚠ USB not connected')).toBeInTheDocument();
    expect(within(card).getByRole('button', { name: 'Sync' })).toBeDisabled();
    await userEvent.click(within(card).getByRole('button', { name: 'Re-link drive' }));
    expect(api.relink).toHaveBeenCalledWith('p1', 'Replica', '/media/usb/Backup', false);
    expect(pickFolder).toHaveBeenCalledWith('Choose the backup folder on USB');
  });

  it('asks before re-linking onto the other side\'s drive, then retries with the flag', async () => {
    vi.mocked(api.listPairs).mockResolvedValue([view({}, [disk(0, { connected: false })])]);
    vi.mocked(pickFolder).mockResolvedValue('/home/me/Backup');
    vi.mocked(api.relink)
      .mockRejectedValueOnce({ code: 'pair.sameVolume', params: {} })
      .mockResolvedValueOnce(view().pair);
    render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
    const card = (await screen.findByText('Photos')).closest('article')!;
    await userEvent.click(within(card).getByRole('button', { name: 'Re-link drive' }));
    expect(api.relink).toHaveBeenLastCalledWith('p1', 'Replica', '/home/me/Backup', false);
    const dialog = await screen.findByRole('dialog', { name: 'Same drive' });
    await userEvent.click(within(dialog).getByRole('button', { name: /I understand/ }));
    expect(api.relink).toHaveBeenLastCalledWith('p1', 'Replica', '/home/me/Backup', true);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('offers one re-link button per drive when both are missing', async () => {
    vi.mocked(api.listPairs).mockResolvedValue([view({ sourceConnected: false }, [disk(0, { connected: false })])]);
    vi.mocked(pickFolder).mockResolvedValue('/media/x');
    vi.mocked(api.relink).mockResolvedValue(view().pair);
    render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
    const card = (await screen.findByText('Photos')).closest('article')!;
    expect(within(card).queryByRole('button', { name: 'Re-link drive' })).not.toBeInTheDocument();
    await userEvent.click(within(card).getByRole('button', { name: 'Re-link source drive' }));
    expect(pickFolder).toHaveBeenLastCalledWith('Choose the source folder on Internal');
    expect(api.relink).toHaveBeenLastCalledWith('p1', 'Source', '/media/x', false);
    await userEvent.click(within(card).getByRole('button', { name: 'Re-link backup drive' }));
    expect(pickFolder).toHaveBeenLastCalledWith('Choose the backup folder on USB');
    expect(api.relink).toHaveBeenLastCalledWith('p1', 'Replica', '/media/x', false);
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
    v.replicas[0].lastSync = { at: '2026-10-07T10:00:00Z', applied: 3, failed: 0, stopped: true };
    vi.mocked(api.listPairs).mockResolvedValue([v]);
    render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
    expect(await screen.findByText(/3 applied, 0 failed · incomplete$/)).toBeInTheDocument();
  });

  it('shows a folder-picker failure, and a later successful re-link clears the error', async () => {
    vi.mocked(api.listPairs).mockResolvedValue([view({}, [disk(0, { connected: false })])]);
    vi.mocked(pickFolder).mockRejectedValueOnce({ code: 'io', params: { detail: 'no dialog' } }).mockResolvedValueOnce('/x');
    vi.mocked(api.relink).mockResolvedValue(view().pair);
    render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
    const card = (await screen.findByText('Photos')).closest('article')!;
    await userEvent.click(within(card).getByRole('button', { name: 'Re-link drive' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('no dialog');
    await userEvent.click(within(card).getByRole('button', { name: 'Re-link drive' }));
    await vi.waitFor(() => expect(api.relink).toHaveBeenCalled());
    await vi.waitFor(() => expect(screen.queryByRole('alert')).not.toBeInTheDocument());
  });

  it('a failed re-link then a successful one leaves no error', async () => {
    vi.mocked(api.listPairs).mockResolvedValue([view({}, [disk(0, { connected: false })])]);
    vi.mocked(pickFolder).mockResolvedValue('/x');
    vi.mocked(api.relink).mockRejectedValueOnce({ code: 'drive.notConnected', params: { label: 'USB' } }).mockResolvedValueOnce(view().pair);
    render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
    const card = (await screen.findByText('Photos')).closest('article')!;
    await userEvent.click(within(card).getByRole('button', { name: 'Re-link drive' }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    await userEvent.click(within(card).getByRole('button', { name: 'Re-link drive' }));
    await vi.waitFor(() => expect(screen.queryByRole('alert')).not.toBeInTheDocument());
  });

  it('re-links the source when only the source drive is missing', async () => {
    vi.mocked(api.listPairs).mockResolvedValue([view({ sourceConnected: false })]);
    vi.mocked(pickFolder).mockResolvedValue('/media/int/Photos');
    vi.mocked(api.relink).mockResolvedValue(view().pair);
    render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
    const card = (await screen.findByText('Photos')).closest('article')!;
    await userEvent.click(within(card).getByRole('button', { name: 'Re-link drive' }));
    expect(pickFolder).toHaveBeenCalledWith('Choose the source folder on Internal');
    expect(api.relink).toHaveBeenCalledWith('p1', 'Source', '/media/int/Photos', false);
  });

  describe('with two backup disks', () => {
    const lastSync = (at: string, applied = 5) => ({ at, applied, failed: 0, stopped: false });

    it('shows each disk with its last sync and whether it is connected, and an away disk is not a warning', async () => {
      const a = { ...home, connected: false, lastSync: lastSync('2026-10-01T10:00:00Z', 7) };
      const b = { ...relative, lastSync: null };
      vi.mocked(api.listPairs).mockResolvedValue([view({ dueReplica: 1 }, [a, b])]);
      render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
      const card = (await screen.findByText('Photos')).closest('article')!;
      const [rowA, rowB] = within(card).getAllByRole('listitem');
      expect(rowA).toHaveTextContent('Disk A: HOME › Backup · away');
      expect(rowA).toHaveTextContent(/Last sync .*: 7 applied, 0 failed/);
      expect(rowB).toHaveTextContent('Disk B: RELATIVE › Backup · connected');
      expect(rowB).toHaveTextContent('Never synced');
      expect(within(card).queryByText(/not connected/)).not.toBeInTheDocument();
      expect(card.querySelector('.warn')).toBeNull();
    });

    it('names the due disk on the Sync button and starts the scan without choosing', async () => {
      vi.mocked(api.listPairs).mockResolvedValue([view({ dueReplica: 1 }, [home, relative])]);
      const navigate = vi.fn();
      render(<I18nProvider lang="en"><Pairs navigate={navigate} /></I18nProvider>);
      await userEvent.click(await screen.findByRole('button', { name: 'Sync to Disk B' }));
      expect(navigate).toHaveBeenCalledWith({ name: 'scanning', pairId: 'p1', pairName: 'Photos' });
    });

    it('syncs to the disk that is plugged in, and warns only when none is', async () => {
      vi.mocked(api.listPairs).mockResolvedValue([view({ dueReplica: 1 }, [{ ...home, connected: false }, relative])]);
      const { unmount } = render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
      expect(await screen.findByRole('button', { name: 'Sync to Disk B' })).toBeEnabled();
      unmount();
      vi.mocked(api.listPairs).mockResolvedValue([view({}, [{ ...home, connected: false }, { ...relative, connected: false }])]);
      render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
      expect(await screen.findByRole('button', { name: /^Sync to / })).toBeDisabled();
      expect(screen.getByText(/None of the backup disks is connected/)).toBeInTheDocument();
    });

    it('opens the trash of the disk that is plugged in', async () => {
      vi.mocked(api.listPairs).mockResolvedValue([view({ dueReplica: 1 }, [{ ...home, connected: false }, relative])]);
      const navigate = vi.fn();
      render(<I18nProvider lang="en"><Pairs navigate={navigate} /></I18nProvider>);
      expect(screen.queryByRole('button', { name: 'Trash (Disk A)' })).not.toBeInTheDocument();
      await userEvent.click(await screen.findByRole('button', { name: 'Trash (Disk B)' }));
      expect(navigate).toHaveBeenCalledWith({
        name: 'trash', pairId: 'p1', pairName: 'Photos', replica: 1, diskName: 'Disk B (RELATIVE)',
      });
    });

    it('re-links only the disk asked for, by its index', async () => {
      vi.mocked(api.listPairs).mockResolvedValue([view({ dueReplica: 0 }, [home, { ...relative, connected: false }])]);
      vi.mocked(pickFolder).mockResolvedValue('/media/rel/Backup');
      vi.mocked(api.relinkReplica).mockResolvedValue(view().pair);
      render(<I18nProvider lang="en"><Pairs navigate={vi.fn()} /></I18nProvider>);
      await userEvent.click(await screen.findByRole('button', { name: 'Re-link Disk B' }));
      expect(pickFolder).toHaveBeenCalledWith('Choose the backup folder on RELATIVE');
      expect(api.relinkReplica).toHaveBeenCalledWith('p1', 1, '/media/rel/Backup', false);
      expect(api.relink).not.toHaveBeenCalled();
      expect(screen.queryByRole('button', { name: 'Re-link Disk A' })).not.toBeInTheDocument();
    });
  });
});
