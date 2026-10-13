import { add } from "./math.ts";

const values = [1, 2, 3];
let total = 0;
for (const v of values) {
  total = add(total, v); // ddbg:loop
}
console.log(`total = ${total}`);
