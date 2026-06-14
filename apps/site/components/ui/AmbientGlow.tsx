// Soft animated color glows so the page is never one flat color.
// Pure CSS — no JS cost. Used behind the whole page.
export default function AmbientGlow() {
  return (
    <div
      aria-hidden
      className="pointer-events-none fixed inset-0 z-0 overflow-hidden"
    >
      <div className="absolute -left-[10%] top-[8%] h-[40rem] w-[40rem] rounded-full bg-cyan-500/[0.07] blur-[150px] animate-float" />
      <div
        className="absolute -right-[12%] top-[35%] h-[38rem] w-[38rem] rounded-full bg-blue-600/[0.08] blur-[150px] animate-float"
        style={{ animationDelay: "-2s" }}
      />
      <div
        className="absolute left-[12%] top-[62%] h-[34rem] w-[34rem] rounded-full bg-teal-500/[0.05] blur-[150px] animate-float"
        style={{ animationDelay: "-4s" }}
      />
      <div
        className="absolute right-[8%] top-[88%] h-[34rem] w-[34rem] rounded-full bg-indigo-600/[0.06] blur-[150px] animate-float"
        style={{ animationDelay: "-1s" }}
      />
    </div>
  );
}
