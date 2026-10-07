// app/src/screens/Trash.test.tsx
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';

vi.mock('../api', () => ({
  api: { trashRuns: vi.fn(), trashContents: vi.fn(), restore: vi.fn(), emptyRun: vi.fn() },
}));
import { api } from '../api';
import Trash from './Trash';

const runId = '2026-10-07_143200';

describe('Trash', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.trashRuns).mockResolvedValue([{ id: runId, files: 2, bytes: 2048, unrecorded: 1 }]);
    vi.mocked(api.trashContents).mockResolvedValue({
      items: [{ path: 'a.txt', size: 1024, mtime_ns: 0, reason: 'deleted' }],
      unrecorded: ['lost.bin'],
      bytes: 2048,
    });
  });

  it('restores selected files and asks before replacing', async () => {
    vi.mocked(api.restore)
      .mockRejectedValueOnce({ code: 'trash.conflict', params: { path: 'a.txt' } })
      .mockResolvedValueOnce(1);
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: /2 files/ }));
    expect(await screen.findByText(/Unrecorded/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('checkbox', { name: 'a.txt' }));
    await userEvent.click(screen.getByRole('button', { name: 'Restore selected' }));
    expect(api.restore).toHaveBeenLastCalledWith('p1', runId, ['a.txt'], false);
    expect(await screen.findByRole('dialog', { name: 'Something is in the way' })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Replace' }));
    expect(api.restore).toHaveBeenLastCalledWith('p1', runId, ['a.txt'], true);
    expect(await screen.findByText('Restored: 1')).toBeInTheDocument();
  });

  it('empties a run only after confirming', async () => {
    vi.mocked(api.emptyRun).mockResolvedValue(undefined);
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: 'Empty' }));
    expect(api.emptyRun).not.toHaveBeenCalled();
    await userEvent.click(screen.getAllByRole('button', { name: 'Empty' }).at(-1)!);
    expect(api.emptyRun).toHaveBeenCalledWith('p1', runId);
  });

  it('goes back to the pairs list by default', async () => {
    const navigate = vi.fn();
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={navigate} /></I18nProvider>);
    await userEvent.click(screen.getByRole('button', { name: 'Back' }));
    expect(navigate).toHaveBeenCalledWith({ name: 'pairs' });
  });

  it('goes back to the screen it was opened from', async () => {
    const navigate = vi.fn();
    const back = { name: 'settings' } as const;
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" back={back} navigate={navigate} /></I18nProvider>);
    await userEvent.click(screen.getByRole('button', { name: 'Back' }));
    expect(navigate).toHaveBeenCalledWith(back);
  });
});
