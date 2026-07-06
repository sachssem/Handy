import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useSettings } from "../../hooks/useSettings";
import { commands } from "@/bindings";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";
import { ToggleSwitch } from "../ui/ToggleSwitch";

interface LearnedCorrectionsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const LearnedCorrections: React.FC<LearnedCorrectionsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating, refreshSettings } =
      useSettings();

    const enabled = getSetting("learn_corrections_enabled") || false;
    const corrections = getSetting("learned_corrections") || [];

    const [newMisheard, setNewMisheard] = useState("");
    const [newIntended, setNewIntended] = useState("");
    const [isAdding, setIsAdding] = useState(false);

    const isBusy = isUpdating("learned_corrections") || isAdding;

    const handleAdd = async () => {
      const misheard = newMisheard.trim();
      const intended = newIntended.trim();
      if (!misheard || !intended) {
        return;
      }
      if (
        corrections.some(
          (c) => c.misheard.toLowerCase() === misheard.toLowerCase(),
        )
      ) {
        toast.error(
          t("settings.advanced.learnedCorrections.add.duplicate", { misheard }),
        );
        return;
      }
      // The backend assigns the id / timestamp, so add through the command and
      // refresh rather than editing the list locally.
      setIsAdding(true);
      try {
        const result = await commands.addLearnedCorrection(misheard, intended);
        if (result.status === "ok") {
          await refreshSettings();
          setNewMisheard("");
          setNewIntended("");
        } else {
          toast.error(result.error);
        }
      } finally {
        setIsAdding(false);
      }
    };

    const handleToggleEntry = (id: string, nextEnabled: boolean) => {
      updateSetting(
        "learned_corrections",
        corrections.map((c) =>
          c.id === id ? { ...c, enabled: nextEnabled } : c,
        ),
      );
    };

    const handleRemove = (id: string) => {
      updateSetting(
        "learned_corrections",
        corrections.filter((c) => c.id !== id),
      );
    };

    const handleKeyPress = (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        handleAdd();
      }
    };

    return (
      <>
        <ToggleSwitch
          checked={enabled}
          onChange={(checked) =>
            updateSetting("learn_corrections_enabled", checked)
          }
          isUpdating={isUpdating("learn_corrections_enabled")}
          label={t("settings.advanced.learnedCorrections.title")}
          description={t("settings.advanced.learnedCorrections.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />

        {enabled && (
          <div className="px-4 p-2 space-y-3">
            <div className="flex flex-wrap items-center gap-2">
              <Input
                type="text"
                className="max-w-40"
                value={newMisheard}
                onChange={(e) => setNewMisheard(e.target.value)}
                onKeyDown={handleKeyPress}
                placeholder={t(
                  "settings.advanced.learnedCorrections.add.misheardPlaceholder",
                )}
                variant="compact"
                disabled={isBusy}
              />
              <Input
                type="text"
                className="max-w-40"
                value={newIntended}
                onChange={(e) => setNewIntended(e.target.value)}
                onKeyDown={handleKeyPress}
                placeholder={t(
                  "settings.advanced.learnedCorrections.add.intendedPlaceholder",
                )}
                variant="compact"
                disabled={isBusy}
              />
              <Button
                onClick={handleAdd}
                disabled={!newMisheard.trim() || !newIntended.trim() || isBusy}
                variant="primary"
                size="md"
              >
                {t("settings.advanced.learnedCorrections.add.button")}
              </Button>
            </div>

            {corrections.length === 0 ? (
              <div className="text-xs text-mid-gray">
                {t("settings.advanced.learnedCorrections.empty")}
              </div>
            ) : (
              <div className="flex flex-col gap-1">
                {corrections.map((correction) => (
                  <div
                    key={correction.id}
                    className="flex items-center justify-between gap-2 text-sm"
                  >
                    <div className="flex items-center gap-2 min-w-0">
                      <input
                        type="checkbox"
                        checked={correction.enabled}
                        disabled={isBusy}
                        onChange={(e) =>
                          handleToggleEntry(correction.id, e.target.checked)
                        }
                        aria-label={t(
                          "settings.advanced.learnedCorrections.enable",
                          { misheard: correction.misheard },
                        )}
                      />
                      <span
                        className={`truncate ${correction.enabled ? "" : "opacity-50 line-through"}`}
                      >
                        {t("settings.advanced.learnedCorrections.mapping", {
                          misheard: correction.misheard,
                          intended: correction.intended,
                        })}
                      </span>
                      <span className="shrink-0 text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded bg-mid-gray/15 text-mid-gray">
                        {t(
                          `settings.advanced.learnedCorrections.source.${correction.source}`,
                        )}
                      </span>
                      {correction.count > 1 && (
                        <span className="shrink-0 text-xs text-mid-gray">
                          {t("settings.advanced.learnedCorrections.count", {
                            count: correction.count,
                          })}
                        </span>
                      )}
                    </div>
                    <Button
                      onClick={() => handleRemove(correction.id)}
                      disabled={isBusy}
                      variant="danger-ghost"
                      size="sm"
                      aria-label={t(
                        "settings.advanced.learnedCorrections.remove",
                        { misheard: correction.misheard },
                      )}
                    >
                      <svg
                        className="w-3 h-3"
                        fill="none"
                        stroke="currentColor"
                        viewBox="0 0 24 24"
                      >
                        <path
                          strokeLinecap="round"
                          strokeLinejoin="round"
                          strokeWidth={2}
                          d="M6 18L18 6M6 6l12 12"
                        />
                      </svg>
                    </Button>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
      </>
    );
  },
);
