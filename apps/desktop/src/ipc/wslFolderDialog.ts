import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

export async function pickWslFolder(initialPath: string): Promise<string | null> {
  const defaultPath = await invoke<string>("wsl_folder_dialog_initial_path", {
    path: initialPath,
  });
  const selected = await open({
    directory: true,
    multiple: false,
    defaultPath,
    title: "Choose WSL folder",
  });
  if (typeof selected !== "string") return null;
  return invoke<string>("wsl_folder_dialog_selection", {
    selectedPath: selected,
  });
}
