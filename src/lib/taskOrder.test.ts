import { applyStoredTaskOrder, mergeVisibleTaskOrder, reorderTaskIds } from './taskOrder';

it('moves selected tasks as one block while preserving their relative order', () => {
  expect(reorderTaskIds(['one', 'two', 'three', 'four'], ['one', 'two'], 'four', true))
    .toEqual(['three', 'four', 'one', 'two']);
});

it('keeps hidden tasks in place when only visible tasks are reordered', () => {
  expect(mergeVisibleTaskOrder(
    ['one', 'hidden', 'two', 'three'],
    ['one', 'two', 'three'],
    ['three', 'one', 'two'],
  )).toEqual(['three', 'hidden', 'one', 'two']);
});

it('puts newly discovered tasks before a persisted custom order', () => {
  const tasks = [{ id: 'new' }, { id: 'one' }, { id: 'two' }];
  expect(applyStoredTaskOrder(tasks, ['two', 'one']).map((task) => task.id))
    .toEqual(['new', 'two', 'one']);
});
