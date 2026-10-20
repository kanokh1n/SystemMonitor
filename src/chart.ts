// Мини‑график (sparkline) на canvas: хранит последние N точек и рисует линию с заливкой.

export class History {
  readonly values: number[] = [];
  constructor(private readonly size = 90) {}

  push(v: number) {
    this.values.push(v);
    if (this.values.length > this.size) this.values.shift();
  }
}

interface Series {
  data: number[];
  color: string;
}

/** Рисует одну или несколько линий. `max` — верх шкалы (для процентов 100). */
export function sparkline(canvas: HTMLCanvasElement, series: Series[], max?: number, size = 90) {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth;
  const h = canvas.clientHeight;
  if (w === 0 || h === 0) return;
  if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
    canvas.width = w * dpr;
    canvas.height = h * dpr;
  }
  const ctx = canvas.getContext("2d")!;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const top = max ?? Math.max(1, ...series.flatMap((s) => s.data)) * 1.15;
  const step = w / (size - 1);

  for (const { data, color } of series) {
    if (data.length < 2) continue;
    const x0 = w - (data.length - 1) * step;
    const y = (v: number) => h - 2 - (Math.min(v, top) / top) * (h - 4);

    ctx.beginPath();
    data.forEach((v, i) => (i === 0 ? ctx.moveTo(x0, y(v)) : ctx.lineTo(x0 + i * step, y(v))));
    ctx.strokeStyle = color;
    ctx.lineWidth = 1.75;
    ctx.lineJoin = "round";
    ctx.stroke();

    ctx.lineTo(w, h);
    ctx.lineTo(x0, h);
    ctx.closePath();
    const grad = ctx.createLinearGradient(0, 0, 0, h);
    grad.addColorStop(0, color + "55");
    grad.addColorStop(1, color + "00");
    ctx.fillStyle = grad;
    ctx.fill();
  }
}
