export interface ParsedDocumentDate {
  year: number;
  month?: number;
  day?: number;
}

const MONTH_YEAR_FORMATTER = new Intl.DateTimeFormat("en", {
  month: "short",
  year: "numeric",
  timeZone: "UTC",
});

const MONTH_FORMATTER = new Intl.DateTimeFormat("en", {
  month: "short",
  timeZone: "UTC",
});

export function parseDocumentDate(
  value: string | null | undefined,
): ParsedDocumentDate | null {
  const trimmed = value?.trim();
  if (!trimmed) return null;

  let match = /^(\d{4})-(\d{2})(?:-(\d{2}))?$/.exec(trimmed);
  if (match) {
    return {
      year: Number(match[1]),
      month: Number(match[2]),
      day: match[3] ? Number(match[3]) : undefined,
    };
  }

  match = /^(\d{1,2})\/(\d{4})$/.exec(trimmed);
  if (match) {
    return { year: Number(match[2]), month: Number(match[1]) };
  }

  match = /^(\d{4})$/.exec(trimmed);
  if (match) return { year: Number(match[1]) };

  return null;
}

export function formatDocumentMonthYear(
  value: string | null | undefined,
): string | null {
  const parsed = parseDocumentDate(value);
  if (!parsed) return null;

  const { year, month } = parsed;
  if (month === undefined || month < 1 || month > 12) return String(year);

  const date = new Date(Date.UTC(year, month - 1, 1));
  if (Number.isNaN(date.getTime())) return null;

  return MONTH_YEAR_FORMATTER.format(date);
}

export function formatDocumentFullDate(
  value: string | null | undefined,
): string | null {
  const parsed = parseDocumentDate(value);
  if (!parsed) return null;

  const { year, month, day } = parsed;
  if (month === undefined || month < 1 || month > 12) return String(year);

  const date = new Date(Date.UTC(year, month - 1, 1));
  if (Number.isNaN(date.getTime())) return null;

  const monthLabel = MONTH_FORMATTER.format(date);
  return day && day >= 1 && day <= 31
    ? `${day} ${monthLabel} ${year}`
    : `${monthLabel} ${year}`;
}

export function formatTimestampFullDate(ms: number): string | null {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return null;

  const monthDate = new Date(Date.UTC(date.getFullYear(), date.getMonth(), 1));
  const monthLabel = MONTH_FORMATTER.format(monthDate);
  return `${date.getDate()} ${monthLabel} ${date.getFullYear()}`;
}
