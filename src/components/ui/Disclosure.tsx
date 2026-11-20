import React, { useId, useState } from "react";

interface DisclosureProps {
  title: string;
  /** Short state shown right of the title while collapsed or open, e.g. "27/29 active". */
  summary?: React.ReactNode;
  defaultOpen?: boolean;
  children: React.ReactNode;
}

/**
 * fork(voice-control): a collapsible settings row. The header matches a
 * `SettingContainer` row (title left, summary + chevron right). The panel adds
 * no horizontal padding: children own it, exactly like rows in a
 * `SettingsGroup` (`px-4`, or nested `SettingContainer`s with `grouped`).
 */
export const Disclosure: React.FC<DisclosureProps> = ({
  title,
  summary,
  defaultOpen = false,
  children,
}) => {
  const [isOpen, setIsOpen] = useState(defaultOpen);
  const panelId = useId();

  return (
    <div>
      <button
        type="button"
        aria-expanded={isOpen}
        aria-controls={panelId}
        onClick={() => setIsOpen(!isOpen)}
        className="w-full flex items-center justify-between gap-4 min-h-12 px-4 p-2 text-start cursor-pointer hover:bg-mid-gray/5 transition-colors duration-150 focus:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-logo-primary"
      >
        <span className="text-sm font-medium shrink-0">{title}</span>
        <span className="flex items-center gap-2 min-w-0 text-sm text-mid-gray">
          {summary !== undefined && <span className="truncate">{summary}</span>}
          <svg
            className={`w-4 h-4 shrink-0 transition-transform duration-200 ${
              isOpen ? "transform rotate-180" : ""
            }`}
            fill="none"
            stroke="currentColor"
            viewBox="0 0 24 24"
            aria-hidden="true"
          >
            <path
              strokeLinecap="round"
              strokeLinejoin="round"
              strokeWidth={2}
              d="M19 9l-7 7-7-7"
            />
          </svg>
        </span>
      </button>
      <div id={panelId} hidden={!isOpen} className="pb-2">
        {children}
      </div>
    </div>
  );
};
