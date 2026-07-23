// Preset / share block, pinned at the top of the Theme section (extracted
// verbatim from ThemeEditor.tsx). Switches presets, saves the current theme as
// a named preset, and imports/exports themes as shareable JSON — themes are
// portable text, like VS Code. Every action is bound straight to `useTheme`.
import { useRef, useState } from "react";
import { Btn, Group, Opt, Row, ThemeSelect } from "../settingRows";
import { useTheme, BUILTIN_PRESETS } from "../../store/theme";
import { IconBtn } from "./themeRows";
import { SaveIcon, ExportIcon, ImportIcon, ResetIcon } from "./themeIcons";

/** Presets / share — pinned at the top of the Theme section. */
export function PresetGroup() {
  const active = useTheme((s) => s.active);
  const presets = useTheme((s) => s.presets);
  const applyPreset = useTheme((s) => s.applyPreset);
  const saveAsPreset = useTheme((s) => s.saveAsPreset);
  const deletePreset = useTheme((s) => s.deletePreset);
  const exportJSON = useTheme((s) => s.exportJSON);
  const importJSON = useTheme((s) => s.importJSON);
  const resetToDefault = useTheme((s) => s.resetToDefault);

  const presetNames = [
    ...BUILTIN_PRESETS.map((p) => p.name),
    ...Object.keys(presets),
  ];
  const isUserPreset = (name: string) =>
    Object.prototype.hasOwnProperty.call(presets, name);

  return (
    <Group title="Preset">
      <Row label="Active">
        <ThemeSelect
          value={presetNames.includes(active.name) ? active.name : ""}
          onChange={(v) => applyPreset(v)}
          title="Switch to a built-in or saved preset"
        >
          {!presetNames.includes(active.name) && (
            <Opt value="">{active.name} (edited)</Opt>
          )}
          {presetNames.map((n) => (
            <Opt key={n} value={n}>
              {n}
              {isUserPreset(n) ? " ·" : ""}
            </Opt>
          ))}
        </ThemeSelect>
      </Row>
      <PresetActions
        activeName={active.name}
        canDelete={isUserPreset(active.name)}
        onSave={saveAsPreset}
        onDelete={() => deletePreset(active.name)}
        onExport={exportJSON}
        onImport={importJSON}
        onReset={resetToDefault}
      />
    </Group>
  );
}

// ---------------------------------------------------------------------------
// Preset actions (save / delete / import / export / reset).
// ---------------------------------------------------------------------------
function PresetActions({
  activeName,
  canDelete,
  onSave,
  onDelete,
  onExport,
  onImport,
  onReset,
}: {
  activeName: string;
  canDelete: boolean;
  onSave: (name: string) => void;
  onDelete: () => void;
  onExport: () => string;
  onImport: (json: string) => string | null;
  onReset: () => void;
}) {
  const [name, setName] = useState(activeName);
  const [msg, setMsg] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);

  const flash = (m: string) => {
    setMsg(m);
    window.setTimeout(() => setMsg(null), 2000);
  };

  const doExport = async () => {
    const json = onExport();
    try {
      await navigator.clipboard.writeText(json);
      flash("Copied JSON to clipboard");
    } catch {
      // Clipboard blocked — fall back to a download so export still works.
      const blob = new Blob([json], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `${activeName || "theme"}.json`;
      a.click();
      URL.revokeObjectURL(url);
      flash("Downloaded theme.json");
    }
  };

  const onFile = async (file: File) => {
    const text = await file.text();
    const err = onImport(text);
    flash(err ? `Import failed: ${err}` : "Imported");
  };

  return (
    <div className="mt-1 flex flex-col gap-2">
      <div className="flex items-center gap-1.5">
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Preset name"
          className="min-w-0 flex-1 rounded border bg-transparent px-2 py-1.5 text-sm"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
        />
        {/* Compact icon actions (same behavior as the old text buttons). */}
        <IconBtn onClick={() => onSave(name)} title="Save the current theme as a named preset" label="Save preset">
          <SaveIcon />
        </IconBtn>
        <IconBtn onClick={doExport} title="Export: copy the active theme as JSON to the clipboard" label="Export theme">
          <ExportIcon />
        </IconBtn>
        <IconBtn onClick={() => fileRef.current?.click()} title="Import a theme from a JSON file" label="Import theme">
          <ImportIcon />
        </IconBtn>
        <IconBtn onClick={onReset} title="Reset to the Midnight default" label="Reset theme">
          <ResetIcon />
        </IconBtn>
        {canDelete && (
          <Btn onClick={onDelete} title="Delete this user preset">
            Delete
          </Btn>
        )}
        <input
          ref={fileRef}
          type="file"
          accept="application/json,.json"
          className="hidden"
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) void onFile(f);
            e.target.value = "";
          }}
        />
      </div>
      {msg && (
        <div className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
          {msg}
        </div>
      )}
    </div>
  );
}
