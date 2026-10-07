import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';
import type { NodeView } from '../api';

vi.mock('../api', () => ({ api: { treeChildren: vi.fn(), toggle: vi.fn() } }));
import { api } from '../api';
import Tree from './Tree';

const folder = (name: string, tick: NodeView['tick']): NodeView => ({
  name, path: name, isFolder: true, changes: [], tick, bytesToCopy: 30,
  counts: { create: 2, update: 0, delete: 1, moves: 0, folders: 0, skipped: 0 },
});
const file: NodeView = {
  name: 'x.txt', path: 'a/x.txt', isFolder: false, tick: 'on', bytesToCopy: 3,
  counts: { create: 1, update: 0, delete: 0, moves: 0, folders: 0, skipped: 0 },
  changes: [{ id: 0, change: { type: 'create', path: 'a/x.txt', size: 3, mtime_ns: 0 } }],
};

describe('Tree', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.treeChildren).mockImplementation(async (p) => (p === '' ? [folder('a', 'on')] : [file]));
  });

  it('shows folder counts, expands lazily and toggles with the keyboard', async () => {
    vi.mocked(api.toggle).mockResolvedValue({} as never);
    const onSummary = vi.fn();
    render(<I18nProvider lang="en"><Tree onSummary={onSummary} expandRequest={null} /></I18nProvider>);
    const a = await screen.findByRole('treeitem', { name: /a/ });
    expect(a).toHaveTextContent('+2');
    expect(a).toHaveTextContent('−1');
    expect(api.treeChildren).toHaveBeenCalledTimes(1);
    a.focus();
    await userEvent.keyboard('{ArrowRight}');
    expect(await screen.findByText('x.txt')).toBeInTheDocument();
    expect(api.treeChildren).toHaveBeenCalledWith('a');
    await userEvent.keyboard(' ');
    expect(api.toggle).toHaveBeenCalledWith('a');
    await waitFor(() => expect(onSummary).toHaveBeenCalled());
  });

  it('marks a partly selected folder as mixed and a skipped-only one as disabled', async () => {
    vi.mocked(api.treeChildren).mockResolvedValue([folder('m', 'mixed'), folder('s', 'none')]);
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={null} /></I18nProvider>);
    expect(await screen.findByRole('treeitem', { name: /m/ })).toHaveAttribute('aria-checked', 'mixed');
    expect(screen.getByRole('checkbox', { name: 's' })).toBeDisabled();
  });
});
