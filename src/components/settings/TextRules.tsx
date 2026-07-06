import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useSettings } from "../../hooks/useSettings";
import { commands } from "@/bindings";
import type { SpacingPolicy, TextRule } from "@/bindings";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Dropdown } from "../ui/Dropdown";
import type { DropdownOption } from "../ui/Dropdown";

interface TextRulesProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const SPACING_POLICIES: SpacingPolicy[] = [
  "attach_left",
  "attach_right",
  "glue",
  "standalone",
];

// Whitespace replacements need a readable representation in the list.
const displayReplacement = (replacement: string): string => {
  if (replacement === "\n") return "\\n";
  if (replacement === "\n\n") return "\\n\\n";
  return replacement;
};

export const TextRules: React.FC<TextRulesProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("text_rules_enabled") || false;
    const itnEnabled = getSetting("text_rules_itn_enabled") || false;
    const customRules = getSetting("text_rules_custom") || [];
    const disabledBuiltins = getSetting("text_rules_disabled_builtins") || [];

    const [builtins, setBuiltins] = useState<TextRule[]>([]);
    const [newTrigger, setNewTrigger] = useState("");
    const [newReplacement, setNewReplacement] = useState("");
    const [newSpacing, setNewSpacing] = useState<SpacingPolicy>("glue");

    useEffect(() => {
      commands.getTextRulesBuiltins().then(setBuiltins);
    }, []);

    const spacingLabel = (spacing: SpacingPolicy): string =>
      t(`settings.advanced.textRules.spacing.${spacing}`);

    const spacingOptions: DropdownOption[] = SPACING_POLICIES.map((policy) => ({
      value: policy,
      label: spacingLabel(policy),
    }));

    const isBuiltinEnabled = (trigger: string): boolean =>
      !disabledBuiltins.some(
        (entry) => entry.toLowerCase() === trigger.toLowerCase(),
      );

    const handleToggleBuiltin = (trigger: string, nextEnabled: boolean) => {
      const withoutTrigger = disabledBuiltins.filter(
        (entry) => entry.toLowerCase() !== trigger.toLowerCase(),
      );
      updateSetting(
        "text_rules_disabled_builtins",
        nextEnabled ? withoutTrigger : [...withoutTrigger, trigger],
      );
    };

    const handleAddRule = () => {
      const trigger = newTrigger.trim();
      const replacement = newReplacement;
      if (!trigger || !replacement) {
        return;
      }
      if (
        customRules.some(
          (r) => r.trigger.toLowerCase() === trigger.toLowerCase(),
        )
      ) {
        toast.error(
          t("settings.advanced.textRules.custom.duplicate", { trigger }),
        );
        return;
      }
      updateSetting("text_rules_custom", [
        ...customRules,
        { trigger, replacement, spacing: newSpacing },
      ]);
      setNewTrigger("");
      setNewReplacement("");
      setNewSpacing("glue");
    };

    const handleRemoveRule = (index: number) => {
      updateSetting(
        "text_rules_custom",
        customRules.filter((_, i) => i !== index),
      );
    };

    const handleKeyPress = (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        handleAddRule();
      }
    };

    return (
      <>
        <ToggleSwitch
          checked={enabled}
          onChange={(checked) => updateSetting("text_rules_enabled", checked)}
          isUpdating={isUpdating("text_rules_enabled")}
          label={t("settings.advanced.textRules.title")}
          description={t("settings.advanced.textRules.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />

        {enabled && (
          <>
            <ToggleSwitch
              checked={itnEnabled}
              onChange={(checked) =>
                updateSetting("text_rules_itn_enabled", checked)
              }
              isUpdating={isUpdating("text_rules_itn_enabled")}
              label={t("settings.advanced.textRules.itn.title")}
              description={t("settings.advanced.textRules.itn.description")}
              descriptionMode={descriptionMode}
              grouped={grouped}
            />

            <div className="px-4 p-2 space-y-2">
              <div className="text-sm font-semibold">
                {t("settings.advanced.textRules.builtins.title")}
              </div>
              <div className="text-xs text-mid-gray">
                {t("settings.advanced.textRules.builtins.description")}
              </div>
              <div className="flex flex-col gap-1">
                {builtins.map((rule) => (
                  <label
                    key={rule.trigger}
                    className="flex items-center gap-2 text-sm cursor-pointer"
                  >
                    <input
                      type="checkbox"
                      checked={isBuiltinEnabled(rule.trigger)}
                      disabled={isUpdating("text_rules_disabled_builtins")}
                      onChange={(e) =>
                        handleToggleBuiltin(rule.trigger, e.target.checked)
                      }
                    />
                    <span>
                      {t("settings.advanced.textRules.mapping", {
                        trigger: rule.trigger,
                        replacement: displayReplacement(rule.replacement),
                      })}
                    </span>
                  </label>
                ))}
              </div>
            </div>

            <div className="px-4 p-2 space-y-2">
              <div className="text-sm font-semibold">
                {t("settings.advanced.textRules.custom.title")}
              </div>
              <div className="text-xs text-mid-gray">
                {t("settings.advanced.textRules.custom.description")}
              </div>
              <div className="flex flex-wrap items-center gap-2">
                <Input
                  type="text"
                  className="max-w-40"
                  value={newTrigger}
                  onChange={(e) => setNewTrigger(e.target.value)}
                  onKeyDown={handleKeyPress}
                  placeholder={t(
                    "settings.advanced.textRules.custom.triggerPlaceholder",
                  )}
                  variant="compact"
                  disabled={isUpdating("text_rules_custom")}
                />
                <Input
                  type="text"
                  className="max-w-24"
                  value={newReplacement}
                  onChange={(e) => setNewReplacement(e.target.value)}
                  onKeyDown={handleKeyPress}
                  placeholder={t(
                    "settings.advanced.textRules.custom.replacementPlaceholder",
                  )}
                  variant="compact"
                  disabled={isUpdating("text_rules_custom")}
                />
                <Dropdown
                  className="w-40"
                  options={spacingOptions}
                  selectedValue={newSpacing}
                  onSelect={(value) => setNewSpacing(value as SpacingPolicy)}
                  disabled={isUpdating("text_rules_custom")}
                />
                <Button
                  onClick={handleAddRule}
                  disabled={
                    !newTrigger.trim() ||
                    !newReplacement ||
                    isUpdating("text_rules_custom")
                  }
                  variant="primary"
                  size="md"
                >
                  {t("settings.advanced.textRules.custom.add")}
                </Button>
              </div>
              {customRules.length > 0 && (
                <div className="flex flex-col gap-1">
                  {customRules.map((rule, index) => (
                    <div
                      key={`${rule.trigger}-${index}`}
                      className="flex items-center justify-between gap-2 text-sm"
                    >
                      <span>
                        {t("settings.advanced.textRules.custom.rule", {
                          trigger: rule.trigger,
                          replacement: displayReplacement(rule.replacement),
                          spacing: spacingLabel(rule.spacing || "glue"),
                        })}
                      </span>
                      <Button
                        onClick={() => handleRemoveRule(index)}
                        disabled={isUpdating("text_rules_custom")}
                        variant="danger-ghost"
                        size="sm"
                        aria-label={t(
                          "settings.advanced.textRules.custom.remove",
                          { trigger: rule.trigger },
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
          </>
        )}
      </>
    );
  },
);
