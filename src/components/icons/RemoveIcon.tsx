import React from "react";

interface RemoveIconProps {
  className?: string;
}

// The small "×" glyph shared by the fork's list rows (TextRules,
// LearnedCorrections) for their remove buttons.
const RemoveIcon: React.FC<RemoveIconProps> = ({ className = "w-3 h-3" }) => {
  return (
    <svg
      className={className}
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
  );
};

export default RemoveIcon;
