export type Translate = (
  key: string,
  options?: Record<string, unknown>,
) => string;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function presentError(error: unknown, translate: Translate): string {
  if (!isRecord(error) || typeof error.code !== "string") {
    return translate("error.unknown");
  }
  const key = `error.${error.code}`;
  const values = isRecord(error.values) ? error.values : {};
  const detail =
    typeof error.safeDetail === "string" ? error.safeDetail : undefined;
  const translated = translate(key, { ...values, detail });
  return translated === key
    ? (detail ?? translate("error.unknown"))
    : translated;
}
