import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";
import { commands, events } from "@/bindings";
import type { ModelInfo } from "@/bindings";

interface AsrContextBiasingProps {
  model: ModelInfo;
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

/**
 * fork(voice-control): toggles `asr_context_biasing_enabled` — custom words,
 * learned corrections and the target app's context passed to the recognizer
 * (see `src-tauri/src/asr_bias`). Only some transcribe-cpp archs accept them;
 * the backend reports what a model accepted on its last run, so the hint
 * appears once that is known (non-transcribe-cpp engines never support it).
 */
export const AsrContextBiasing: React.FC<AsrContextBiasingProps> = ({
  model,
  descriptionMode = "tooltip",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const [observed, setObserved] = useState<boolean | null>(null);

  const isTranscribeCpp = model.engine_type === "TranscribeCpp";
  useEffect(() => {
    let active = true;
    let request = 0;
    setObserved(null);
    const refresh = () => {
      if (!active || !isTranscribeCpp) return;
      const currentRequest = ++request;
      commands
        .getAsrBiasSupport(model.id)
        .then((supported) => {
          if (active && currentRequest === request) setObserved(supported);
        })
        .catch(() => {});
    };
    refresh();
    // The model's observed support can change on its first/next transcription.
    const unlisten = isTranscribeCpp
      ? events.historyUpdatePayload.listen(({ payload }) => {
          if (payload.action === "added" || payload.action === "updated") {
            refresh();
          }
        })
      : null;
    // Re-read after registration so a completion during setup is covered too.
    unlisten?.then(refresh).catch(() => {});
    return () => {
      active = false;
      unlisten?.then((stop) => stop()).catch(() => {});
    };
  }, [model.id, isTranscribeCpp]);

  const unsupported = !isTranscribeCpp || observed === false;
  const enabled = getSetting("asr_context_biasing_enabled") ?? true;

  return (
    <ToggleSwitch
      checked={enabled}
      onChange={(value) => updateSetting("asr_context_biasing_enabled", value)}
      isUpdating={isUpdating("asr_context_biasing_enabled")}
      label={t("settings.general.asrBiasing.label")}
      description={
        unsupported
          ? t("settings.general.asrBiasing.unsupported")
          : t("settings.general.asrBiasing.description")
      }
      descriptionMode={unsupported ? "inline" : descriptionMode}
      grouped={grouped}
    />
  );
};
