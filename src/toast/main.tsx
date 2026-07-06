import React from "react";
import ReactDOM from "react-dom/client";
import LearnedToast from "./LearnedToast";
import "@/i18n";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <LearnedToast />
  </React.StrictMode>,
);
