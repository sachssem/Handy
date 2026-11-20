import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface RecordingLimitAutoStopProps {
  descriptionMode?: "tooltip" | "inline";
  grouped?: boolean;
}

export const RecordingLimitAutoStop: React.FC<RecordingLimitAutoStopProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const enabled = getSetting("auto_stop_recording_on_limit") ?? true;

  return (
    <ToggleSwitch
      checked={enabled}
      onChange={(enabled) =>
        updateSetting("auto_stop_recording_on_limit", enabled)
      }
      isUpdating={isUpdating("auto_stop_recording_on_limit")}
      label={t("settings.advanced.recordingLimitAutoStop.title")}
      description={t("settings.advanced.recordingLimitAutoStop.description")}
      descriptionMode={descriptionMode}
      grouped={grouped}
    />
  );
};
