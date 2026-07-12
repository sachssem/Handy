import React, { useState, useRef, useEffect, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { SettingContainer } from "../ui/SettingContainer";
import { ResetButton } from "../ui/ResetButton";
import { useSettings } from "../../hooks/useSettings";
import { useModelStore } from "../../stores/modelStore";
import {
  getLanguageLabel,
  recognitionLanguage,
  SELECTABLE_LANGUAGES,
  supportsLanguageCode,
} from "../../lib/constants/languages";

interface LanguageSelectorProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
  supportedLanguages?: string[];
  // Whether the model can auto-detect language. Gates the "Auto" option:
  // must-pick models (no detection) omit it and force a concrete choice.
  supportsLanguageDetection?: boolean;
}

// Mirrors the matching logic of `effective_language` in
// src-tauri/src/managers/model.rs. The Rust function is authoritative for the
// *concrete* code the engine receives (e.g. "en-US"); this resolves the
// canonical *base* code ("en") so the highlighted picker item matches an entry
// in the LANGUAGES list. Matching is base-aware (`supportsLanguageCode` strips
// region/script subtags), so a model advertising full locales still resolves.
const effectiveLanguage = (
  intent: string,
  supported: string[],
  supportsDetection: boolean,
): string => {
  if (supported.length === 0) return intent;
  if (intent !== "auto" && supportsLanguageCode(supported, intent))
    return intent;
  if (supportsDetection) return "auto";
  if (supportsLanguageCode(supported, "en")) return "en";
  return recognitionLanguage(supported[0]);
};

