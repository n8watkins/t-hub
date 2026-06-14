import { useState } from "react";
import { Canvas } from "./components/Canvas";
import { Sidebar } from "./components/Sidebar";
import { Titlebar } from "./components/Titlebar";

// 0.5 shell: the 0.1 terminal canvas plus the read-only supervision sidebar
// (Workstream F). The sidebar is collapsible (Ctrl/Cmd+B, handled in Canvas) so
// the canvas can still go full-screen like the 0.1 nucleus. Selecting a session
// surfaces it for now; tab/tile focus routing lands with workspace tabs.
//
// The OS window is frameless (decorations:false); <Titlebar/> is the only window
// chrome and auto-hides at the top edge to reclaim vertical real estate
// (PRD §5.3). It is a fixed overlay, so it does not consume layout height.
export default function App() {
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [, setSelectedSession] = useState<string | null>(null);

  return (
    <div className="flex h-full w-full bg-neutral-950 text-neutral-100">
      <Titlebar />
      {sidebarOpen && <Sidebar onSelectSession={setSelectedSession} />}
      <div className="relative min-w-0 flex-1">
        <Canvas onToggleSidebar={() => setSidebarOpen((v) => !v)} />
      </div>
    </div>
  );
}
