/**
 * The dictation languages Claude's speech service takes (claude's own list —
 * mirrored by the daemon's `voice::LANGUAGES`), each named in itself for the
 * settings picker. A pure leaf: the settings schema reads it too.
 */
export const DICTATION_LANGUAGES: ReadonlyArray<{ code: string; name: string }> = [
  { code: "en", name: "English" },
  { code: "es", name: "Español" },
  { code: "fr", name: "Français" },
  { code: "de", name: "Deutsch" },
  { code: "it", name: "Italiano" },
  { code: "pt", name: "Português" },
  { code: "nl", name: "Nederlands" },
  { code: "sv", name: "Svenska" },
  { code: "da", name: "Dansk" },
  { code: "no", name: "Norsk" },
  { code: "pl", name: "Polski" },
  { code: "cs", name: "Čeština" },
  { code: "el", name: "Ελληνικά" },
  { code: "tr", name: "Türkçe" },
  { code: "uk", name: "Українська" },
  { code: "ru", name: "Русский" },
  { code: "hi", name: "हिन्दी" },
  { code: "id", name: "Bahasa Indonesia" },
  { code: "ja", name: "日本語" },
  { code: "ko", name: "한국어" },
];
