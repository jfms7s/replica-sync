import { useEffect, useState } from 'react';
import { api, type AppError, type PairView, type PreviewSummary, type RunView } from './api';
import { I18nProvider, type Lang } from './i18n';
import Broken from './screens/Broken';
import Pairs from './screens/Pairs';
import PairEditor from './screens/PairEditor';
import Preview from './screens/Preview';
import Scanning from './screens/Scanning';
import Settings from './screens/Settings';

export type Screen =
  | { name: 'pairs' }
  | { name: 'editor'; pair: PairView | null }
  | { name: 'scanning'; pairId: string; pairName: string }
  | { name: 'preview'; summary: PreviewSummary }
  | { name: 'applying'; mode: 'apply' | 'retry'; pairName: string }
  | { name: 'result'; run: RunView; pairName: string }
  | { name: 'trash'; pairId: string; pairName: string }
  | { name: 'settings' };
export type Navigate = (s: Screen) => void;

export default function App() {
  const [lang, setLang] = useState<Lang>('en');
  const [screen, setScreen] = useState<Screen>({ name: 'pairs' });
  const [broken, setBroken] = useState<AppError | null>(null);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    api.getSettings().then((s) => setLang(s.resolvedLanguage)).catch(() => {});
    api
      .startupStatus()
      .catch((e: AppError) => setBroken(e))
      .finally(() => setReady(true));
  }, []);

  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);

  if (!ready) return null;
  return (
    <I18nProvider lang={lang}>
      {broken ? <Broken error={broken} /> : <Current screen={screen} navigate={setScreen} onLanguage={setLang} />}
    </I18nProvider>
  );
}

function Current({ screen, navigate, onLanguage }: { screen: Screen; navigate: Navigate; onLanguage: (l: Lang) => void }) {
  switch (screen.name) {
    case 'pairs':
      return <Pairs navigate={navigate} />;
    case 'editor':
      return <PairEditor pair={screen.pair} navigate={navigate} />;
    case 'settings':
      return <Settings navigate={navigate} onLanguage={onLanguage} />;
    case 'scanning':
      return <Scanning pairId={screen.pairId} pairName={screen.pairName} navigate={navigate} />;
    case 'preview':
      return <Preview summary={screen.summary} navigate={navigate} />;
    default:
      return <p className="screen">{screen.name}</p>; // replaced in Tasks 9–11
  }
}
