import { Canvas } from "./components/Canvas";

// 0.1 nucleus shell: full-screen terminal canvas, no app chrome (per PRD §5.1).
export default function App() {
  return (
    <div className="h-full w-full bg-neutral-950 text-neutral-100">
      <Canvas />
    </div>
  );
}
