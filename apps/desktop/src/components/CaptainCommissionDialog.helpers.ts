

export function validateNewCodebaseDestination(parent: string, name: string): string {
  const base = parent.trim().replace(/\/+$/, "");
  const leaf = name.trim();
  if (!base.startsWith("/") || base.startsWith("//") || base.includes("\\")) {
    throw new Error("Parent must be an absolute WSL path.");
  }
  if (!leaf) throw new Error("Destination folder name is required.");
  // The control-character range is the POINT of this guard: a folder name
  // carrying NUL/C0/DEL is exactly what must be rejected before it reaches a
  // shell or the filesystem.
  // eslint-disable-next-line no-control-regex
  if (leaf === "." || leaf === ".." || /[\\/\u0000-\u001f\u007f]/.test(leaf)) {
    throw new Error("Destination folder name must be one safe folder name.");
  }
  return `${base}/${leaf}`;
}
