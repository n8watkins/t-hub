

export type FileIconThemeId = "lucide" | "vscode" | "seti";

/** The selectable icon themes, in picker order. */
export const FILE_ICON_THEMES: { id: FileIconThemeId; label: string }[] = [
  { id: "lucide", label: "Lucide (minimal)" },
  { id: "vscode", label: "VS Code (colorful)" },
  { id: "seti", label: "Seti (muted)" },
];
