import React from "react";
import { useTranslation } from "react-i18next";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { LanguageSelector } from "../LanguageSelector";
import { LanguageAllowlist } from "../LanguageAllowlist"; // fork(voice-control)
import { AsrContextBiasing } from "../AsrContextBiasing"; // fork(voice-control)
import { TranslateToEnglish } from "../TranslateToEnglish";
import { useModelStore } from "../../../stores/modelStore";
import type { ModelInfo } from "@/bindings";

export const ModelSettingsCard: React.FC = () => {
  const { t } = useTranslation();
  const { currentModel, models } = useModelStore();

  const currentModelInfo = models.find((m: ModelInfo) => m.id === currentModel);

  const showLanguageSelector =
    currentModelInfo?.supports_language_selection ?? false;
  const supportsTranslation = currentModelInfo?.supports_translation ?? false;

  // Don't render anything if no model is selected.
  // fork(voice-control): the ASR biasing toggle applies to every model, so the
  // card no longer hides when the model has no language/translation settings.
  if (!currentModel || !currentModelInfo) {
    return null;
  }

  return (
    <SettingsGroup
      title={t("settings.modelSettings.title", {
        model: currentModelInfo.name,
      })}
    >
      {showLanguageSelector && (
        <LanguageSelector
          descriptionMode="tooltip"
          grouped={true}
          supportedLanguages={currentModelInfo.supported_languages}
          supportsLanguageDetection={
            currentModelInfo.supports_language_detection
          }
        />
      )}
      {/* fork(voice-control): auto-detect allowlist (LanguageAllowlist.tsx) */}
      {showLanguageSelector && (
        <LanguageAllowlist
          descriptionMode="tooltip"
          grouped={true}
          supportedLanguages={currentModelInfo.supported_languages}
          supportsLanguageDetection={
            currentModelInfo.supports_language_detection
          }
        />
      )}
      {supportsTranslation && (
        <TranslateToEnglish descriptionMode="tooltip" grouped={true} />
      )}
      {/* fork(voice-control): vocabulary & app-context biasing (AsrContextBiasing.tsx) */}
      <AsrContextBiasing
        model={currentModelInfo}
        descriptionMode="tooltip"
        grouped={true}
      />
    </SettingsGroup>
  );
};
