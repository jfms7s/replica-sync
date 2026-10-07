// app/src/App.test.tsx
import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

vi.mock('./api', () => ({
  api: {
    startupStatus: vi.fn().mockRejectedValue({ code: 'store.unreadable', params: { path: '/data/pairs.json', detail: 'bad json' } }),
    getSettings: vi.fn().mockResolvedValue({ settings: { language: 'auto', defaultTrashDays: 30 }, resolvedLanguage: 'en' }),
    listPairs: vi.fn().mockResolvedValue([]),
    openDataFolder: vi.fn(),
  },
  onEvent: vi.fn().mockResolvedValue(() => {}),
}));
import App from './App';

describe('App', () => {
  it('shows_the_blocking_screen_when_the_store_is_unreadable', async () => {
    render(<App />);
    expect(await screen.findByText("replica-sync can't read its saved pairs")).toBeInTheDocument();
    expect(screen.getByText(/\/data\/pairs\.json is damaged/)).toBeInTheDocument();
    expect(screen.queryByText('Sync pairs')).not.toBeInTheDocument();
  });
});
