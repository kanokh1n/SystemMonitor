// Карта диска (treemap): каждая папка — прямоугольник, площадь пропорциональна размеру.
// Раскладка «squarified»: прямоугольники стараются быть ближе к квадратам, чтобы их было легко сравнивать.

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
  /** Индекс значения во входном массиве. */
  i: number;
}

/** Худшее соотношение сторон в ряду, если положить его вдоль стороны `side`. */
function worst(row: number[], side: number) {
  const sum = row.reduce((a, b) => a + b, 0);
  const max = Math.max(...row);
  const min = Math.min(...row);
  return Math.max((side * side * max) / (sum * sum), (sum * sum) / (side * side * min));
}

/** `values` должны быть отсортированы по убыванию и больше нуля. */
export function squarify(values: number[], width: number, height: number): Rect[] {
  const total = values.reduce((a, b) => a + b, 0);
  if (!total || width <= 0 || height <= 0) return [];
  const areas = values.map((v) => (v / total) * width * height);
  const rects: Rect[] = [];
  let box = { x: 0, y: 0, w: width, h: height };
  let i = 0;
  while (i < areas.length) {
    const side = Math.min(box.w, box.h);
    const row = [areas[i]];
    let j = i + 1;
    while (j < areas.length && worst([...row, areas[j]], side) <= worst(row, side)) row.push(areas[j++]);
    const sum = row.reduce((a, b) => a + b, 0);
    if (box.w >= box.h) {
      // Ряд ставим столбцом слева.
      const cw = sum / box.h;
      let y = box.y;
      row.forEach((a, k) => {
        rects.push({ x: box.x, y, w: cw, h: a / cw, i: i + k });
        y += a / cw;
      });
      box = { x: box.x + cw, y: box.y, w: box.w - cw, h: box.h };
    } else {
      // Ряд ставим строкой сверху.
      const rh = sum / box.w;
      let x = box.x;
      row.forEach((a, k) => {
        rects.push({ x, y: box.y, w: a / rh, h: rh, i: i + k });
        x += a / rh;
      });
      box = { x: box.x, y: box.y + rh, w: box.w, h: box.h - rh };
    }
    i = j;
  }
  return rects;
}
