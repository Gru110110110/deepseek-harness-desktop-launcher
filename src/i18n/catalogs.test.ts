import { describe, expect, it } from "vitest";
import en from "./en.json";
import zh from "./zh.json";

function leafKeys(value: unknown, prefix = ""): string[] {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return [prefix];
  }
  return Object.entries(value).flatMap(([key, child]) =>
    leafKeys(child, prefix ? `${prefix}.${key}` : key),
  );
}

describe("translation catalogs", () => {
  it("keep English and Simplified Chinese structurally identical", () => {
    expect(leafKeys(en).sort()).toEqual(leafKeys(zh).sort());
  });
});
