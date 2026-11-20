import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { commands, type LanguageStatus } from "@/bindings";
import { Disclosure } from "../ui/Disclosure";
import { Dropdown } from "../ui/Dropdown";
import type { DropdownOption } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";
import { useModelStore } from "../../stores/modelStore";
import {
  languageName,
  searchLanguages,
  LANGUAGES,
  supportsLanguage,
} from "../../lib/constants/languages";

interface LanguageAllowlistProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
  supportedLanguages?: string[];
  modelId: string;
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
  modelId,
}) => {
  const { t, i18n } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const [searchQuery, setSearchQuery] = useState("");

  const intent = getSetting("selected_language") || "auto";
  const allowlist = getSetting("language_allowlist") || [];
  const fallbackModel = getSetting("language_allowlist_fallback_model") ?? null;
  const models = useModelStore((state) => state.models);

  // Use upstream's resolved Auto status, including its downloaded language detector.
  const spoken = getSetting("spoken_languages");
  const spokenKey = spoken?.join(",");
  const detectionMode = getSetting("language_detection");
  const [status, setStatus] = useState<LanguageStatus | null>(null);
  useEffect(() => {
    let cancelled = false;
    setStatus(null);
    const refresh = () =>
      commands.getLanguageStatus(modelId).then((next) => {
        if (!cancelled) setStatus(next);
      });
    refresh();
    const unlisten = listen("model-download-complete", refresh);
    return () => {
      cancelled = true;
      unlisten.then((stop) => stop());
    };
  }, [modelId, intent, spokenKey, detectionMode]);
  const isAuto = status?.kind === "auto";

  // Concrete languages the model can recognize (no "auto" pseudo-entry).
  const languages = useMemo(
    () =>
      LANGUAGES.filter(
        (language) =>
          supportedLanguages.length === 0 ||
          supportsLanguage(supportedLanguages, language.code),
      ),
    [supportedLanguages],
  );

  // Allowed languages first (primary on top), then the rest; both filtered by
  // the search so a long Whisper list stays scannable.
  const visibleLanguages = useMemo(() => {
    const matches = searchLanguages(searchQuery, i18n.language, languages);
    const allowed = allowlist
      .map((code) => matches.find((language) => language.code === code))
      .filter((language) => language !== undefined);
    const rest = matches.filter(
      (language) => !allowlist.includes(language.code),
    );
    return [...allowed, ...rest];
  }, [languages, allowlist, searchQuery, i18n.language]);

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

  const labelFor = (code: string) => languageName(code, i18n.language);

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
              const index = allowlist.indexOf(language.code);
              const isAllowed = index !== -1;
              return (
                <div
                  key={language.code}
                  className="flex items-center justify-between gap-2 px-2 py-1 hover:bg-logo-primary/10 transition-colors duration-150"
                >
                  <label className="flex items-center gap-2 text-sm cursor-pointer min-w-0 flex-1">
                    <input
                      type="checkbox"
                      checked={isAllowed}
                      disabled={updatingAllowlist}
                      onChange={(event) =>
                        handleToggle(language.code, event.target.checked)
                      }
                    />
                    <span className="truncate">{labelFor(language.code)}</span>
                  </label>
                  {index === 0 ? (
                    <span className="text-xs text-logo-primary font-semibold">
                      {t("settings.general.language.allowlist.primary")}
                    </span>
                  ) : (
                    isAllowed && (
                      <button
                        type="button"
                        onClick={() => handleMakePrimary(language.code)}
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
