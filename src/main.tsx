import ReactDOM from 'react-dom/client';
import App from './App';

// E2e builds only (VITE_E2E=1): the wdio frontend shim for log forwarding and
// browser.tauri.execute. Statically false otherwise, so Rollup drops the
// branch and no test code reaches normal builds. Not awaited — first render
// must not wait on it (cold-open budget).
if (import.meta.env.VITE_E2E === '1') {
  void import('@wdio/tauri-plugin');
}

// No StrictMode: its double-effect dev behavior would double-register the
// imperative viewer/session wiring, and the flip path is deliberately
// framework-free anyway.
ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(<App />);
