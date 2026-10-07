import { useEffect, useState } from 'react';
import { api, type PairView, type PreviewSummary, type RunView } from './api';
import { I18nProvider, type Lang } from './i18n';
import Applying from './screens/Applying';
import Broken from './screens/Broken';
import CloseGuard from './screens/CloseGuard';
import Pairs from './screens/Pairs';
import PairEditor from './screens/PairEditor';
import Preview from './screens/Preview';
import Result from './screens/Result';
import Scanning from './screens/Scanning';
import Settings from './screens/Settings';
import Trash from './screens/Trash';

export type Screen =
  | { name: 'pairs' }
  | { name: 'editor'; pair: PairView | null }
  | { name: 'scanning'; pairId: string; pairName: string }
  | { name: 'preview'; summary: PreviewSummary }
  | { name: 'applying'; mode: 'apply' | 'retry'; pairName: string }
  | { name: 'result'; run: RunView; pairName: string }
  | { name: 'trash'; pairId: string; pairName: string; back?: Screen }
  | { name: 'settings' };
export type Navigate = (s: Screen) => void;

export default function App() {
  const [lang, setLang] = useState<Lang>('en');
  const [screen, setScreen] = useState<Screen>({ name: 'pairs' });
  // Wrapped so any rejection, even `null`, counts as broken.
  const [broken, setBroken] = useState<{ error: unknown } | null>(null);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    api.getSettings().then((s) => setLang(s.resolvedLanguage)).catch(() => {});
    api
      .startupStatus()
      .catch((e: unknown) => setBroken({ error: e }))
      .finally(() => setReady(true));
  }, []);

  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);

  if (!ready) return null;
  return (
    <I18nProvider lang={lang}>
      {broken ? <Broken error={broken.error} /> : <Current screen={screen} navigate={setScreen} onLanguage={setLang} />}
      <CloseGuard />
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
    case 'applying':
      return <Applying mode={screen.mode} pairName={screen.pairName} navigate={navigate} />;
    case 'result':
      return <Result run={screen.run} pairName={screen.pairName} navigate={navigate} />;
    case 'trash':
      return <Trash pairId={screen.pairId} pairName={screen.pairName} back={screen.back} navigate={navigate} />;
  }
}
