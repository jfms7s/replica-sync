// app/src/screens/PairEditor.test.tsx
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';

vi.mock('../api', () => ({
  api: { savePair: vi.fn(), deletePair: vi.fn(), builtinRules: vi.fn(), getSettings: vi.fn(), addReplica: vi.fn(), relinkReplica: vi.fn(), removeReplica: vi.fn() },
  pickFolder: vi.fn(),
}));
import { api, pickFolder } from '../api';
import PairEditor from './PairEditor';

// What save_pair answers with: the pair as stored.
const SAVED = {
  id: 'p1', name: 'Photos', source: { volume_id: 'a', rel_path: 'Photos', label: 'Internal' },
  replicas: [{ side: { volume_id: 'b', rel_path: 'Backup', label: 'USB' }, last_sync: null, last_scan_files: null }],
  user_rules: [], trash_days: 30,
} as never;

describe('PairEditor', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.builtinRules).mockResolvedValue(['Thumbs.db']);
    vi.mocked(api.getSettings).mockResolvedValue({ settings: { language: 'auto', defaultTrashDays: 21 }, resolvedLanguage: 'en' });
  });

  it('shows a translated inline error and the same-drive tick when the engine asks for it', async () => {
    vi.mocked(pickFolder).mockResolvedValueOnce('/a').mockResolvedValueOnce('/b');
    vi.mocked(api.savePair)
      .mockRejectedValueOnce({ code: 'pair.sameVolume', params: {} })
      .mockResolvedValueOnce(SAVED);
    const navigate = vi.fn();
    render(<I18nProvider lang="en"><PairEditor pair={null} navigate={navigate} /></I18nProvider>);
    await userEvent.type(screen.getByLabelText('Name'), 'Photos');
    const browse = screen.getAllByRole('button', { name: 'Browse…' });
    await userEvent.click(browse[0]);
    await userEvent.click(browse[1]);
    expect(pickFolder).toHaveBeenNthCalledWith(1, 'Folder to back up (source)');
    expect(pickFolder).toHaveBeenNthCalledWith(2, 'Backup folder');
    expect(await screen.findByDisplayValue('21')).toBeInTheDocument(); // default trash age from settings
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/same drive/);
    await userEvent.click(screen.getByLabelText(/I understand/));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(api.savePair).toHaveBeenLastCalledWith(
      expect.objectContaining({ name: 'Photos', source: '/a', replica: '/b', allowSameVolume: true, trashDays: 21 }),
    );
    expect(navigate).toHaveBeenCalledWith({ name: 'pairs' });
  });

  it('never sends a trash age of 0 or an empty one', async () => {
    vi.mocked(pickFolder).mockResolvedValueOnce('/a').mockResolvedValueOnce('/b');
    render(<I18nProvider lang="en"><PairEditor pair={null} navigate={vi.fn()} /></I18nProvider>);
    await userEvent.type(screen.getByLabelText('Name'), 'Photos');
    const browse = screen.getAllByRole('button', { name: 'Browse…' });
    await userEvent.click(browse[0]);
    await userEvent.click(browse[1]);
    const days = await screen.findByDisplayValue('21');
    await userEvent.clear(days);
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await userEvent.type(days, '0');
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(api.savePair).not.toHaveBeenCalled();
    await userEvent.clear(days);
    await userEvent.type(days, '7');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(api.savePair).toHaveBeenCalledWith(expect.objectContaining({ trashDays: 7 }));
  });

  it('forgets the same-drive answer when a folder changes', async () => {
    vi.mocked(pickFolder).mockResolvedValueOnce('/a').mockResolvedValueOnce('/b').mockResolvedValueOnce('/c');
    vi.mocked(api.savePair).mockRejectedValueOnce({ code: 'pair.sameVolume', params: {} }).mockResolvedValue(SAVED);
    render(<I18nProvider lang="en"><PairEditor pair={null} navigate={vi.fn()} /></I18nProvider>);
    await userEvent.type(screen.getByLabelText('Name'), 'Photos');
    const browse = screen.getAllByRole('button', { name: 'Browse…' });
    await userEvent.click(browse[0]);
    await userEvent.click(browse[1]);
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await userEvent.click(await screen.findByLabelText(/same drive/));
    await userEvent.click(browse[1]);
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(api.savePair).toHaveBeenLastCalledWith(expect.objectContaining({ replica: '/c', allowSameVolume: false }));
  });

  it('shows a folder-picker failure', async () => {
    vi.mocked(pickFolder).mockRejectedValue({ code: 'io', params: { detail: 'no dialog' } });
    render(<I18nProvider lang="en"><PairEditor pair={null} navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(screen.getAllByRole('button', { name: 'Browse…' })[0]);
    expect(await screen.findByRole('alert')).toHaveTextContent('no dialog');
  });

  it('says when the default trash age could not be read', async () => {
    vi.mocked(api.getSettings).mockRejectedValue({ code: 'io', params: { detail: 'x' } });
    vi.spyOn(console, 'error').mockImplementation(() => {});
    render(<I18nProvider lang="en"><PairEditor pair={null} navigate={vi.fn()} /></I18nProvider>);
    expect(await screen.findByText(/default could not be read/)).toBeInTheDocument();
  });

  const existing = {
    pair: {
      id: 'p1', name: 'Photos',
      source: { volume_id: 'a', rel_path: 'Photos', label: 'Internal' },
      replicas: [{ side: { volume_id: 'b', rel_path: 'Backup', label: 'USB' }, last_sync: null, last_scan_files: null }],
      user_rules: ['*.tmp'], trash_days: 14,
    },
    sourceConnected: true, replicas: [], dueReplica: 0,
  };

  it('deletes a pair only after confirming', async () => {
    vi.mocked(api.deletePair).mockResolvedValue(undefined as never);
    const navigate = vi.fn();
    render(<I18nProvider lang="en"><PairEditor pair={existing} navigate={navigate} /></I18nProvider>);
    await userEvent.click(screen.getByRole('button', { name: 'Delete pair' }));
    expect(api.deletePair).not.toHaveBeenCalled();
    const dialog = screen.getByRole('dialog', { name: 'Delete pair' });
    expect(dialog).toHaveTextContent('Delete the pair Photos?');
    await userEvent.click(screen.getAllByRole('button', { name: 'Delete pair' }).at(-1)!);
    expect(api.deletePair).toHaveBeenCalledWith('p1');
    expect(navigate).toHaveBeenCalledWith({ name: 'pairs' });
  });

  it('editing without changing folders sends both as null', async () => {
    vi.mocked(api.savePair).mockResolvedValue(SAVED);
    render(<I18nProvider lang="en"><PairEditor pair={existing} navigate={vi.fn()} /></I18nProvider>);
    const name = screen.getByLabelText('Name');
    await userEvent.clear(name);
    await userEvent.type(name, 'Pictures');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(api.savePair).toHaveBeenCalledWith(expect.objectContaining({
      id: 'p1', name: 'Pictures', source: null, replica: null, userRules: ['*.tmp'], trashDays: 14,
    }));
  });

  describe('backup disks', () => {
    const sideOf = (label: string, rel = 'Backup') => ({ side: { volume_id: label, rel_path: rel, label }, last_sync: null, last_scan_files: null });
    const savedPair = (disks: string[]) => ({
      ...existing.pair, replicas: disks.map((d) => sideOf(d)),
    });
    const twoDisks = { ...existing, pair: savedPair(['HOME', 'RELATIVE']) };

    it('shows the advice and lets a second disk be added, saved after the pair', async () => {
      vi.mocked(pickFolder).mockResolvedValue('/media/relative/Backup');
      vi.mocked(api.savePair).mockResolvedValue(savedPair(['USB']) as never);
      vi.mocked(api.addReplica).mockResolvedValue(savedPair(['USB', 'RELATIVE']) as never);
      const navigate = vi.fn();
      render(<I18nProvider lang="en"><PairEditor pair={existing} navigate={navigate} /></I18nProvider>);
      expect(screen.getByText(/not theft, fire or ransomware/)).toBeInTheDocument();
      await userEvent.click(screen.getByRole('button', { name: 'Add a second backup disk' }));
      expect(screen.getByText('Disk A: backup folder')).toBeInTheDocument();
      const field = screen.getByText('Disk B: backup folder').closest('.panel') as HTMLElement;
      await userEvent.click(within(field).getByRole('button', { name: 'Browse…' }));
      expect(pickFolder).toHaveBeenCalledWith('Disk B: backup folder');
      await userEvent.click(screen.getByRole('button', { name: 'Save' }));
      expect(api.savePair).toHaveBeenCalledWith(expect.objectContaining({ id: 'p1', replica: null }));
      expect(api.addReplica).toHaveBeenCalledWith('p1', '/media/relative/Backup', false);
      expect(navigate).toHaveBeenCalledWith({ name: 'pairs' });
    });

    it('without choosing a folder for the second disk, adds nothing', async () => {
      vi.mocked(api.savePair).mockResolvedValue(savedPair(['USB']) as never);
      render(<I18nProvider lang="en"><PairEditor pair={existing} navigate={vi.fn()} /></I18nProvider>);
      await userEvent.click(screen.getByRole('button', { name: 'Add a second backup disk' }));
      await userEvent.click(screen.getByRole('button', { name: 'Save' }));
      await vi.waitFor(() => expect(api.savePair).toHaveBeenCalled());
      expect(api.addReplica).not.toHaveBeenCalled();
    });

    it('re-links the second disk by index when a folder is chosen for it', async () => {
      vi.mocked(pickFolder).mockResolvedValue('/media/relative/New');
      vi.mocked(api.savePair).mockResolvedValue(twoDisks.pair as never);
      vi.mocked(api.relinkReplica).mockResolvedValue(twoDisks.pair as never);
      render(<I18nProvider lang="en"><PairEditor pair={twoDisks} navigate={vi.fn()} /></I18nProvider>);
      expect(screen.getByText('Current: HOME › Backup')).toBeInTheDocument();
      expect(screen.getByText('Current: RELATIVE › Backup')).toBeInTheDocument();
      const field = screen.getByText('Disk B: backup folder').closest('.panel') as HTMLElement;
      await userEvent.click(within(field).getByRole('button', { name: 'Change…' }));
      await userEvent.click(screen.getByRole('button', { name: 'Save' }));
      expect(api.savePair).toHaveBeenCalledWith(expect.objectContaining({ replica: null }));
      expect(api.relinkReplica).toHaveBeenCalledWith('p1', 1, '/media/relative/New', false);
      expect(api.addReplica).not.toHaveBeenCalled();
    });

    it('a failed second step leaves the pair saved and a retry does not send the first folders again', async () => {
      vi.mocked(pickFolder).mockResolvedValueOnce('/home/me/Photos').mockResolvedValueOnce('/media/a').mockResolvedValueOnce('/media/b');
      vi.mocked(api.savePair).mockResolvedValue(savedPair(['USB']) as never);
      vi.mocked(api.addReplica)
        .mockRejectedValueOnce({ code: 'drive.notConnected', params: { side: 'replica', label: 'RELATIVE' } })
        .mockResolvedValueOnce(savedPair(['USB', 'RELATIVE']) as never);
      render(<I18nProvider lang="en"><PairEditor pair={null} navigate={vi.fn()} /></I18nProvider>);
      await userEvent.type(screen.getByLabelText('Name'), 'Photos');
      const browse = screen.getAllByRole('button', { name: 'Browse…' });
      await userEvent.click(browse[0]);
      await userEvent.click(browse[1]);
      await userEvent.click(screen.getByRole('button', { name: 'Add a second backup disk' }));
      const field = screen.getByText('Disk B: backup folder').closest('.panel') as HTMLElement;
      await userEvent.click(within(field).getByRole('button', { name: 'Browse…' }));
      await userEvent.click(screen.getByRole('button', { name: 'Save' }));
      expect(await screen.findByRole('alert')).toBeInTheDocument();
      await userEvent.click(screen.getByRole('button', { name: 'Save' }));
      await vi.waitFor(() => expect(api.addReplica).toHaveBeenCalledTimes(2));
      expect(api.savePair).toHaveBeenNthCalledWith(1, expect.objectContaining({ id: null, source: '/home/me/Photos', replica: '/media/a' }));
      expect(api.savePair).toHaveBeenNthCalledWith(2, expect.objectContaining({ id: 'p1', source: null, replica: null }));
    });

    it('removes a disk only after confirming', async () => {
      vi.mocked(api.removeReplica).mockResolvedValue(savedPair(['RELATIVE']) as never);
      render(<I18nProvider lang="en"><PairEditor pair={twoDisks} navigate={vi.fn()} /></I18nProvider>);
      await userEvent.click(screen.getByRole('button', { name: 'Remove Disk A' }));
      expect(api.removeReplica).not.toHaveBeenCalled();
      const dialog = screen.getByRole('dialog', { name: 'Remove Disk A' });
      expect(dialog).toHaveTextContent('Remove Disk A (HOME) from this pair?');
      await userEvent.click(within(dialog).getByRole('button', { name: 'Remove disk' }));
      expect(api.removeReplica).toHaveBeenCalledWith('p1', 0);
      expect(await screen.findByText('Backup folder')).toBeInTheDocument(); // back to a single disk
      expect(screen.queryByRole('button', { name: /Remove Disk/ })).not.toBeInTheDocument();
    });

    it('offers no removal while the pair has one disk', () => {
      render(<I18nProvider lang="en"><PairEditor pair={existing} navigate={vi.fn()} /></I18nProvider>);
      expect(screen.queryByRole('button', { name: /Remove Disk/ })).not.toBeInTheDocument();
    });
  });
});
