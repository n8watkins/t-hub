import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

interface TauriConfig {
  identifier?: string;
  mainBinaryName?: string;
  productName?: string;
}

interface TauriConfigSchema {
  properties?: {
    mainBinaryName?: {
      type?: string[];
    };
  };
}

const tauriDir = resolve(process.cwd(), "src-tauri");

function readJsonConfig(name: string): TauriConfig {
  return JSON.parse(readFileSync(resolve(tauriDir, name), "utf8")) as TauriConfig;
}

function cargoPackageName(): string {
  const manifest = readFileSync(resolve(tauriDir, "Cargo.toml"), "utf8");
  const match = /^name\s*=\s*"([^"]+)"/m.exec(manifest);
  if (!match) throw new Error("Cargo package name is missing");
  return match[1];
}

describe("Tauri build variant configuration", () => {
  it("resolves distinct production and development main binaries", () => {
    const production = readJsonConfig("tauri.conf.json");
    const developmentOverlay = readJsonConfig("tauri.dev.conf.json");
    const development = { ...production, ...developmentOverlay };
    const cargoBinary = cargoPackageName();

    expect(production.mainBinaryName ?? cargoBinary).toBe("t-hub");
    expect(development.mainBinaryName ?? cargoBinary).toBe("t-hub-dev");
    expect(development.mainBinaryName).not.toBe(production.mainBinaryName ?? cargoBinary);
    expect(development.productName).toBe("T-Hub Dev");
    expect(development.identifier).toBe("com.t-hub.dev");
  });

  it("uses the root mainBinaryName field defined by the installed Tauri schema", () => {
    const schemaPath = resolve(process.cwd(), "node_modules/@tauri-apps/cli/config.schema.json");
    const schema = JSON.parse(readFileSync(schemaPath, "utf8")) as TauriConfigSchema;
    const development = readJsonConfig("tauri.dev.conf.json");

    expect(schema.properties?.mainBinaryName?.type).toContain("string");
    expect(development.mainBinaryName).toBe("t-hub-dev");
    expect(development.mainBinaryName).not.toMatch(/\.exe$/i);
  });
});
