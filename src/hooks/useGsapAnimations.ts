import { useRef, useLayoutEffect, useCallback } from "react";
import gsap from "gsap";

/**
 * Animate a modal + backdrop on open/close.
 * Returns refs to attach to the backdrop and modal panel divs,
 * plus an `animateClose` helper that plays the reverse animation
 * and calls the provided callback when done.
 */
export function useModalAnimation(isOpen: boolean) {
  const backdropRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  // useLayoutEffect runs synchronously after DOM mutation but before browser paint,
  // preventing the flash of un-animated content.
  useLayoutEffect(() => {
    if (!isOpen) return;
    const backdrop = backdropRef.current;
    const panel = panelRef.current;
    if (!backdrop || !panel) return;

    gsap.fromTo(backdrop, { opacity: 0 }, { opacity: 1, duration: 0.2, ease: "power2.out" });
    gsap.fromTo(
      panel,
      { scale: 0.92, opacity: 0 },
      { scale: 1, opacity: 1, duration: 0.25, ease: "power2.out" },
    );
  }, [isOpen]);

  const animateClose = useCallback((onComplete: () => void) => {
    const backdrop = backdropRef.current;
    const panel = panelRef.current;
    if (!backdrop || !panel) {
      onComplete();
      return;
    }
    const tl = gsap.timeline({ onComplete });
    tl.to(panel, { scale: 0.92, opacity: 0, duration: 0.18, ease: "power2.in" }, 0);
    tl.to(backdrop, { opacity: 0, duration: 0.18, ease: "power2.in" }, 0);
  }, []);

  return { backdropRef, panelRef, animateClose } as const;
}

/**
 * Animate a search panel appearing / disappearing.
 * Returns a ref for the panel and an `animateClose` helper.
 */
export function useSearchPanelAnimation(isOpen: boolean) {
  const panelRef = useRef<HTMLDivElement>(null);
  const backdropRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (!isOpen) return;
    const panel = panelRef.current;
    const backdrop = backdropRef.current;
    if (!panel) return;

    gsap.fromTo(
      panel,
      { scale: 0.95, opacity: 0 },
      { scale: 1, opacity: 1, duration: 0.22, ease: "power2.out" },
    );
    if (backdrop) {
      gsap.fromTo(backdrop, { opacity: 0 }, { opacity: 1, duration: 0.18, ease: "power2.out" });
    }
  }, [isOpen]);

  const animateClose = useCallback((onComplete: () => void) => {
    const panel = panelRef.current;
    const backdrop = backdropRef.current;
    if (!panel) {
      onComplete();
      return;
    }
    const tl = gsap.timeline({ onComplete });
    tl.to(panel, { scale: 0.95, opacity: 0, duration: 0.15, ease: "power2.in" }, 0);
    if (backdrop) {
      tl.to(backdrop, { opacity: 0, duration: 0.15, ease: "power2.in" }, 0);
    }
  }, []);

  return { panelRef, backdropRef, animateClose } as const;
}
