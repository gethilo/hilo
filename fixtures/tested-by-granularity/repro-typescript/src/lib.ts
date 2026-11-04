import { helper } from './helper';

// The referenced function (see lib.test.ts).
export function used(): number {
  return helper();
}

// The unreferenced function.
export function neverCalled(): number {
  return 2;
}