export const LanguageSelector: React.FC<LanguageSelectorProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
  supportedLanguages,
  supportsLanguageDetection = true,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, resetSetting, isUpdating } = useSettings();
  const [isOpen, setIsOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const dropdownRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);

  // The persisted *intent* (auto | code). What's actually used/shown is the
  // effective value resolved against the current model's capabilities.
  const intent = getSetting("selected_language") || "auto";
  const selectedLanguage = effectiveLanguage(
    intent,
    supportedLanguages ?? [],
    supportsLanguageDetection,
  );

  // fork(voice-control): the auto-detect allowlist. Only meaningful while the
  // effective language is "auto"; the first entry is the language a rejected
  // detection is re-pinned to (see the guard in managers/transcription.rs).
  const allowlist = getSetting("language_allowlist") || [];

  // fork(voice-control): optional fallback model the guard escalates to on an
  // out-of-bounds detection, chosen from the downloaded models. Empty selection
  // ("None") clears it, keeping the same-engine pin-retry.
  const fallbackModel = getSetting("language_allowlist_fallback_model") ?? null;
  const models = useModelStore((state) => state.models);
  const initializeModels = useModelStore((state) => state.initialize);
  const downloadedModels = useMemo(
    () => models.filter((model) => model.is_downloaded),
    [models],
  );

  useEffect(() => {
    void initializeModels();
  }, [initializeModels]);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (
        dropdownRef.current &&
        !dropdownRef.current.contains(event.target as Node)
      ) {
        setIsOpen(false);
        setSearchQuery("");
      }
    };

    document.addEventListener("mousedown", handleClickOutside);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
    };
  }, []);

  useEffect(() => {
    if (isOpen && searchInputRef.current) {
      searchInputRef.current.focus();
    }
  }, [isOpen]);

  const availableLanguages = useMemo(() => {
    if (!supportedLanguages || supportedLanguages.length === 0)
      return SELECTABLE_LANGUAGES;
    return SELECTABLE_LANGUAGES.filter((lang) =>
      lang.value === "auto"
        ? supportsLanguageDetection
        : supportsLanguageCode(supportedLanguages, lang.value),
    );
  }, [supportedLanguages, supportsLanguageDetection]);

  // fork(voice-control): the concrete languages the allowlist can contain
  // (the model's available languages, minus the "auto" pseudo-entry).
  const allowlistLanguages = useMemo(
    () => availableLanguages.filter((language) => language.value !== "auto"),
    [availableLanguages],
  );

  const filteredLanguages = useMemo(
    () =>
      availableLanguages.filter((language) =>
        language.label.toLowerCase().includes(searchQuery.toLowerCase()),
      ),
    [searchQuery, availableLanguages],
  );

  const selectedLanguageName =
    getLanguageLabel(selectedLanguage) || t("settings.general.language.auto");

  const handleLanguageSelect = async (languageCode: string) => {
    await updateSetting("selected_language", languageCode);
    setIsOpen(false);
    setSearchQuery("");
  };

  const handleReset = async () => {
    await resetSetting("selected_language");
  };

  // fork(voice-control): toggle a language in the allowlist. Appending on check
  // keeps the first-checked language at index 0 so the "primary" (retry) target
  // stays stable while later languages are added or removed.
  const handleToggleAllowed = (code: string, checked: boolean) => {
    const next = checked
      ? [...allowlist.filter((c) => c !== code), code]
      : allowlist.filter((c) => c !== code);
    updateSetting("language_allowlist", next);
  };

  // fork(voice-control): select (or clear, via "") the fallback model.
  const handleFallbackModelChange = (value: string) => {
    updateSetting(
      "language_allowlist_fallback_model",
      value === "" ? null : value,
    );
  };

  const handleToggle = () => {
    if (isUpdating("selected_language")) return;
    setIsOpen(!isOpen);
  };

  const handleSearchChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    setSearchQuery(event.target.value);
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter" && filteredLanguages.length > 0) {
      // Select first filtered language on Enter
      handleLanguageSelect(filteredLanguages[0].value);
    } else if (event.key === "Escape") {
      setIsOpen(false);
      setSearchQuery("");
    }
  };

  return (
    <>
      <SettingContainer
        title={t("settings.general.language.title")}
        description={t("settings.general.language.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      >
        <div className="flex items-center space-x-1">
          <div className="relative" ref={dropdownRef}>
            <button
              type="button"
              className={`px-2 py-1 text-sm font-semibold bg-mid-gray/10 border border-mid-gray/80 rounded min-w-[200px] text-start flex items-center justify-between transition-all duration-150 ${
                isUpdating("selected_language")
                  ? "opacity-50 cursor-not-allowed"
                  : "hover:bg-logo-primary/10 cursor-pointer hover:border-logo-primary"
              }`}
              onClick={handleToggle}
              disabled={isUpdating("selected_language")}
            >
              <span className="truncate">{selectedLanguageName}</span>
              <svg
                className={`w-4 h-4 ms-2 transition-transform duration-200 ${
                  isOpen ? "transform rotate-180" : ""
                }`}
                fill="none"
                stroke="currentColor"
                viewBox="0 0 24 24"
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  strokeWidth={2}
                  d="M19 9l-7 7-7-7"
                />
              </svg>
            </button>

            {isOpen && !isUpdating("selected_language") && (
              <div className="absolute top-full left-0 right-0 mt-1 bg-background border border-mid-gray/80 rounded shadow-lg z-50 max-h-60 overflow-hidden">
                {/* Search input */}
                <div className="p-2 border-b border-mid-gray/80">
                  <input
                    ref={searchInputRef}
                    type="text"
                    value={searchQuery}
                    onChange={handleSearchChange}
                    onKeyDown={handleKeyDown}
                    placeholder={t(
                      "settings.general.language.searchPlaceholder",
                    )}
                    className="w-full px-2 py-1 text-sm bg-mid-gray/10 border border-mid-gray/40 rounded focus:outline-none focus:ring-1 focus:ring-logo-primary focus:border-logo-primary"
                  />
                </div>

                <div className="max-h-48 overflow-y-auto">
                  {filteredLanguages.length === 0 ? (
                    <div className="px-2 py-2 text-sm text-mid-gray text-center">
                      {t("settings.general.language.noResults")}
                    </div>
                  ) : (
                    filteredLanguages.map((language) => (
                      <button
                        key={language.value}
                        type="button"
                        className={`w-full px-2 py-1 text-sm text-start hover:bg-logo-primary/10 transition-colors duration-150 ${
                          selectedLanguage === language.value
                            ? "bg-logo-primary/20 text-logo-primary font-semibold"
                            : ""
                        }`}
                        onClick={() => handleLanguageSelect(language.value)}
                      >
                        <div className="flex items-center justify-between">
                          <span className="truncate">{language.label}</span>
                        </div>
                      </button>
                    ))
                  )}
                </div>
              </div>
            )}
          </div>
          <ResetButton
            onClick={handleReset}
            disabled={isUpdating("selected_language")}
          />
        </div>
        {isUpdating("selected_language") && (
          <div className="absolute inset-0 bg-mid-gray/10 rounded flex items-center justify-center">
            <div className="w-4 h-4 border-2 border-logo-primary border-t-transparent rounded-full animate-spin"></div>
          </div>
        )}
      </SettingContainer>

      {/* fork(voice-control): restrict "auto" detection to a chosen set of
        languages, re-running mis-detections pinned to the first ("primary")
        entry. Only relevant while the effective language is auto. */}
      {selectedLanguage === "auto" &&
        supportsLanguageDetection &&
        allowlistLanguages.length > 0 && (
          <div className="px-4 p-2 space-y-2">
            <div className="text-sm font-semibold">
              {t("settings.general.language.allowlist.title")}
            </div>
            <div className="text-xs text-mid-gray">
              {t("settings.general.language.allowlist.description")}
            </div>
            <div className="flex flex-col gap-1">
              {allowlistLanguages.map((language) => {
                const isPrimary = allowlist[0] === language.value;
                return (
                  <label
                    key={language.value}
                    className="flex items-center gap-2 text-sm cursor-pointer"
                  >
                    <input
                      type="checkbox"
                      checked={allowlist.includes(language.value)}
                      disabled={isUpdating("language_allowlist")}
                      onChange={(e) =>
                        handleToggleAllowed(language.value, e.target.checked)
                      }
                    />
                    <span>{language.label}</span>
                    {isPrimary && (
                      <span className="text-xs text-logo-primary font-semibold">
                        {t("settings.general.language.allowlist.primary")}
                      </span>
                    )}
                  </label>
                );
              })}
            </div>

            <div className="pt-1 space-y-1">
              <div className="text-sm font-semibold">
                {t("settings.general.language.allowlist.fallbackModel.label")}
              </div>
              <div className="text-xs text-mid-gray">
                {t(
                  "settings.general.language.allowlist.fallbackModel.description",
                )}
              </div>
              <select
                value={fallbackModel ?? ""}
                disabled={isUpdating("language_allowlist_fallback_model")}
                onChange={(e) => handleFallbackModelChange(e.target.value)}
                className="px-2 py-1 text-sm bg-mid-gray/10 border border-mid-gray/80 rounded min-w-[200px] focus:outline-none focus:ring-1 focus:ring-logo-primary focus:border-logo-primary disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <option value="">
                  {t("settings.general.language.allowlist.fallbackModel.none")}
                </option>
                {downloadedModels.map((model) => (
                  <option key={model.id} value={model.id}>
                    {model.name}
                  </option>
                ))}
              </select>
            </div>
          </div>
        )}
    </>
  );
};
