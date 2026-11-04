import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';
import type { FindPage, NodeView, PreviewSummary } from '../api';

vi.mock('../api', () => ({ api: { treeFind: vi.fn(), toggle: vi.fn() } }));
import { api } from '../api';
import Find from './Find';

const totals = { creates: 2, updates: 0, moves: 0, deletes: 0, folders: 0, skipped: 0, bytes_to_copy: 8 };
const summary: PreviewSummary = {
  pairId: 'p1', pairName: 'Photos', totals, selected: totals, selectedCount: 2, actionableCount: 2,
  isEmpty: false, guard: null, guardConfirmed: false, shortfall: null,
  source: { files: 2, bytes: 8, elapsed_ms: 1 }, replica: { files: 0, bytes: 0, elapsed_ms: 1 },
  warnings: [],
};

const file = (path: string, tick: NodeView['tick'] = 'on'): NodeView => ({
  name: path.split('/').at(-1)!, path, isFolder: false, tick, bytesToCopy: 7,
  counts: { create: 1, update: 0, delete: 0, moves: 0, folders: 0, skipped: 0 },
  changes: [{ id: 1, change: { type: 'create', path, size: 7, mtime_ns: 0 } }],
});
const found = (nodes: NodeView[], totalMatches = nodes.length): FindPage => ({ nodes, totalMatches });

const setup = (onToggled = vi.fn()) => {
  render(<I18nProvider lang="en"><Find summary={summary} onToggled={onToggled} /></I18nProvider>);
  return onToggled;
};

describe('Find', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('lists the matches for what is typed, with their folder', async () => {
    vi.mocked(api.treeFind).mockResolvedValue(found([file('shots/IMG_2400.jpg')]));
    setup();
    await userEvent.type(screen.getByRole('searchbox', { name: 'Find a file or folder by name' }), 'img_24');
    expect(await screen.findByRole('checkbox', { name: 'shots/IMG_2400.jpg' })).toBeChecked();
    expect(screen.getByText('IMG_2400.jpg')).toBeInTheDocument();
    expect(screen.getByText('shots')).toBeInTheDocument();
    expect(api.treeFind).toHaveBeenLastCalledWith('img_24', 200);
  });

  it('ticking a match calls toggle with its path and hands the new summary on', async () => {
    const next = { ...summary, selectedCount: 1 };
    vi.mocked(api.treeFind).mockResolvedValue(found([file('shots/IMG_2400.jpg')]));
    vi.mocked(api.toggle).mockResolvedValue(next);
    const onToggled = setup();
    await userEvent.type(screen.getByRole('searchbox'), 'IMG_2400');
    await userEvent.click(await screen.findByRole('checkbox', { name: 'shots/IMG_2400.jpg' }));
    expect(api.toggle).toHaveBeenCalledTimes(1);
    expect(api.toggle).toHaveBeenCalledWith('shots/IMG_2400.jpg');
    await waitFor(() => expect(onToggled).toHaveBeenCalledWith(next));
  });

  it('says how many more matches are not listed, and when there are none', async () => {
    vi.mocked(api.treeFind).mockResolvedValueOnce(found([file('a/x1.txt'), file('a/x2.txt')], 1500));
    setup();
    await userEvent.type(screen.getByRole('searchbox'), 'x');
    expect(await screen.findByText(/1,498 more matches not shown/)).toBeInTheDocument();
    vi.mocked(api.treeFind).mockResolvedValue(found([]));
    await userEvent.type(screen.getByRole('searchbox'), 'yz');
    expect(await screen.findByText(/No change has a file or folder name containing that/)).toBeInTheDocument();
  });

  it('asks Rust nothing for a blank query and clears the list when the box is emptied', async () => {
    vi.mocked(api.treeFind).mockResolvedValue(found([file('a/x.txt')]));
    setup();
    const box = screen.getByRole('searchbox');
    await userEvent.type(box, '   ');
    await new Promise((r) => setTimeout(r, 300));
    expect(api.treeFind).not.toHaveBeenCalled();
    await userEvent.clear(box);
    await userEvent.type(box, 'x');
    await screen.findByRole('checkbox', { name: 'a/x.txt' });
    await userEvent.clear(box);
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
  });

  it('a skipped match is listed but cannot be ticked, and a failed toggle is reported', async () => {
    vi.mocked(api.treeFind).mockResolvedValue(found([file('a/link', 'none'), file('a/x.txt')]));
    vi.mocked(api.toggle).mockRejectedValue({ code: 'io', params: { detail: 'boom' } });
    setup();
    await userEvent.type(screen.getByRole('searchbox'), 'a');
    expect(await screen.findByRole('checkbox', { name: 'a/link' })).toBeDisabled();
    await userEvent.click(screen.getByRole('checkbox', { name: 'a/x.txt' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/boom/);
  });
});
