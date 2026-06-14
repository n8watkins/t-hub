import Navbar from "@/components/Navbar";
import Footer from "@/components/Footer";
import AmbientGlow from "@/components/ui/AmbientGlow";
import ScrollProgress from "@/components/ui/ScrollProgress";
import Hero from "@/components/sections/Hero";
import Features from "@/components/sections/Features";
import Showcase from "@/components/sections/Showcase";
import HowItWorks from "@/components/sections/HowItWorks";
import Why from "@/components/sections/Why";
import Stack from "@/components/sections/Stack";
import CTA from "@/components/sections/CTA";

export default function Home() {
  return (
    <>
      <ScrollProgress />
      <Navbar />
      <AmbientGlow />
      <main
        id="main"
        className="relative z-10 bg-gradient-to-b from-ink-900 via-ink-800 to-ink-900"
      >
        <Hero />
        <Features />
        <Showcase />
        <HowItWorks />
        <Why />
        <Stack />
        <CTA />
        <Footer />
      </main>
    </>
  );
}
