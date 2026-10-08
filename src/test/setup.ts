import '@testing-library/jest-dom/vitest';

// The UI follows the system language, and jsdom reports en-US. Component
// tests are written against the default zh-CN copy, so pin the language here
// instead of letting each suite depend on the host environment.
Object.defineProperty(window.navigator, 'languages', { configurable: true, get: () => ['zh-CN'] });
Object.defineProperty(window.navigator, 'language', { configurable: true, get: () => 'zh-CN' });
