export interface Point {
  x: number;
  y: number;
}

export function add(a: number, b: number): number {
  const sum = a + b; // ddbg:add
  return sum;
}

export function addPoints(p: Point, q: Point): Point {
  return { x: add(p.x, q.x), y: add(p.y, q.y) };
}
