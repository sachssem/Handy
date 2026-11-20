import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "@/bindings";
import { Dropdown } from "../ui/Dropdown";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { SettingContainer } from "../ui/SettingContainer";
import { PathDisplay } from "../ui/PathDisplay";
import { useSettings } from "../../hooks/useSettings";

interface DictationJournalProps {
  descriptionMode?: "tooltip" | "inline";
  grouped?: boolean;
}

/** Retention choices offered in the UI (the backend accepts 1–365). */
const RETENTION_DAYS = [30, 90, 180, 365];

/** Fork (voice-control): local dictation journal switch, retention + folder. */
export const DictationJournal: React.FC<DictationJournalProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const enabled = getSetting("dictation_journal_enabled") ?? true;
  const retentionDays = getSetting("dictation_journal_retention_days") ?? 90;
  // Keep a value set outside these choices (settings file) selectable.
  const retentionOptions = [...new Set([...RETENTION_DAYS, retentionDays])]
    .sort((a, b) => a - b)
    .map((days) => ({
      value: String(days),
      label: t("settings.advanced.dictationJournal.retentionOption", { days }),
    }));
  const [journalDir, setJournalDir] = useState("");

  useEffect(() => {
    commands
      .getJournalDirPath()
      .then((result) => {
        if (result.status === "ok") setJournalDir(result.data);
      })
      .catch(() => {});
  }, []);

  const handleOpen = async () => {
    try {
      await commands.openJournalDir();
    } catch (err) {
      console.error("Failed to open journal directory:", err);
    }
  };

  return (
    <>
      <ToggleSwitch
        checked={enabled}
        onChange={(checked) =>
          updateSetting("dictation_journal_enabled", checked)
        }
        isUpdating={isUpdating("dictation_journal_enabled")}
        label={t("settings.advanced.dictationJournal.title")}
        description={t("settings.advanced.dictationJournal.description", {
          days: retentionDays,
        })}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
      {enabled && (
        <SettingContainer
          title={t("settings.advanced.dictationJournal.retentionTitle")}
          description={t(
            "settings.advanced.dictationJournal.retentionDescription",
          )}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <Dropdown
            options={retentionOptions}
            selectedValue={String(retentionDays)}
            onSelect={(value) =>
              updateSetting("dictation_journal_retention_days", Number(value))
            }
            disabled={isUpdating("dictation_journal_retention_days")}
          />
        </SettingContainer>
      )}
      {enabled && journalDir && (
        <SettingContainer
          title={t("settings.advanced.dictationJournal.folderTitle")}
          description={t(
            "settings.advanced.dictationJournal.folderDescription",
          )}
          descriptionMode={descriptionMode}
          grouped={grouped}
          layout="stacked"
        >
          <PathDisplay path={journalDir} onOpen={handleOpen} />
        </SettingContainer>
      )}
    </>
  );
};
