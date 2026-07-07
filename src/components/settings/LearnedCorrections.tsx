import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { type } from "@tauri-apps/plugin-os";
import { useSettings } from "../../hooks/useSettings";
import { commands } from "@/bindings";
import type { Aggressiveness } from "@/bindings";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Dropdown } from "../ui/Dropdown";
import type { DropdownOption } from "../ui/Dropdown";
import { RemoveIcon } from "../icons";

interface LearnedCorrectionsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const AGGRESSIVENESS_LEVELS: Aggressiveness[] = [
  "conservative",
  "balanced",
  "aggressive",
];

// Learning-window bounds, mirroring MIN_WINDOW_SECS / MAX_WINDOW_SECS in
// correction_learning/session.rs — the persisted value is clamped here so the
// input always shows the value the session will actually use.
const WINDOW_MIN_SECS = 10;
const WINDOW_MAX_SECS = 300;

export const LearnedCorrections: React.FC<LearnedCorrectionsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating, refreshSettings } =
      useSettings();

    // Automatic learning (the post-paste watcher) only exists on macOS; the
    // dictionary itself — manual add + deterministic apply — works everywhere.
    const isMacOS = type() === "macos";

    const enabled = getSetting("learn_corrections_enabled") || false;
    const logOnly = getSetting("learn_corrections_log_only") || false;
    const aggressiveness =
      getSetting("learn_corrections_aggressiveness") || "conservative";
    const windowSecs = getSetting("learn_corrections_window_secs") ?? 45;
    const corrections = getSetting("learned_corrections") || [];

    const aggressivenessOptions: DropdownOption[] = AGGRESSIVENESS_LEVELS.map(
      (level) => ({
        value: level,
        label: t(
          `settings.advanced.learnedCorrections.aggressiveness.${level}.label`,
        ),
      }),
    );

    const handleWindowChange = (event: React.ChangeEvent<HTMLInputElement>) => {
      const value = parseInt(event.target.value, 10);
      if (!isNaN(value)) {
        const clamped = Math.min(
          WINDOW_MAX_SECS,
          Math.max(WINDOW_MIN_SECS, value),
        );
        updateSetting("learn_corrections_window_secs", clamped);
      }
    };

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
      // Adding an existing misheard word is allowed: the backend upserts on the
      // misheard key, so a new target simply replaces the previous mapping.
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
          <>
            {/* Auto-learning-only controls: macOS is the only platform with the
                post-paste field watcher. Elsewhere the dictionary is manual, so
                these are hidden and a hint explains why. */}
            {isMacOS ? (
              <>
                <ToggleSwitch
                  checked={logOnly}
                  onChange={(checked) =>
                    updateSetting("learn_corrections_log_only", checked)
                  }
                  isUpdating={isUpdating("learn_corrections_log_only")}
                  label={t(
                    "settings.advanced.learnedCorrections.trialMode.title",
                  )}
                  description={t(
                    "settings.advanced.learnedCorrections.trialMode.description",
                  )}
                  descriptionMode={descriptionMode}
                  grouped={grouped}
                />

                <div className="px-4 p-2 space-y-2">
                  <div className="text-sm font-semibold">
                    {t(
                      "settings.advanced.learnedCorrections.aggressiveness.title",
                    )}
                  </div>
                  <div className="text-xs text-mid-gray">
                    {t(
                      "settings.advanced.learnedCorrections.aggressiveness.description",
                    )}
                  </div>
                  <Dropdown
                    className="w-48"
                    options={aggressivenessOptions}
                    selectedValue={aggressiveness}
                    onSelect={(value) =>
                      updateSetting(
                        "learn_corrections_aggressiveness",
                        value as Aggressiveness,
                      )
                    }
                    disabled={isUpdating("learn_corrections_aggressiveness")}
                  />
                  <div className="text-xs text-mid-gray">
                    {t(
                      `settings.advanced.learnedCorrections.aggressiveness.${aggressiveness}.description`,
                    )}
                  </div>
                </div>

                <div className="px-4 p-2 space-y-2">
                  <div className="text-sm font-semibold">
                    {t("settings.advanced.learnedCorrections.window.title")}
                  </div>
                  <div className="text-xs text-mid-gray">
                    {t(
                      "settings.advanced.learnedCorrections.window.description",
                    )}
                  </div>
                  <div className="flex items-center space-x-2">
                    <Input
                      type="number"
                      min="10"
                      max="300"
                      value={windowSecs}
                      onChange={handleWindowChange}
                      disabled={isUpdating("learn_corrections_window_secs")}
                      className="w-20"
                    />
                    <span className="text-sm text-text">
                      {t("settings.advanced.learnedCorrections.window.seconds")}
                    </span>
                  </div>
                </div>
              </>
            ) : (
              <div className="px-4 p-2">
                <div className="text-xs text-mid-gray">
                  {t("settings.advanced.learnedCorrections.platformHint")}
                </div>
              </div>
            )}

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
                  disabled={
                    !newMisheard.trim() || !newIntended.trim() || isBusy
                  }
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
                        {correction.lang && (
                          <span
                            className="shrink-0 text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded bg-mid-gray/15 text-mid-gray"
                            title={t(
                              "settings.advanced.learnedCorrections.langBadge",
                              { lang: correction.lang },
                            )}
                          >
                            {correction.lang}
                          </span>
                        )}
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
                        <RemoveIcon />
                      </Button>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </>
        )}
      </>
    );
  },
);
