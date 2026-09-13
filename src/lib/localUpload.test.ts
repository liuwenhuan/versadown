import { describe, expect, it } from 'vitest';
import { uploadPathSegments, normalizeUploadSelection, type LocalUploadEntry } from './localUpload';

const file = (path: string): LocalUploadEntry => ({ path, name: path, kind: 'file' });
const dir = (path: string): LocalUploadEntry => ({ path, name: path, kind: 'directory' });

it('maps breadcrumb names to absolute Windows, UNC and POSIX directories', () => {
  expect(uploadPathSegments('C:\\Users\\deepin')).toEqual([
    { name: 'C:\\', path: 'C:\\' }, { name: 'Users', path: 'C:\\Users' }, { name: 'deepin', path: 'C:\\Users\\deepin' }
  ]);
  expect(uploadPathSegments('\\\\server\\share\\Docs')).toEqual([
    { name: '\\\\server\\share', path: '\\\\server\\share' }, { name: 'Docs', path: '\\\\server\\share\\Docs' }
  ]);
  expect(uploadPathSegments('/home/a\\b')).toEqual([
    { name: '/', path: '/' }, { name: 'home', path: '/home' }, { name: 'a\\b', path: '/home/a\\b' }
  ]);
  expect(uploadPathSegments('D:/Photos')).toEqual([{ name: 'D:/', path: 'D:/' }, { name: 'Photos', path: 'D:/Photos' }]);
  expect(uploadPathSegments('/')).toEqual([{ name: '/', path: '/' }]);
  expect(uploadPathSegments('')).toEqual([]);
});

describe('mixed upload selection', () => {
  it('keeps multiple files and directories and removes duplicates without reordering', () => {
    expect(normalizeUploadSelection([file('/a.txt'), dir('/Photos'), file('/b.txt'), dir('/Docs'), file('/a.txt')]).map(item => item.path))
      .toEqual(['/a.txt', '/Photos', '/b.txt', '/Docs']);
  });
  it('removes descendants of selected directories regardless of selection order, not sibling prefixes', () => {
    expect(normalizeUploadSelection([file('/Photos/a.jpg'), dir('/Photos/sub'), dir('/Photos'), file('/Photos-old/b.jpg')]).map(item => item.path))
      .toEqual(['/Photos', '/Photos-old/b.jpg']);
  });
  it('normalizes Windows separators, case, UNC and trailing separators for duplicate checks', () => {
    expect(normalizeUploadSelection([file('C:\\Photos\\a.jpg'), dir('c:/PHOTOS/'), file('C:/photos-old/a.jpg'), dir('\\\\server\\share\\Docs'), file('//SERVER/share/docs/a.txt')]).map(item => item.path))
      .toEqual(['c:/PHOTOS/', 'C:/photos-old/a.jpg', '\\\\server\\share\\Docs']);
  });
  it('preserves distinct POSIX case and does not mistake backslashes in filenames for separators', () => {
    expect(normalizeUploadSelection([dir('/Photos'), file('/photos/a.jpg'), file('/Photos\\a.jpg')]).map(item => item.path))
      .toEqual(['/Photos', '/photos/a.jpg', '/Photos\\a.jpg']);
  });
});
