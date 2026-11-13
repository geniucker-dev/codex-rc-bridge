import { setLanguage, t, useLanguage, type Language } from "./i18n";
export function LanguageSelect() {
  const language = useLanguage();
  return (
    <label className="language-select">
      <span>{t("Language")}</span>
      <select
        aria-label={t("Language")}
        value={language}
        onChange={(event) => setLanguage(event.target.value as Language)}
      >
        <option value="system">{t("Follow system")}</option>
        <option value="en">English</option>
        <option value="zh">简体中文</option>
      </select>
    </label>
  );
}
