// Форматирование чисел для интерфейса.

const UNITS = ["Б", "КБ", "МБ", "ГБ", "ТБ"];

export function bytes(n: number, digits = 1): string {
  let i = 0;
  while (n >= 1024 && i < UNITS.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i === 0 ? 0 : digits)} ${UNITS[i]}`;
}

export const rate = (n: number) => `${bytes(n)}/с`;

export const percent = (n: number, digits = 0) => `${n.toFixed(digits)}%`;

export function duration(totalSecs: number): string {
  const d = Math.floor(totalSecs / 86400);
  const h = Math.floor((totalSecs % 86400) / 3600);
  const m = Math.floor((totalSecs % 3600) / 60);
  if (d > 0) return `${d} д ${h} ч`;
  if (h > 0) return `${h} ч ${m} мин`;
  return `${m} мин`;
}

export function minutes(total: number): string {
  const h = Math.floor(total / 60);
  const m = total % 60;
  return h > 0 ? `${h} ч ${m} мин` : `${m} мин`;
}

export function escape(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}
