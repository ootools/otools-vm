import arSA from '../../i18n/ar-SA.json';
import deDE from '../../i18n/de-DE.json';
import enUS from '../../i18n/en-US.json';
import esES from '../../i18n/es-ES.json';
import jaJP from '../../i18n/ja-JP.json';
import koKR from '../../i18n/ko-KR.json';
import ruRU from '../../i18n/ru-RU.json';
import zhCN from '../../i18n/zh-CN.json';

type Dictionary = Record<string, string>;

const DICTIONARIES: Record<string, Dictionary> = {
  'ar-SA': arSA as Dictionary,
  'de-DE': deDE as Dictionary,
  'en-US': enUS as Dictionary,
  'es-ES': esES as Dictionary,
  'ja-JP': jaJP as Dictionary,
  'ko-KR': koKR as Dictionary,
  'ru-RU': ruRU as Dictionary,
  'zh-CN': zhCN as Dictionary,
};

const SUPPORTED = Object.keys(DICTIONARIES);

function pickLocale(): string {
  const candidates: string[] = [];
  if (typeof navigator !== 'undefined') {
    candidates.push(...(navigator.languages ?? []), navigator.language ?? '');
  }
  try {
    const fromQuery = new URLSearchParams(window.location.search).get('lang');
    if (fromQuery) {
      candidates.unshift(fromQuery);
    }
  } catch {
    // ignore
  }
  for (const raw of candidates) {
    const normalized = String(raw ?? '').replace('_', '-');
    if (!normalized) continue;
    const exact = SUPPORTED.find((item) => item.toLowerCase() === normalized.toLowerCase());
    if (exact) return exact;
    const prefix = normalized.split('-')[0].toLowerCase();
    const byPrefix = SUPPORTED.find((item) => item.toLowerCase().startsWith(prefix));
    if (byPrefix) return byPrefix;
  }
  return 'en-US';
}

const locale = pickLocale();
const primary = DICTIONARIES[locale] ?? DICTIONARIES['en-US'];
const fallback = DICTIONARIES['en-US'];

export const currentLocale = locale;

/** 取文案；缺失时回落英文，再缺失就原样返回 key（便于发现漏翻）。 */
export function t(key: string, params?: Record<string, string | number>): string {
  const template = primary[key] ?? fallback[key] ?? key;
  if (!params) {
    return template;
  }
  return template.replace(/\{(\w+)\}/g, (match, name: string) => {
    const value = params[name];
    return value === undefined ? match : String(value);
  });
}
