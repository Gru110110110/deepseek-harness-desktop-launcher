import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./en.json";
import zh from "./zh.json";

const resources = { en: { translation: en }, zh: { translation: zh } } as const;

void i18n.use(initReactI18next).init({
  resources,
  lng: "zh",
  fallbackLng: "zh",
  interpolation: { escapeValue: false },
  returnNull: false,
});
