// Ambient types for the untyped `vscode-icons-js` package (a filename → icon-name
// mapper) and the offline Iconify collection we lazy-load for the vscode theme.
// Declaring the JSON module here also keeps tsc from trying to infer the type of
// a 3.6MB icon file.

declare module "vscode-icons-js" {
  export function getIconForFile(fileName: string): string | undefined;
  export function getIconForFolder(folderName: string): string | undefined;
  export function getIconForOpenFolder(folderName: string): string | undefined;
  const vscodeIcons: {
    getIconForFile: typeof getIconForFile;
    getIconForFolder: typeof getIconForFolder;
    getIconForOpenFolder: typeof getIconForOpenFolder;
  };
  export default vscodeIcons;
}

declare module "@iconify-json/vscode-icons/icons.json" {
  const data: {
    prefix: string;
    icons: Record<string, unknown>;
    aliases?: Record<string, unknown>;
    [k: string]: unknown;
  };
  export default data;
}
