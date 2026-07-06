# LLM-Cleanup-Prompt für Handys natives Post-Processing

> Deckt die ⚙️-Punkte aus [REQUIREMENTS.md](REQUIREMENTS.md) ab: Glättung, ITN-Fallback,
> gesprochene Satzzeichen (Übergangslösung, bis der deterministische Regel-Layer greift)
> und technisches Diktieren. Zum interaktiven A/B-Testen mit und ohne LLM gedacht.

## Setup in Handy

1. **Ollama:** `brew install ollama && ollama pull qwen3:4b-instruct-q4_K_M` (~3 GB)
2. Handy → Settings → **Post-Processing** aktivieren
3. Provider: **Custom (OpenAI-compatible)** → Base URL `http://localhost:11434/v1`, Model `qwen3:4b-instruct-q4_K_M`, API-Key beliebig (z. B. `ollama`)
4. Neuen Prompt anlegen, Inhalt von unten einfügen. Temperatur, falls einstellbar: **0.1**
5. Mac-Alternative mit 0 extra RAM: Provider **Apple Intelligence** wählen, gleicher Prompt

## Prompt (kopierfertig)

```text
Du bist ein Diktat-Nachbearbeiter. Korrigiere das folgende Transkript eines deutsch/englischen Diktats. Gib NUR den korrigierten Text zurück — keine Erklärungen, keine Anführungszeichen drumherum, keine Einleitung.

Regeln:
1. Entferne Versprecher, Füllwörter und aufgelöste Selbstkorrekturen (bei "X, nein, ich meine Y" bleibt nur Y).
2. Schreibe Zahlwörter als Ziffern: "fünfhundertneununddreißig" → "539", "zwei Komma fünf" → "2,5". Ausnahme: eigenständige Zahlen bis zwölf in normalem Fließtext bleiben Wörter.
3. Ersetze gesprochene Satzzeichen-Befehle durch die Zeichen — deutsch wie englisch: "Bindestrich"/"dash" → "-", "Punkt"/"dot" (als Befehl, nicht als Satzende) → ".", "Schrägstrich"/"slash" → "/", "Unterstrich"/"underscore" → "_", "Doppelpunkt" → ":", "Klammer auf/zu" → "(" ")", "neue Zeile"/"new line" → Zeilenumbruch, "neuer Absatz"/"new paragraph" → Leerzeile. Nur ersetzen, wenn es erkennbar als Befehl gemeint ist.
4. Technisches Diktieren: Setze Dateipfade, URLs, CLI-Befehle, Variablen- und Funktionsnamen korrekt zusammen. "src Schrägstrich components Schrägstrich settings" → "src/components/settings". "camel case user name" → "userName", "snake case user name" → "user_name". Bekannte Tool-Namen korrekt schreiben (git, kubectl, Tauri, React, TypeScript, Rust, cargo, bun).
5. Ansonsten: Wortlaut und Sprache(n) des Diktats beibehalten. Nichts hinzufügen, nichts zusammenfassen, Ton nicht verändern. Deutsch-englischer Mischtext bleibt gemischt.

Transkript:
${output}
```

## Testfälle fürs A/B

| Diktat (gesprochen) | Erwartet |
| --- | --- |
| "fünfhundertneununddreißig Euro" | `539 Euro` |
| "voice Bindestrich control" | `voice-control` |
| "src slash components slash settings" | `src/components/settings` |
| "erster Punkt neue Zeile zweiter Punkt" | `erster Punkt` ⏎ `zweiter Punkt` (Ambiguität — beobachten!) |
| "camel case use settings" | `useSettings` |
| "ähm also ich meine wir nehmen äh Variante zwei" | `Wir nehmen Variante zwei` |

Beobachtungen/Fehlschläge hier notieren; alles, was der LLM unzuverlässig macht,
wandert als deterministische Regel in den Text-Rules-Layer des Forks.
