import { used } from './lib';

test('partial coverage', () => {
  expect(used()).toBe(1);
});
