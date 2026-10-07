import { api, type AppError } from '../api';
import { useT } from '../i18n';

export default function Broken({ error }: { error: AppError }) {
  const { t } = useT();
  return (
    <main className="screen">
      <h1>{t('broken.title')}</h1>
      <p>{t('broken.body', { path: error.params.path ?? '', detail: error.params.detail ?? '' })}</p>
      <div className="row">
        <button onClick={() => void api.openDataFolder()}>{t('broken.openFolder')}</button>
      </div>
    </main>
  );
}
