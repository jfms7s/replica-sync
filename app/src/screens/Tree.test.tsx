import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';
import type { ChildrenPage, NodeView } from '../api';

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

const page = (nodes: NodeView[], total = nodes.length): ChildrenPage => ({ nodes, total });

describe('Tree', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.treeChildren).mockImplementation(async (p) => page(p === '' ? [folder('a', 'on')] : [file]));
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
    expect(api.treeChildren).toHaveBeenCalledWith('a', 'name');
    await userEvent.keyboard(' ');
    expect(api.toggle).toHaveBeenCalledWith('a');
    await waitFor(() => expect(onSummary).toHaveBeenCalled());
  });

  it('marks a partly selected folder as mixed and a skipped-only one as disabled', async () => {
    vi.mocked(api.treeChildren).mockResolvedValue(page([folder('m', 'mixed'), folder('s', 'none')]));
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={null} /></I18nProvider>);
    expect(await screen.findByRole('treeitem', { name: /m/ })).toHaveAttribute('aria-checked', 'mixed');
    expect(screen.getByRole('checkbox', { name: 's' })).toBeDisabled();
  });

  it('reexpanding_a_collapsed_folder_after_toggling_it_refetches_it', async () => {
    vi.mocked(api.toggle).mockResolvedValue({} as never);
    vi.mocked(api.treeChildren).mockImplementation(async (p) =>
      page(p === '' ? [folder('a', 'on'), folder('b', 'on')] : [file]));
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={null} /></I18nProvider>);
    const a = await screen.findByRole('treeitem', { name: /^a$/ });
    a.focus();
    await userEvent.keyboard('{ArrowRight}');
    await screen.findByText('x.txt');
    await userEvent.keyboard('{ArrowLeft}');
    await userEvent.keyboard(' ');
    await waitFor(() => expect(api.toggle).toHaveBeenCalledWith('a'));
    const calls = (p: string) => vi.mocked(api.treeChildren).mock.calls.filter(([q]) => q === p).length;
    await waitFor(() => expect(calls('')).toBe(2));
    const before = calls('a');
    screen.getByRole('treeitem', { name: /^a$/ }).focus();
    await userEvent.keyboard('{ArrowRight}');
    await waitFor(() => expect(calls('a')).toBe(before + 1));
  });

  it('a toggle refetches only the folders above it and keeps every other page', async () => {
    vi.mocked(api.toggle).mockResolvedValue({} as never);
    const c: NodeView = { ...file, name: 'c.txt', path: 'a/b/c.txt' };
    vi.mocked(api.treeChildren).mockImplementation(async (p) =>
      page(p === '' ? [folder('a', 'on'), folder('z', 'on')]
        : p === 'a' ? [{ ...folder('b', 'on'), path: 'a/b' }]
          : p === 'a/b' ? [c]
            : [{ ...file, name: 'y.txt', path: 'z/y.txt' }]));
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={['a', 'a/b', 'z']} /></I18nProvider>);
    await screen.findByText('c.txt');
    await screen.findByText('y.txt');
    vi.mocked(api.treeChildren).mockClear();
    await userEvent.click(screen.getByRole('checkbox', { name: 'c.txt' }));
    await waitFor(() => expect(api.toggle).toHaveBeenCalledWith('a/b/c.txt'));
    await waitFor(() => expect(api.treeChildren).toHaveBeenCalledTimes(3));
    expect(vi.mocked(api.treeChildren).mock.calls.map(([p]) => p).sort()).toEqual(['', 'a', 'a/b']);
    expect(screen.getByText('y.txt')).toBeInTheDocument();
  });

  it('draws only the rows in view of a huge page and scrolls the focused row into view', async () => {
    const many = Array.from({ length: 5000 }, (_, i): NodeView =>
      ({ ...file, name: `f${i}.txt`, path: `f${i}.txt` }));
    vi.mocked(api.treeChildren).mockResolvedValue(page(many));
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={null} /></I18nProvider>);
    const first = await screen.findByRole('treeitem', { name: 'f0.txt' });
    expect(screen.getAllByRole('treeitem').length).toBeLessThanOrEqual(100);
    expect(screen.queryByRole('treeitem', { name: 'f4999.txt' })).not.toBeInTheDocument();
    first.focus();
    for (let i = 0; i < 40; i++) await userEvent.keyboard('{ArrowDown}');
    await waitFor(() => expect(screen.getByRole('treeitem', { name: 'f40.txt' })).toHaveFocus());
    expect(screen.getByRole('tree').scrollTop).toBeGreaterThan(0);
    expect(screen.queryByRole('treeitem', { name: 'f0.txt' })).not.toBeInTheDocument();
    expect(screen.getAllByRole('treeitem').length).toBeLessThanOrEqual(100);
  });

  it('keeps one row tabbable after the focused row scrolled away', async () => {
    const many = Array.from({ length: 500 }, (_, i): NodeView =>
      ({ ...file, name: `f${i}.txt`, path: `f${i}.txt` }));
    vi.mocked(api.treeChildren).mockResolvedValue(page(many));
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={null} /></I18nProvider>);
    await screen.findByRole('treeitem', { name: 'f0.txt' });
    const tree = screen.getByRole('tree');
    act(() => tree.scrollTo({ top: 24 * 300 }));
    await waitFor(() => expect(screen.queryByRole('treeitem', { name: 'f0.txt' })).not.toBeInTheDocument());
    expect(screen.getAllByRole('treeitem').filter((el) => el.tabIndex === 0)).toHaveLength(1);
  });

  it('says how many children of a huge folder are not shown', async () => {
    vi.mocked(api.treeChildren).mockImplementation(async (p) =>
      p === '' ? page([folder('a', 'on')]) : page([file], 2500));
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={null} /></I18nProvider>);
    expect(screen.queryByText(/more not shown/)).not.toBeInTheDocument();
    const a = await screen.findByRole('treeitem', { name: /^a$/ });
    a.focus();
    await userEvent.keyboard('{ArrowRight}');
    expect(await screen.findByText(
      "2,499 more not shown — use the folder's checkbox to include or exclude them all.")).toBeInTheDocument();
  });

  it('keeps a row tabbable after a mouse collapse hides the focused child, and ArrowLeft on a file goes to its folder', async () => {
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={null} /></I18nProvider>);
    const a = await screen.findByRole('treeitem', { name: /a/ });
    a.focus();
    await userEvent.keyboard('{ArrowRight}');
    const x = await screen.findByRole('treeitem', { name: 'x.txt' });
    await userEvent.keyboard('{ArrowDown}');
    await waitFor(() => expect(x).toHaveFocus());
    await userEvent.keyboard('{ArrowLeft}');
    await waitFor(() => expect(screen.getByRole('treeitem', { name: /a/ })).toHaveFocus());
    await userEvent.keyboard('{ArrowDown}');
    await waitFor(() => expect(screen.getByRole('treeitem', { name: 'x.txt' })).toHaveFocus());
    await userEvent.click(screen.getByRole('button', { name: /a\// }));
    await waitFor(() => expect(screen.queryByRole('treeitem', { name: 'x.txt' })).not.toBeInTheDocument());
    (document.activeElement as HTMLElement | null)?.blur();
    await userEvent.tab(); // the sort button above the tree
    await userEvent.tab();
    expect(screen.getByRole('treeitem', { name: /a/ })).toHaveFocus();
  });

  it('does not open "Show them" folders past their parent\'s first page, and says how many', async () => {
    vi.mocked(api.treeChildren).mockImplementation(async (p) =>
      page(p === '' ? [folder('a', 'on')] : p === 'a' ? [file] : [], p === 'a' ? 3000 : undefined));
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={['a', 'a/far']} /></I18nProvider>);
    expect(await screen.findByText(/1 folders with deletes are past/)).toBeInTheDocument();
    expect(screen.getByRole('treeitem', { name: 'a' })).toHaveAttribute('aria-expanded', 'true');
  });

  it('ArrowDown and ArrowUp walk across an open folder into the next one', async () => {
    vi.mocked(api.treeChildren).mockImplementation(async (p) =>
      page(p === '' ? [folder('a', 'on'), folder('z', 'on')] : [{ ...file, name: 'x.txt', path: `${p}/x.txt` }]));
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={null} /></I18nProvider>);
    const a = await screen.findByRole('treeitem', { name: 'a' });
    a.focus();
    await userEvent.keyboard('{ArrowRight}');
    await screen.findByRole('treeitem', { name: 'x.txt' });
    await userEvent.keyboard('{ArrowDown}');
    await waitFor(() => expect(screen.getByRole('treeitem', { name: 'x.txt' })).toHaveFocus());
    await userEvent.keyboard('{ArrowDown}');
    await waitFor(() => expect(screen.getByRole('treeitem', { name: 'z' })).toHaveFocus());
    await userEvent.keyboard('{ArrowUp}{ArrowUp}');
    await waitFor(() => expect(a).toHaveFocus());
  });

  it('collapses a folder with the mouse', async () => {
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={null} /></I18nProvider>);
    const open = await screen.findByRole('button', { name: /a\// });
    await userEvent.click(open);
    expect(await screen.findByText('x.txt')).toBeInTheDocument();
    expect(screen.getByRole('treeitem', { name: 'a' })).toHaveAttribute('aria-expanded', 'true');
    await userEvent.click(open);
    expect(screen.queryByText('x.txt')).not.toBeInTheDocument();
    expect(screen.getByRole('treeitem', { name: 'a' })).toHaveAttribute('aria-expanded', 'false');
  });

  it('shows a "more" row under a folder with more children than were sent', async () => {
    vi.mocked(api.treeChildren).mockImplementation(async (p) => page(p === '' ? [folder('a', 'on')] : [file], p === '' ? 1 : 2500));
    render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={['a']} /></I18nProvider>);
    expect(await screen.findByText(/2,499 more not shown — use the folder's checkbox/)).toBeInTheDocument();
  });

  describe('sort by size', () => {
    const big: NodeView = { ...file, name: 'big.bin', path: 'a/big.bin', bytesToCopy: 100 };
    const small: NodeView = { ...file, name: 'small.txt', path: 'a/small.txt', bytesToCopy: 1 };
    const serve = () =>
      vi.mocked(api.treeChildren).mockImplementation(async (p, order) =>
        page(p === '' ? [folder('a', 'on'), folder('b', 'on')]
          : p === 'a' ? (order === 'size' ? [big, small] : [small, big])
            : [file]));
    const rowNames = () => screen.getAllByRole('treeitem').map((el) => el.getAttribute('aria-label'));

    it('asks Rust for that folder only in size order and redraws it largest first', async () => {
      serve();
      render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={['a', 'b']} /></I18nProvider>);
      await screen.findByText('small.txt');
      expect(rowNames().filter((n) => n === 'small.txt' || n === 'big.bin')).toEqual(['small.txt', 'big.bin']);
      vi.mocked(api.treeChildren).mockClear();
      await userEvent.click(screen.getByRole('button', { name: 'Largest first in a' }));
      await waitFor(() => expect(api.treeChildren).toHaveBeenCalledWith('a', 'size'));
      await waitFor(() =>
        expect(rowNames().filter((n) => n === 'small.txt' || n === 'big.bin')).toEqual(['big.bin', 'small.txt']));
      expect(api.treeChildren).toHaveBeenCalledTimes(1);
      expect(screen.getByRole('button', { name: 'Largest first in a' })).toHaveAttribute('aria-pressed', 'true');
      expect(screen.getByRole('button', { name: 'Largest first in b' })).toHaveAttribute('aria-pressed', 'false');
    });

    it('keeps the folder in size order across a tick and flips back to name order', async () => {
      serve();
      vi.mocked(api.toggle).mockResolvedValue({} as never);
      render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={['a']} /></I18nProvider>);
      await screen.findByText('small.txt');
      await userEvent.click(screen.getByRole('button', { name: 'Largest first in a' }));
      await waitFor(() => expect(rowNames()).toContain('big.bin'));
      vi.mocked(api.treeChildren).mockClear();
      await userEvent.click(screen.getByRole('checkbox', { name: 'small.txt' }));
      await waitFor(() => expect(api.toggle).toHaveBeenCalledWith('a/small.txt'));
      await waitFor(() => expect(api.treeChildren).toHaveBeenCalledWith('a', 'size'));
      expect(vi.mocked(api.treeChildren).mock.calls.filter(([p]) => p === 'a').every(([, o]) => o === 'size')).toBe(true);
      await userEvent.click(screen.getByRole('button', { name: 'Largest first in a' }));
      await waitFor(() => expect(api.treeChildren).toHaveBeenLastCalledWith('a', 'name'));
    });

    it('Space on the sort button does not tick the folder', async () => {
      serve();
      vi.mocked(api.toggle).mockResolvedValue({} as never);
      render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={['a']} /></I18nProvider>);
      const btn = await screen.findByRole('button', { name: 'Largest first in a' });
      btn.focus();
      await userEvent.keyboard(' ');
      await waitFor(() => expect(api.treeChildren).toHaveBeenCalledWith('a', 'size'));
      expect(api.toggle).not.toHaveBeenCalled();
    });

    it('S on a focused open folder row flips its sort, and does nothing on a closed one or a file', async () => {
      serve();
      render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={['a']} /></I18nProvider>);
      await screen.findByText('small.txt');
      screen.getByRole('treeitem', { name: 'b' }).focus();
      await userEvent.keyboard('s');
      screen.getByRole('treeitem', { name: 'small.txt' }).focus();
      await userEvent.keyboard('s');
      expect(vi.mocked(api.treeChildren).mock.calls.some(([, o]) => o === 'size')).toBe(false);
      const a = screen.getByRole('treeitem', { name: 'a' });
      expect(a).toHaveAttribute('aria-keyshortcuts', 'S');
      a.focus();
      await userEvent.keyboard('s');
      await waitFor(() => expect(api.treeChildren).toHaveBeenCalledWith('a', 'size'));
      await waitFor(() =>
        expect(rowNames().filter((n) => n === 'small.txt' || n === 'big.bin')).toEqual(['big.bin', 'small.txt']));
      a.focus();
      await userEvent.keyboard('S');
      await waitFor(() => expect(api.treeChildren).toHaveBeenLastCalledWith('a', 'name'));
    });

    it('sorts the root with the button above the tree', async () => {
      serve();
      render(<I18nProvider lang="en"><Tree onSummary={vi.fn()} expandRequest={null} /></I18nProvider>);
      await screen.findByRole('treeitem', { name: 'a' });
      await userEvent.click(screen.getByRole('button', { name: 'Largest first' }));
      await waitFor(() => expect(api.treeChildren).toHaveBeenCalledWith('', 'size'));
    });
  });
});
