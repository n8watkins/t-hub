import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

// Note: React.StrictMode is intentionally omitted. Its double-invoke of effects
// in development breaks xterm.js terminals (double `open()` / disposed addons).

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <App />,
);
