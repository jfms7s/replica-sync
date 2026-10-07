import { I18nProvider, useT } from './i18n';

function Title() {
  const { t } = useT();
  return <h1>{t('app.title')}</h1>;
}

export default function App() {
  return (
    <I18nProvider lang="en">
      <Title />
    </I18nProvider>
  );
}
