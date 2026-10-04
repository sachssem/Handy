import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Disclosure } from "../ui/Disclosure";
import { Dropdown } from "../ui/Dropdown";
import type { DropdownOption } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";
import { useModelStore } from "../../stores/modelStore";
import {
  getLanguageLabel,
  LANGUAGES,
  supportsLanguageCode,
} from "../../lib/constants/languages";

interface LanguageAllowlistProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
  supportedLanguages?: string[];
  supportsLanguageDetection?: boolean;
}

/**
 * fork(voice-control): restricts "auto" language detection to a chosen set of
 * languages (`language_allowlist`). Index 0 is the *primary* language — the
 * one a rejected detection is re-pinned to (see the guard in
 * managers/transcription.rs) — so ordering is part of the setting's contract.
 * Optionally escalates to a fallback model instead of the same-engine retry.
 *
 * Mounted next to `LanguageSelector` in `ModelSettingsCard`; renders nothing
 * unless the effective recognition language is "auto".
 */
export const LanguageAllowlist: React.FC<LanguageAllowlistProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
  supportedLanguages = [],
  supportsLanguageDetection = true,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const [searchQuery, setSearchQuery] = useState("");

  const intent = getSetting("selected_language") || "auto";
  const allowlist = getSetting("language_allowlist") || [];
  const fallbackModel = getSetting("language_allowlist_fallback_model") ?? null;
  const models = useModelStore((state) => state.models);

  // Mirrors `effectiveLanguage` in LanguageSelector: the picker resolves to
  // "auto" when the intent is auto or unsupported by a detecting model.
  const isAuto =
    supportsLanguageDetection &&
    (intent === "auto" ||
      (supportedLanguages.length > 0 &&
        !supportsLanguageCode(supportedLanguages, intent)));

  // Concrete languages the model can recognize (no "auto" pseudo-entry).
  const languages = useMemo(
    () =>
      LANGUAGES.filter(
        (language) =>
          language.value !== "auto" &&
          (supportedLanguages.length === 0 ||
            supportsLanguageCode(supportedLanguages, language.value)),
      ),
    [supportedLanguages],
  );

  // Allowed languages first (primary on top), then the rest; both filtered by
  // the search so a long Whisper list stays scannable.
  const visibleLanguages = useMemo(() => {
    const query = searchQuery.trim().toLowerCase();
    const matches = languages.filter((language) =>
      language.label.toLowerCase().includes(query),
    );
    const allowed = allowlist
      .map((code) => matches.find((language) => language.value === code))
      .filter((language) => language !== undefined);
    const rest = matches.filter(
      (language) => !allowlist.includes(language.value),
    );
    return [...allowed, ...rest];
  }, [languages, allowlist, searchQuery]);

  const fallbackOptions: DropdownOption[] = useMemo(
    () => [
      {
        value: "",
        label: t("settings.general.language.allowlist.fallbackModel.none"),
      },
      ...models
        .filter((model) => model.is_downloaded)
        .map((model) => ({ value: model.id, label: model.name })),
    ],
    [models, t],
  );

  if (!isAuto || languages.length === 0) return null;

  const updatingAllowlist = isUpdating("language_allowlist");

  // Appending on check keeps the primary (index 0) stable while languages are
  // added or removed; removing the primary promotes the next one.
  const handleToggle = (code: string, checked: boolean) => {
    const next = checked
      ? [...allowlist.filter((c) => c !== code), code]
      : allowlist.filter((c) => c !== code);
    updateSetting("language_allowlist", next);
  };

  const handleMakePrimary = (code: string) => {
    updateSetting("language_allowlist", [
      code,
      ...allowlist.filter((c) => c !== code),
    ]);
  };

  const labelFor = (code: string) => getLanguageLabel(code) ?? code;

  const summary =
    allowlist.length === 0
      ? t("settings.general.language.allowlist.summaryAll")
      : [
          t("settings.general.language.allowlist.primaryTag", {
            language: labelFor(allowlist[0]),
          }),
          ...allowlist.slice(1).map(labelFor),
        ].join(", ");

  return (
    <Disclosure
      title={t("settings.general.language.allowlist.title")}
      summary={summary}
    >
      <div className="px-4 space-y-2">
        <p className="text-xs text-mid-gray">
          {t("settings.general.language.allowlist.description")}
        </p>
        <input
          type="text"
          value={searchQuery}
          onChange={(event) => setSearchQuery(event.target.value)}
          placeholder={t("settings.general.language.searchPlaceholder")}
          aria-label={t("settings.general.language.searchPlaceholder")}
          className="w-full px-2 py-1 text-sm bg-mid-gray/10 border border-mid-gray/40 rounded focus:outline-none focus:ring-1 focus:ring-logo-primary focus:border-logo-primary"
        />
        <div className="max-h-48 overflow-y-auto border border-mid-gray/20 rounded">
          {visibleLanguages.length === 0 ? (
            <div className="px-2 py-2 text-sm text-mid-gray text-center">
              {t("settings.general.language.noResults")}
            </div>
          ) : (
            visibleLanguages.map((language) => {
              const index = allowlist.indexOf(language.value);
              const isAllowed = index !== -1;
              return (
                <div
                  key={language.value}
                  className="flex items-center justify-between gap-2 px-2 py-1 hover:bg-logo-primary/10 transition-colors duration-150"
                >
                  <label className="flex items-center gap-2 text-sm cursor-pointer min-w-0 flex-1">
                    <input
                      type="checkbox"
                      checked={isAllowed}
                      disabled={updatingAllowlist}
                      onChange={(event) =>
                        handleToggle(language.value, event.target.checked)
                      }
                    />
                    <span className="truncate">{language.label}</span>
                  </label>
                  {index === 0 ? (
                    <span className="text-xs text-logo-primary font-semibold">
                      {t("settings.general.language.allowlist.primary")}
                    </span>
                  ) : (
                    isAllowed && (
                      <button
                        type="button"
                        onClick={() => handleMakePrimary(language.value)}
                        disabled={updatingAllowlist}
                        className="text-xs text-mid-gray hover:text-logo-primary cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                      >
                        {t("settings.general.language.allowlist.makePrimary")}
                      </button>
                    )
                  )}
                </div>
              );
            })
          )}
        </div>
      </div>
      <SettingContainer
        title={t("settings.general.language.allowlist.fallbackModel.label")}
        description={t(
          "settings.general.language.allowlist.fallbackModel.description",
        )}
        descriptionMode={descriptionMode}
        grouped={grouped}
        disabled={allowlist.length === 0}
      >
        <Dropdown
          options={fallbackOptions}
          selectedValue={fallbackModel ?? ""}
          onSelect={(value) =>
            updateSetting(
              "language_allowlist_fallback_model",
              value === "" ? null : value,
            )
          }
          disabled={
            allowlist.length === 0 ||
            isUpdating("language_allowlist_fallback_model")
          }
        />
      </SettingContainer>
    </Disclosure>
  );
};
