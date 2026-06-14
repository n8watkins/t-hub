import { useState } from "react";
import { Canvas } from "./components/Canvas";
import { Sidebar } from "./components/Sidebar";
import { Titlebar } from "./components/Titlebar";

// 0.5 shell: a persistent Chrome-style top bar, then the body row (the read-only
// supervision sidebar + the terminal canvas). The sidebar is collapsible
// (Ctrl/Cmd+B, handled in Canvas) so the canvas can still go full-width like the
// 0.1 nucleus. Selecting a session surfaces it for now; tab/tile focus routing
// lands with workspace tabs.
//
// The OS window is frameless (decorations:false); <Titlebar/> is the only window
// chrome. Unlike the 0.1 auto-hide bar, it is ALWAYS visible (~32px) and a real
// layout row — it hosts the workspace tab strip + window controls, with
// data-tauri-drag-region zones beside the tabs so the window can be moved like
// Chrome. The body row takes the remaining height.
export default function App() {
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [, setSelectedSession] = useState<string | null>(null);

  return (
    <div className="flex h-full w-full flex-col bg-neutral-950 text-neutral-100">
      <Titlebar />
      <div className="flex min-h-0 flex-1">
        {sidebarOpen && <Sidebar onSelectSession={setSelectedSession} />}
        <div className="relative min-w-0 flex-1">
          <Canvas onToggleSidebar={() => setSidebarOpen((v) => !v)} />
        </div>
      </div>
    </div>
  );
}
