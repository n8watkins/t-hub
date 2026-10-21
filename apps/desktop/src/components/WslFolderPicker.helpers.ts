

export function normalizePosixPath(path: string): string | null {
  const trimmed = path.trim();
  if (!trimmed.startsWith("/") || trimmed.startsWith("//") || trimmed.includes("\\")) return null;
  const parts = trimmed.split("/").filter((part) => part && part !== ".");
  const normalized: string[] = [];
  for (const part of parts) {
    if (part === "..") return null;
    normalized.push(part);
  }
  return `/${normalized.join("/")}`;
}

export function parentPath(path: string): string | null {
  const normalized = normalizePosixPath(path);
  if (!normalized || normalized === "/") return null;
  const splitAt = normalized.lastIndexOf("/");
  return splitAt === 0 ? "/" : normalized.slice(0, splitAt);
}

export function pathBreadcrumbs(path: string): Array<{ label: string; path: string }> {
  const normalized = normalizePosixPath(path) ?? "/";
  const parts = normalized.split("/").filter(Boolean);
  return [
    { label: "/", path: "/" },
    ...parts.map((part, index) => ({
      label: part,
      path: `/${parts.slice(0, index + 1).join("/")}`,
    })),
  ];
}
